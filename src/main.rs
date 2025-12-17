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
        hash32(&(self.id.clone(), self.addr))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Room {
    id: u32,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum MessageFromServer {
    Ok,
    BadRequest,
    Disconnect { reason: DisconnectReason },
    RoomCreated { room_id: u32 },
    GuestJoined { guest_id: String },
    JoinedRoom { room: Room },
}

impl MessageFromServer {
    fn ws_msg(&self) -> ws::Message {
        let text: Utf8Bytes = serde_json::to_string(self).unwrap().into();
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
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
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

        if client_id.is_empty() {
            println!("Invalid client_id: {client_id}");
            _ = to_client
                .send(
                    MessageFromServer::Disconnect {
                        reason: DisconnectReason::InvalidClientId,
                    }
                    .ws_msg(),
                )
                .await;
            _ = to_client.close();
            return;
        }

        let mut clients = state.clients.lock().await;
        if clients.contains_key(&client_id) {
            println!("This client_id {client_id} is already taken");
            _ = to_client
                .send(
                    MessageFromServer::Disconnect {
                        reason: DisconnectReason::ClientIdTaken,
                    }
                    .ws_msg(),
                )
                .await;
            _ = to_client.close();
            return;
        }

        let to_client = Arc::new(Mutex::new(to_client));
        let to_client_clone = to_client.clone();

        let client = Client {
            id: client_id.clone(),
            addr,
            comm: to_client_clone,
        };

        clients.insert(client_id.clone(), client.clone());

        let state = state.clone();

        _ = tokio::spawn(async move { ws_loop(client, from_client, to_client, state).await });
    })
}

async fn ws_loop(
    client: Client,
    mut from_client: SplitStream<WebSocket>,
    to_client: Arc<Mutex<SplitSink<WebSocket, ws::Message>>>,
    state: State,
) {
    println!("Starting ws loop");
    while let Some(Ok(ws_msg)) = from_client.next().await {
        if let ws::Message::Close(_) = ws_msg {
            println!("Client connection closed");
            let mut clients = state.clients.lock().await;
            if clients.remove(&client.id).is_none() {
                println!(
                    "[Should not happen] Client {} disconnected, but was not in the clients map",
                    client.id
                );
            }
            return;
        }

        let ws::Message::Text(text) = ws_msg else {
            println!("[Should not happen] Unknown message from client: {ws_msg:?}");
            let mut to_client = to_client.lock().await;
            _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
            continue;
        };

        let msg_from_client = match serde_json::from_str::<MessageFromClient>(&text) {
            Ok(m) => m,
            Err(err) => {
                println!("[Should not happen] Error parsing client message `{text}`: {err}");
                let mut to_client = to_client.lock().await;
                _ = to_client.send(MessageFromServer::BadRequest.ws_msg()).await;
                continue;
            }
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
                forward_to_client(to_client_id, text.clone(), &state).await
            }
        }
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
        {
            let guest_client = clients.get(guest_id).unwrap();
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
            id: room_id,
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

    let host = clients.get(&room.host_id).unwrap();
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

async fn forward_to_client(to_client_id: String, text: Utf8Bytes, state: &State) {
    let clients = state.clients.lock().await;
    let client = clients.get(&to_client_id).unwrap();
    let mut to_client = client.comm.lock().await;
    _ = to_client.send(ws::Message::Text(text)).await;
}
