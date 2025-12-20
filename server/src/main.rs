use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    Router,
    extract::{
        self, ConnectInfo, Query, WebSocketUpgrade,
        ws::{self, WebSocket},
    },
    response::{Html, IntoResponse, Redirect},
    routing::{any, get},
};
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use fxhash::hash32;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

use server::*;

type ClientTx = SplitSink<WebSocket, ws::Message>;
type ClientRx = SplitStream<WebSocket>;

#[derive(Debug, Clone)]
struct Client {
    id: Uuid,
    addr: SocketAddr,
    comm: Arc<Mutex<ClientTx>>,
}

impl Client {
    fn new(id: Uuid, addr: SocketAddr, comm: ClientTx) -> Self {
        Self {
            id: id.into(),
            addr,
            comm: Arc::new(Mutex::new(comm)),
        }
    }

    async fn send(&mut self, ws_msg: impl Into<ws::Message>) {
        let mut comm = self.comm.lock().await;
        if let Err(err) = comm.send(ws_msg.into()).await {
            error!("Error sending to client {}: {err}", self.id);
        }
    }

    async fn disconnect(&mut self, reason: DisconnectReason) {
        let mut comm = self.comm.lock().await;
        Self::disconnect_comm(&mut comm, reason).await
    }

    async fn disconnect_comm(
        comm: &mut SplitSink<WebSocket, ws::Message>,
        reason: DisconnectReason,
    ) {
        if let Err(err) = comm
            .send(MessageFromServer::Disconnect { reason }.into())
            .await
        {
            error!("Error sending to client: {err}");
        }
        if let Err(err) = comm.close().await {
            error!("Error closing connection for client: {err}");
        }
    }

    async fn ok(&mut self) {
        self.send(MessageFromServer::Ok).await
    }

    async fn bad_request(&mut self) {
        self.send(MessageFromServer::BadRequest).await
    }

    async fn client_not_found(&mut self) {
        self.send(MessageFromServer::ClientNotFound).await
    }

    async fn room_created(&mut self, room_id: u32) {
        self.send(MessageFromServer::RoomCreated { room_id }).await
    }

    async fn guest_joined(&mut self, guest_id: Uuid) {
        self.send(MessageFromServer::GuestJoined { guest_id }).await
    }

    async fn joined_room(&mut self, room: Room) {
        self.send(MessageFromServer::JoinedRoom { room }).await
    }

    async fn host_deleted_room(&mut self) {
        self.send(MessageFromServer::HostDeletedRoom).await
    }

    fn host_room_id(&self) -> u32 {
        hash32(&(&self.id, self.addr))
    }
}

#[derive(Clone)]
struct State {
    clients: Arc<Mutex<HashMap<Uuid, Client>>>,
    rooms: Arc<Mutex<HashMap<u32, Room>>>,
}

impl State {
    fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            rooms: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

const PORT: u16 = 3000;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = State::new();

    let app = Router::new()
        .route("/", get(handle_serve_html))
        .route("/index.html", get(handle_serve_html))
        .route("/ws", get(handle_ws))
        .route("/healthz", any(handle_healthz))
        .fallback(any(handle_catch_all))
        .with_state(state);

    info!("Server running on port {PORT}");
    let addr = format!("0.0.0.0:{PORT}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Can bind to port");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Can listen and serve");
}

async fn handle_serve_html() -> Html<&'static str> {
    Html(include_str!("../../index.html"))
}

async fn handle_healthz() -> &'static str {
    "service ok"
}

async fn handle_catch_all() -> impl IntoResponse {
    if let Ok(url) = env::var("CATCH_ALL_REDIRECT_URL") {
        Redirect::to(&url)
    } else {
        Redirect::to("/")
    }
}

#[derive(Deserialize)]
struct WsQuery {
    client_id: String,
}

async fn handle_ws(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<WsQuery>,
    extract::State(state): extract::State<State>,
) -> impl IntoResponse {
    ws.on_upgrade(async move |socket| {
        let client_id_str = query.client_id;
        info!("New ws connection from `{client_id_str}`");

        let (mut client_tx, client_rx) = socket.split();

        let client_id = match verify(&client_id_str, &state).await {
            Ok(id) => id,
            Err(reason) => {
                Client::disconnect_comm(&mut client_tx, reason).await;
                return;
            }
        };

        let client = Client::new(client_id, addr, client_tx);

        let mut clients = state.clients.lock().await;
        clients.insert(client_id, client.clone());

        let state = state.clone();

        _ = tokio::spawn(async move { ws_loop(client, client_rx, state).await });
    })
}

async fn verify(client_id_str: &str, state: &State) -> Result<Uuid, DisconnectReason> {
    let Ok(client_id) = Uuid::try_parse(client_id_str) else {
        return Err(DisconnectReason::InvalidClientId);
    };
    let clients = state.clients.lock().await;
    if clients.contains_key(&client_id) {
        return Err(DisconnectReason::ClientIdTaken);
    }
    Ok(client_id)
}

async fn ws_loop(mut client: Client, mut client_rx: ClientRx, state: State) {
    while let Some(Ok(ws_msg)) = client_rx.next().await {
        if let ws::Message::Close(_) = ws_msg {
            break;
        }

        let Ok(msg_from_client) = MessageFromClient::try_from(&ws_msg) else {
            warn!("Unknown message from client: {ws_msg:?}");
            client.bad_request().await;
            continue;
        };

        use MessageFromClient as M;
        match msg_from_client {
            M::StartGame => handle_start_game(&mut client, &state).await,
            M::GetRooms => handle_get_rooms(&mut client, &state).await,
            M::CreateRoom => handle_create_room(&mut client, &state).await,
            M::JoinRoom { room_id } => handle_join_room(room_id, &mut client, &state).await,
            M::DeleteRoom { room_id } => handle_delete_room(room_id, &mut client, &state).await,
            M::Offer { to_client_id, .. }
            | M::Answer { to_client_id, .. }
            | M::IceCandidate { to_client_id, .. } => {
                forward_to_client(to_client_id, ws_msg, &mut client, &state).await
            }
        }
    }

    handle_close_connection(&client, &state).await;
}

async fn handle_close_connection(client: &Client, state: &State) {
    // TODO: remove clients from rooms they were hosting or guests of

    // TODO: close connections of each client.comm?

    let mut clients = state.clients.lock().await;
    if clients.remove(&client.id).is_none() {
        warn!(
            "Client {} disconnected, but was not in the clients map",
            client.id
        );
    } else {
        info!("Client {} disconnected", client.id);
    }
}

async fn handle_start_game(client: &mut Client, state: &State) {
    let mut rooms = state.rooms.lock().await;

    let room_id = client.host_room_id();
    let Some(room) = rooms.get(&room_id) else {
        client.bad_request().await;
        return;
    };

    client.disconnect(DisconnectReason::HostStartedGame).await;

    let mut clients = state.clients.lock().await;
    for guest_id in &room.guest_ids {
        if let Some(guest_client) = clients.get_mut(guest_id) {
            guest_client
                .disconnect(DisconnectReason::HostStartedGame)
                .await;
        } else {
            warn!("Guest id {guest_id} is present in room {room_id}, but is not in clients");
        }
    }

    rooms.remove(&room_id);
}

async fn handle_get_rooms(client: &mut Client, state: &State) {
    let rooms = state.rooms.lock().await;
    let rooms = rooms.iter().map(|(_, room)| room.clone()).collect();
    client.send(MessageFromServer::Rooms { rooms }).await;
}

async fn handle_create_room(client: &mut Client, state: &State) {
    let mut rooms = state.rooms.lock().await;
    let room_id = client.host_room_id();

    if rooms.contains_key(&room_id) {
        client.bad_request().await;
        return;
    }

    rooms.insert(
        room_id,
        Room {
            id: room_id,
            host_id: client.id,
            guest_ids: HashSet::new(),
        },
    );

    info!("Room {room_id} created by client {}", client.id);

    client.room_created(room_id).await;
}

async fn handle_join_room(room_id: u32, client: &mut Client, state: &State) {
    let mut clients = state.clients.lock().await;
    let mut rooms = state.rooms.lock().await;

    let Some(room) = rooms.get_mut(&room_id) else {
        client.bad_request().await;
        return;
    };
    if room.host_id == client.id {
        client.bad_request().await;
        return;
    }

    let Some(host) = clients.get_mut(&room.host_id) else {
        warn!(
            "Correcting illegal state: room {room_id} exists with host {} not in clients",
            room.host_id
        );
        client.bad_request().await;
        return;
    };

    room.guest_ids.insert(client.id);
    client.joined_room(room.clone()).await;

    host.guest_joined(client.id).await;

    for guest_id in &room.guest_ids {
        if guest_id != &client.id {
            if let Some(guest) = clients.get_mut(guest_id) {
                guest.guest_joined(client.id).await;
            } else {
                warn!(
                    "Room {} contains guest id {guest_id}, but there is no client for them",
                    room.id
                );
            };
        }
    }
}

async fn handle_delete_room(room_id: u32, client: &mut Client, state: &State) {
    let mut clients = state.clients.lock().await;
    let mut rooms = state.rooms.lock().await;

    let Some(room) = rooms.get_mut(&room_id) else {
        client.bad_request().await;
        return;
    };

    if room.host_id != client.id {
        client.bad_request().await;
        return;
    }

    let guest_ids_to_inform = room.guest_ids.clone();

    rooms.remove(&room_id);

    for guest_id in guest_ids_to_inform {
        if let Some(guest) = clients.get_mut(&guest_id) {
            guest.host_deleted_room().await;
        } else {
            warn!("Room {room_id} contains guest id {guest_id}, but there is no client for them");
        };
    }

    client.ok().await;
}

async fn forward_to_client(
    to_client_id: Uuid,
    ws_msg: ws::Message,
    client: &mut Client,
    state: &State,
) {
    let mut clients = state.clients.lock().await;
    let Some(target_client) = clients.get_mut(&to_client_id) else {
        client.client_not_found().await;
        return;
    };
    target_client.send(ws_msg).await;
}
