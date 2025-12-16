use std::sync::Arc;

use axum::{
    Router,
    extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

type Tx = mpsc::UnboundedSender<Message>;
type PeerTx = mpsc::UnboundedSender<Option<Tx>>;

#[derive(Clone)]
struct State {
    waiting_client: Arc<Mutex<Option<(Tx, PeerTx)>>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum WsMessage {
    Join,
    Waiting,
    Ready { initiator: bool },
    Offer { offer: serde_json::Value },
    Answer { answer: serde_json::Value },
    IceCandidate { candidate: serde_json::Value },
    PeerDisconnected,
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

    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let (peer_tx_sender, mut peer_tx_receiver) = mpsc::unbounded_channel::<Option<Tx>>();

    // Spawn task to send messages from channel to client
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let tx_clone = tx.clone();
    let state_clone = state.clone();
    let peer_tx = Arc::new(Mutex::new(None::<Tx>));
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
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                if let Ok(data) = serde_json::from_str::<WsMessage>(&text) {
                    println!("Received: {:?}", data);

                    match data {
                        WsMessage::Join => {
                            handle_join(tx_clone.clone(), peer_tx_sender.clone(), &state_clone)
                                .await;
                        }
                        WsMessage::Offer { .. }
                        | WsMessage::Answer { .. }
                        | WsMessage::IceCandidate { .. } => {
                            let peer_opt = peer_tx_for_recv.lock().await;
                            if let Some(ref peer) = *peer_opt {
                                let _ = peer.send(Message::Text(text));
                            }
                        }
                        _ => {}
                    }
                }
            } else if let Message::Close(_) = msg {
                break;
            }
        }
        peer_tx_for_recv.lock().await.clone()
    });

    // Wait for tasks to complete
    tokio::select! {
        peer_tx_result = &mut recv_task => {
            send_task.abort();
            // Notify peer if connected
            if let Ok(Some(peer)) = peer_tx_result {
                let msg = serde_json::to_string(&WsMessage::PeerDisconnected).unwrap().into();
                let _ = peer.send(Message::Text(msg));
            }
        }
        _ = &mut send_task => {
            recv_task.abort();
        }
    }

    println!("Client disconnected");
}

async fn handle_join(tx: Tx, peer_tx_sender: PeerTx, state: &State) {
    let mut waiting = state.waiting_client.lock().await;

    if waiting.is_none() {
        // First client - wait for another
        println!("First client waiting for peer...");
        let msg = serde_json::to_string(&WsMessage::Waiting).unwrap().into();
        let _ = tx.send(Message::Text(msg));
        *waiting = Some((tx.clone(), peer_tx_sender));
    } else {
        // Second client - pair them up
        let (peer1_tx, peer1_peer_sender) = waiting.take().unwrap();
        let peer2_tx = tx.clone();

        println!("Two clients paired! Starting WebRTC handshake...");

        // Send each client the other's transmitter so they can forward messages
        let _ = peer1_peer_sender.send(Some(peer2_tx.clone()));
        let _ = peer_tx_sender.send(Some(peer1_tx.clone()));

        // Tell first client to initiate the offer
        let msg1 = serde_json::to_string(&WsMessage::Ready { initiator: true })
            .unwrap()
            .into();
        let _ = peer1_tx.send(Message::Text(msg1));

        let msg2 = serde_json::to_string(&WsMessage::Ready { initiator: false })
            .unwrap()
            .into();
        let _ = peer2_tx.send(Message::Text(msg2));

        // Close connections after clients establish P2P (give them 5 seconds for handshake)
        let peer1_clone = peer1_tx.clone();
        let peer2_clone = peer2_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            println!("P2P connection established. Closing signaling connections...");

            let msg: Utf8Bytes = serde_json::to_string(&WsMessage::ServerClosing)
                .unwrap()
                .into();
            let _ = peer1_clone.send(Message::Text(msg.clone()));
            let _ = peer2_clone.send(Message::Text(msg));

            // Close connections by sending Close message
            let _ = peer1_clone.send(Message::Close(None));
            let _ = peer2_clone.send(Message::Close(None));
        });
    }
}
