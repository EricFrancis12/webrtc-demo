use std::{
    collections::HashMap,
    net::SocketAddr,
    ops::Range,
    sync::{Arc, Mutex},
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
use futures::{SinkExt, StreamExt, stream::SplitSink};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug)]
struct Client {
    id: String,
    addr: SocketAddr,
    comm: Arc<Mutex<SplitSink<WebSocket, ws::Message>>>,
}

#[derive(Debug)]
struct Room {
    max_size: usize,
    // password: Option<String>, // TODO: ...
    host: Client,
    guests: HashMap<Uuid, Client>,
}

#[derive(Clone)]
struct State {
    clients: Arc<Mutex<HashMap<String, Client>>>,
    rooms: Arc<Mutex<HashMap<u64, Room>>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum MessageFromClient {
    GetRooms,
    CreateRoom,
    JoinRoom { room_id: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum MessageFromServer {
    Ok,
    BadRequest,
    Disconnect(DisconnectReason),
}

impl MessageFromServer {
    fn ws_msg(&self) -> ws::Message {
        let text: Utf8Bytes = serde_json::to_string(self).unwrap().into();
        ws::Message::Text(text)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum DisconnectReason {
    InvalidClientId,
    ClientIdTaken,
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
    println!("HERE");

    ws.on_upgrade(async move |socket| {
        let client_id = query.client_id;
        println!("New ws connection from client_id {client_id}");

        let (mut to_client, mut from_client) = socket.split();

        if client_id.is_empty() {
            println!("Invalid client_id: {client_id}");
            _ = to_client.send(MessageFromServer::Disconnect(
                DisconnectReason::InvalidClientId,
            ).ws_msg());
            _ = to_client.close();
            return;
        }

        let mut clients = state.clients.lock().unwrap();
        if clients.contains_key(&client_id) {
            println!("This client_id {client_id} is already taken");
            _ = to_client.send(MessageFromServer::Disconnect(
                DisconnectReason::InvalidClientId,
            ).ws_msg());
            _ = to_client.close();
            return;
        }

        let client_id = client_id.clone();
        let state = state.clone();

        let to_client = Arc::new(Mutex::new(to_client));
        let to_client_clone = to_client.clone();
        clients.insert(client_id.clone(), Client{id: client_id.clone(), addr, comm: to_client_clone});

        tokio::spawn(async move {
            while let Some(Ok(ws_msg)) = from_client.next().await {
                if let ws::Message::Close(_) = ws_msg {
                    println!("Client connection closed");
                    let mut clients = state.clients.lock().unwrap();
                    if clients.remove(&client_id).is_none() {
                        println!("[Should not happen] Client {client_id} disconnected, but was not in the clients map");
                    }
                    break;
                }

                let ws::Message::Text(text) = ws_msg else {
                    println!("[Should not happen] Unknown message from client: {ws_msg:?}");
                    let mut to_client = to_client.lock().unwrap();
                    _ = to_client.send(MessageFromServer::BadRequest.ws_msg());
                    continue;
                };

                let Ok(msg_from_client) = serde_json::from_str::<MessageFromClient>(&text) else {
                    println!("[Should not happen] Error parsing client message");
                    let mut to_client = to_client.lock().unwrap();
                    _ = to_client.send(MessageFromServer::BadRequest.ws_msg());
                    continue;
                };

                match msg_from_client {
                    MessageFromClient::GetRooms => handle_get_rooms(),
                    MessageFromClient::CreateRoom => handle_create_room(),
                    MessageFromClient::JoinRoom { room_id } => handle_join_room(room_id),
                }
            }
        });
    })
}

fn handle_get_rooms() {}

fn handle_create_room() {}

fn handle_join_room(room_id: u64) {}

// (String, SocketAddr) -> u64

// let room_id = (host_id, addr);

// if rooms.contains_key(&room_id) {
//     println!("Client {host_id} already is hosting a room");

//     let msg = serde_json::to_string(&MessageFromServer::RoomAlreadyExists)
//         .unwrap()
//         .into();
//     _ = to_client.send(ws::Message::Text(msg));
// } else {
//     println!("Creating new room with client {host_id} as the host");

//     let msg = serde_json::to_string(&MessageFromServer::RoomCreated)
//         .unwrap()
//         .into();
//     _ = to_client.send(ws::Message::Text(msg));

//     let host = Client {
//         id: host_id,
//         addr,
//         comm: to_client,
//     };
//     rooms.insert(
//         room_id,
//         Room {
//             max_size: 4,
//             host,
//             guests: HashMap::new(),
//         },
//     );
// }
