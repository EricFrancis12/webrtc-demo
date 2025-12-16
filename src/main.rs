use std::sync::Arc;

use axum::{
    Router,
    extract::ws::{self, Utf8Bytes, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    Mutex,
    mpsc::{self, UnboundedSender},
};

type PeerTx = UnboundedSender<Option<UnboundedSender<ws::Message>>>;

#[derive(Clone)]
struct State {
    waiting_client: Arc<Mutex<Option<(UnboundedSender<ws::Message>, PeerTx)>>>,
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
        waiting_client: Arc::new(Mutex::new(None)),
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
    axum::extract::State(state): axum::extract::State<State>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: State) {
    println!("New client connected");

    let (mut socket_sender, mut socket_receiver) = socket.split();
    let (ws_msg_sender, mut ws_msg_receiver) = mpsc::unbounded_channel::<ws::Message>();
    let (peer_tx_sender, mut peer_tx_receiver) =
        mpsc::unbounded_channel::<Option<UnboundedSender<ws::Message>>>();

    // Spawn task to send messages from channel to client
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = ws_msg_receiver.recv().await {
            if let Err(err) = socket_sender.send(msg).await {
                println!("Error sending message: {err}");
                break;
            }
        }
    });

    let ws_msg_sender_clone = ws_msg_sender.clone();
    let state_clone = state.clone();
    let peer_tx = Arc::new(Mutex::new(None::<UnboundedSender<ws::Message>>));
    let peer_tx_clone = peer_tx.clone();

    // Listen for peer assignment
    tokio::spawn(async move {
        if let Some(peer) = peer_tx_receiver.recv().await {
            *peer_tx_clone.lock().await = peer;
        }
    });

    let peer_tx_for_recv = peer_tx.clone();
    // Receive messages from client
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = socket_receiver.next().await {
            if let ws::Message::Close(_) = msg {
                break;
            }

            let ws::Message::Text(text) = msg else {
                println!("[Should not happen] Unhandled message from client: {msg:?}");
                continue;
            };

            let Ok(data) = serde_json::from_str::<MessageFromClient>(&text) else {
                continue;
            };

            match data {
                MessageFromClient::JoinRequest => {
                    handle_join_request(
                        ws_msg_sender_clone.clone(),
                        peer_tx_sender.clone(),
                        &state_clone,
                    )
                    .await;
                }
                MessageFromClient::Offer { .. }
                | MessageFromClient::Answer { .. }
                | MessageFromClient::IceCandidate { .. } => {
                    // Forward messages to the other peer (partner)
                    let peer_opt = peer_tx_for_recv.lock().await;
                    if let Some(ref peer) = *peer_opt {
                        _ = peer.send(ws::Message::Text(text));
                    }
                }
            }
        }
        peer_tx_for_recv.lock().await.clone()
    });

    // Wait for either task to complete/fail.
    // Whichever finishes first causes the other to be aborted.
    tokio::select! {
       _= &mut recv_task => {
            send_task.abort();
        }
        _ = &mut send_task => {
            recv_task.abort();
        }
    }

    println!("Client disconnected");
}

async fn handle_join_request(
    ws_msg_sender: UnboundedSender<ws::Message>,
    peer_tx_sender: PeerTx,
    state: &State,
) {
    let mut waiting_client = state.waiting_client.lock().await;

    if waiting_client.is_none() {
        // First client - wait for another
        println!("First client waiting for peer...");
        let msg = serde_json::to_string(&MessageFromServer::WaitingForPartner)
            .unwrap()
            .into();
        _ = ws_msg_sender.send(ws::Message::Text(msg));
        *waiting_client = Some((ws_msg_sender.clone(), peer_tx_sender));
    } else {
        // Second client - pair them up
        let (peer1_tx, peer1_peer_sender) = waiting_client.take().unwrap();
        let peer2_tx = ws_msg_sender.clone();

        println!("Two clients paired! Starting WebRTC handshake...");

        // Send each client the other's transmitter so they can forward messages
        _ = peer1_peer_sender.send(Some(peer2_tx.clone()));
        _ = peer_tx_sender.send(Some(peer1_tx.clone()));

        // Tell first client to initiate the offer
        let msg1 = serde_json::to_string(&MessageFromServer::PartnersReady { initiator: true })
            .unwrap()
            .into();
        _ = peer1_tx.send(ws::Message::Text(msg1));

        let msg2 = serde_json::to_string(&MessageFromServer::PartnersReady { initiator: false })
            .unwrap()
            .into();
        _ = peer2_tx.send(ws::Message::Text(msg2));

        // Close connections after clients establish P2P (give them 5 seconds for handshake)
        let peer1_clone = peer1_tx.clone();
        let peer2_clone = peer2_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            println!("P2P connection established. Closing signaling connections...");

            let msg: Utf8Bytes = serde_json::to_string(&MessageFromServer::ServerClosing)
                .unwrap()
                .into();
            _ = peer1_clone.send(ws::Message::Text(msg.clone()));
            _ = peer2_clone.send(ws::Message::Text(msg));

            // Close connections by sending Close message
            _ = peer1_clone.send(ws::Message::Close(None));
            _ = peer2_clone.send(ws::Message::Close(None));
        });
    }
}
