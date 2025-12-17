use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    extract::{
        self,
        ws::{self, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::get,
};
use futures::{SinkExt, StreamExt, stream::SplitSink};
use serde::{Deserialize, Serialize};

struct Client {
    id: usize,
    comm: SplitSink<WebSocket, ws::Message>,
}

#[derive(Clone)]
struct State {
    connection_counter: Arc<Mutex<usize>>,
    room: Arc<Mutex<HashMap<usize, Client>>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum MessageFromClient {
    JoinRequest,
    Offer { offer: serde_json::Value },
    Answer { answer: serde_json::Value },
    IceCandidate { candidate: serde_json::Value },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum MessageFromServer {
    WaitingForPartner,
    PartnersReady { initiator: bool },
    ServerClosing,
}

const PORT: u16 = 3000;

#[tokio::main]
async fn main() {
    let state = State {
        connection_counter: Arc::new(Mutex::new(0)),
        room: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(handle_serve_html))
        .route("/index.html", get(handle_serve_html))
        .route("/ws", get(ws_handler))
        .with_state(state);

    println!("Server running on port {PORT}");
    let addr = format!("0.0.0.0:{PORT}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_serve_html() -> Html<&'static str> {
    Html(include_str!("../index.html"))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    extract::State(state): extract::State<State>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: State) {
    // Increment connection counter
    let client_id = {
        let mut counter = state.connection_counter.lock().unwrap();
        *counter += 1;
        *counter
    };

    println!("New client connected (id {})", client_id);

    let (mut socket_out, mut socket_in) = socket.split();

    let mut room = state.room.lock().unwrap();
    if room.len() < 2 {
        if room.is_empty() {
            let msg = serde_json::to_string(&MessageFromServer::WaitingForPartner)
                .unwrap()
                .into();
            _ = socket_out.send(ws::Message::Text(msg));
        }

        room.insert(
            client_id,
            Client {
                id: client_id,
                comm: socket_out,
            },
        );

        if room.len() == 2 {
            let mut initiator = true;
            for client in room.values_mut() {
                let msg = serde_json::to_string(&MessageFromServer::PartnersReady { initiator })
                    .unwrap()
                    .into();
                _ = client.comm.send(ws::Message::Text(msg));
                _ = client.comm.close();
                initiator = !initiator;
            }
        }
    } else {
        println!("Room is full, please try again later");
        return;
    }

    let state_clone = state.clone();

    tokio::spawn(async move {
        while let Some(Ok(msg)) = socket_in.next().await {
            if let ws::Message::Close(_) = msg {
                println!("Client connection closed");
                handle_disconnect(client_id, state_clone.clone());
                break;
            }

            let ws::Message::Text(text) = msg else {
                println!("[Should not happen] Unhandled ws::Message from client: {msg:?}");
                continue;
            };

            let Ok(msg_from_client) = serde_json::from_str::<MessageFromClient>(&text) else {
                println!("[Should not happen] Error parsing client message");
                continue;
            };

            match msg_from_client {
                MessageFromClient::JoinRequest => handle_join_request(state_clone.clone()),
                MessageFromClient::Offer { .. }
                | MessageFromClient::Answer { .. }
                | MessageFromClient::IceCandidate { .. } => {
                    forward_to_peer(client_id, msg_from_client, state_clone.clone())
                }
            }
        }
    });
}

fn handle_join_request(state: State) {
    // TODO: ...
}

fn forward_to_peer(from_client_id: usize, msg_from_client: MessageFromClient, state: State) {
    let mut room = state.room.lock().unwrap();
    for client in room.values_mut() {
        if client.id == from_client_id {
            continue;
        }
        let msg = serde_json::to_string(&msg_from_client).unwrap().into();
        _ = client.comm.send(ws::Message::Text(msg));
    }
}

fn handle_disconnect(client_id: usize, state: State) {
    let mut room = state.room.lock().unwrap();
    if room.remove(&client_id).is_none() {
        println!(
            "[Should not happen] Attempting to remove client {client_id} from room, but they are not there"
        );
    }
}
