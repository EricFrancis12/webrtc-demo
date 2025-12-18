use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    Router,
    extract::{
        self, ConnectInfo, Query, WebSocketUpgrade,
        ws::{self, Utf8Bytes, WebSocket},
    },
    response::{Html, IntoResponse},
    routing::get,
};
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use fxhash::hash32;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct Client {
    id: String,
    addr: SocketAddr,
    comm: Arc<Mutex<SplitSink<WebSocket, ws::Message>>>,
}

impl Client {
    fn host_room_id(&self) -> u32 {
        hash32(&(&self.id, self.addr))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Room {
    // TODO: ...
    // max_size: usize,
    // password: Option<String>,
    host_id: String,
    guest_ids: HashSet<String>,
}

#[derive(Clone)]
struct State {
    clients: Arc<Mutex<HashMap<String, Client>>>,
    rooms: Arc<Mutex<HashMap<u32, Room>>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum MessageFromClient {
    StartGame,
    GetRooms,
    CreateRoom,
    JoinRoom {
        room_id: u32,
    },
    DeleteRoom {
        room_id: u32,
    },
    Offer {
        to_client_id: String,
        from_client_id: String,
        offer: serde_json::Value,
    },
    Answer {
        to_client_id: String,
        from_client_id: String,
        answer: serde_json::Value,
    },
    IceCandidate {
        to_client_id: String,
        from_client_id: String,
        candidate: serde_json::Value,
    },
}

pub enum MessageFromClientConversionError {
    NonTextMessage,
    JsonParseError(serde_json::error::Error),
}

impl TryFrom<&ws::Message> for MessageFromClient {
    type Error = MessageFromClientConversionError;

    fn try_from(ws_msg: &ws::Message) -> Result<Self, Self::Error> {
        let ws::Message::Text(text) = ws_msg else {
            return Err(MessageFromClientConversionError::NonTextMessage);
        };
        serde_json::from_str(&text)
            .map_err(|err| MessageFromClientConversionError::JsonParseError(err))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum MessageFromServer {
    Ok,
    BadRequest,
    ClientNotFound,
    Disconnect { reason: DisconnectReason },
    RoomCreated { room_id: u32 },
    GuestJoined { guest_id: String },
    JoinedRoom { room: Room },
}

impl MessageFromServer {
    fn ws_msg(&self) -> ws::Message {
        let text: Utf8Bytes = serde_json::to_string(self)
            .expect("MessageFromServer should serialize")
            .into();
        ws::Message::Text(text)
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum DisconnectReason {
    InvalidClientId,
    ClientIdTaken,
    HostStartedGame,
}

const PORT: u16 = 3000;

#[tokio::main]
async fn main() {
    let state = State {
        clients: Arc::new(Mutex::new(HashMap::new())),
        rooms: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(handle_serve_html))
        .route("/index.html", get(handle_serve_html))
        .route("/ws", get(handle_ws))
        .with_state(state);

    println!("Server running on port {PORT}");
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
    Html(include_str!("../index.html"))
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
        let client_id = query.client_id;
        println!("New ws connection from client_id {client_id}");

        let (mut to_client, from_client) = socket.split();

        if let Some(reason) = should_disconnect(&client_id, &state).await {
            _ = to_client
                .send(MessageFromServer::Disconnect { reason }.ws_msg())
                .await;
            _ = to_client.close();
            return;
        }

        let client = Client {
            id: client_id.clone(),
            addr,
            comm: Arc::new(Mutex::new(to_client)),
        };

        let mut clients = state.clients.lock().await;
        clients.insert(client_id.clone(), client.clone());

        let state = state.clone();

        _ = tokio::spawn(async move { ws_loop(client, from_client, state).await });
    })
}

async fn should_disconnect(client_id: &str, state: &State) -> Option<DisconnectReason> {
    if client_id.is_empty() {
        return Some(DisconnectReason::InvalidClientId);
    }
    let clients = state.clients.lock().await;
    if clients.contains_key(client_id) {
        return Some(DisconnectReason::ClientIdTaken);
    }
    None
}

async fn ws_loop(client: Client, mut from_client: SplitStream<WebSocket>, state: State) {
    while let Some(Ok(ws_msg)) = from_client.next().await {
        if let ws::Message::Close(_) = ws_msg {
            handle_connection_closed(&client, &state).await;
            return;
        }

        let Ok(msg_from_client) = MessageFromClient::try_from(&ws_msg) else {
            println!("[Should not happen] Unknown message from client: {ws_msg:?}");
            let mut to_client = client.comm.lock().await;
            _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
            continue;
        };

        match msg_from_client {
            MessageFromClient::StartGame => handle_start_game(&client, &state).await,
            MessageFromClient::GetRooms => handle_get_rooms().await,
            MessageFromClient::CreateRoom => handle_create_room(&client, &state).await,
            MessageFromClient::JoinRoom { room_id } => {
                handle_join_room(room_id, &client, &state).await
            }
            MessageFromClient::DeleteRoom { room_id } => {
                handle_delete_room(room_id, &client, &state).await
            }
            MessageFromClient::Offer { to_client_id, .. }
            | MessageFromClient::Answer { to_client_id, .. }
            | MessageFromClient::IceCandidate { to_client_id, .. } => {
                forward_to_client(to_client_id, ws_msg, &client, &state).await
            }
        }
    }
}

async fn handle_connection_closed(client: &Client, state: &State) {
    let mut clients = state.clients.lock().await;
    if clients.remove(&client.id).is_none() {
        println!(
            "[Should not happen] Client {} disconnected, but was not in the clients map",
            client.id
        );
    }
}

async fn handle_start_game(client: &Client, state: &State) {
    let mut rooms = state.rooms.lock().await;

    let room_id = client.host_room_id();
    let Some(room) = rooms.get(&room_id) else {
        let mut to_client = client.comm.lock().await;
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    };

    let mut clients = state.clients.lock().await;
    for guest_id in &room.guest_ids {
        if let Some(guest_client) = clients.get(guest_id) {
            let mut to_client = guest_client.comm.lock().await;
            _ = to_client
                .send(
                    MessageFromServer::Disconnect {
                        reason: DisconnectReason::HostStartedGame,
                    }
                    .ws_msg(),
                )
                .await;
            _ = to_client.close();
        } else {
            println!(
                "[Should not happen] Guest id {guest_id} is present in room {room_id}, but is not in clients"
            );
        }

        clients.remove(guest_id);
    }

    let mut to_client = client.comm.lock().await;
    _ = to_client
        .send(
            MessageFromServer::Disconnect {
                reason: DisconnectReason::HostStartedGame,
            }
            .ws_msg(),
        )
        .await;
    _ = to_client.close();

    rooms.remove(&room_id);
}

async fn handle_get_rooms() {
    todo!();
}

async fn handle_create_room(client: &Client, state: &State) {
    let mut to_client = client.comm.lock().await;
    let mut rooms = state.rooms.lock().await;

    let room_id = client.host_room_id();

    if rooms.contains_key(&room_id) {
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    }

    rooms.insert(
        room_id,
        Room {
            host_id: client.id.clone(),
            guest_ids: HashSet::new(),
        },
    );

    println!("Room {room_id} created by client {}", client.id);

    _ = to_client
        .send(MessageFromServer::RoomCreated { room_id }.ws_msg())
        .await;
}

async fn handle_join_room(room_id: u32, client: &Client, state: &State) {
    let mut to_client = client.comm.lock().await;
    let clients = state.clients.lock().await;
    let mut rooms = state.rooms.lock().await;

    let Some(room) = rooms.get_mut(&room_id) else {
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    };

    if room.host_id == client.id {
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    }

    let ws_msg = MessageFromServer::GuestJoined {
        guest_id: client.id.clone(),
    }
    .ws_msg();

    room.guest_ids.insert(client.id.clone());

    let Some(host) = clients.get(&room.host_id) else {
        println!(
            "[Should not happen] Correcting illegal state: room {room_id} exists with host {} not in clients",
            room.host_id
        );
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    };

    let mut to_host = host.comm.lock().await;
    _ = to_host.send(ws_msg.clone()).await;

    for guest_id in &room.guest_ids {
        if guest_id == &client.id {
            _ = to_client
                .send(MessageFromServer::JoinedRoom { room: room.clone() }.ws_msg())
                .await;
            continue;
        }

        let guest = clients.get(guest_id).unwrap();
        let mut to_guest = guest.comm.lock().await;
        _ = to_guest.send(ws_msg.clone()).await;
    }
}

async fn handle_delete_room(room_id: u32, client: &Client, state: &State) {
    let mut to_client = client.comm.lock().await;
    let mut rooms = state.rooms.lock().await;

    let Some(room) = rooms.get_mut(&room_id) else {
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    };

    if room.host_id != client.id {
        _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
        return;
    }

    rooms.remove(&room_id);
    todo!("notify guests that the host deleted this room")
}

async fn forward_to_client(
    to_client_id: String,
    ws_msg: ws::Message,
    client: &Client,
    state: &State,
) {
    let clients = state.clients.lock().await;
    let Some(target_client) = clients.get(&to_client_id) else {
        let mut comm = client.comm.lock().await;
        _ = comm.send(MessageFromServer::ClientNotFound.ws_msg()).await;
        return;
    };
    let mut to_client = target_client.comm.lock().await;
    _ = to_client.send(ws_msg).await;
}
