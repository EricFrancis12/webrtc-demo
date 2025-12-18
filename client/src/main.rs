use std::{
    collections::HashMap,
    io::{self, Write},
    sync::Arc,
};

use futures::{SinkExt, StreamExt, stream::SplitSink};
use rand;
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite as ws};
use webrtc::{
    api::APIBuilder,
    data_channel::RTCDataChannel,
    ice_transport::{ice_candidate::RTCIceCandidate, ice_server::RTCIceServer},
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
};

use server::{MessageFromClient, MessageFromServer};

struct Peer {
    rpc_conn: RTCPeerConnection,
    data_channel: Option<Arc<RTCDataChannel>>,
}

#[tokio::main]
async fn main() {
    let client_id = rand::random::<u32>().to_string();
    let url = format!("ws://localhost:3000/ws?client_id={}", client_id);

    println!("Connecting to ws server at {url}");

    let (ws_stream, _) = connect_async(url).await.unwrap();
    let (server_tx, mut server_rx) = ws_stream.split();

    let server_tx = Arc::new(Mutex::new(server_tx));
    let server_tx_clone = server_tx.clone();

    let conns: Arc<Mutex<HashMap<String, Peer>>> = Arc::new(Mutex::new(HashMap::new()));
    let conns_clone = conns.clone();

    _ = tokio::spawn(async move {
        let server_tx = server_tx_clone.clone();
        let conns = conns_clone.clone();

        while let Some(Ok(ws_msg)) = server_rx.next().await {
            let ws::Message::Text(text) = &ws_msg else {
                println!("[Should not happen] Non-text ws message");
                continue;
            };

            if let Ok(msg_from_server) = serde_json::from_str::<MessageFromServer>(text) {
                println!("Message from server: {msg_from_server:?}");
                use MessageFromServer as M;
                match msg_from_server {
                    M::Ok | M::BadRequest | M::ClientNotFound => { /* Do  nothing */ }
                    M::Disconnect { reason } => {
                        println!("Disconnected from server. reason: {reason:?}");
                        break;
                    }
                    M::Rooms { rooms } => println!("Rooms: {rooms:?}"),
                    M::RoomCreated { room_id } => println!("You are now hosting Room {room_id}"),
                    M::GuestJoined { guest_id } => {
                        let mut peer =
                            setup_peer_connection(client_id.clone(), &guest_id, server_tx.clone())
                                .await;

                        let data_channel = peer
                            .rpc_conn
                            .create_data_channel("chat", None)
                            .await
                            .unwrap();

                        setup_data_channel(&data_channel);

                        peer.data_channel = Some(data_channel);

                        // Create and send offer
                        let offer = peer.rpc_conn.create_offer(None).await.unwrap();
                        peer.rpc_conn
                            .set_local_description(offer.clone())
                            .await
                            .unwrap();

                        let mut conns = conns.lock().await;
                        conns.insert(guest_id.clone(), peer);

                        let msg_from_client = MessageFromClient::Offer {
                            to_client_id: guest_id,
                            from_client_id: client_id.clone(),
                            offer,
                        };
                        let text = serde_json::to_string(&msg_from_client).unwrap().into();

                        let mut server_tx = server_tx.lock().await;
                        _ = server_tx.send(ws::Message::Text(text)).await;
                    }
                    M::JoinedRoom { room } => {
                        let peer = setup_peer_connection(
                            client_id.clone(),
                            &room.host_id,
                            server_tx.clone(),
                        )
                        .await;

                        let host_id_clone = room.host_id.clone();
                        let conns_clone = conns.clone();

                        peer.rpc_conn.on_data_channel(Box::new(move |data_channel| {
                            let host_id = host_id_clone.clone();
                            let conns = conns_clone.clone();
                            Box::pin(async move {
                                let mut conns = conns.lock().await;
                                let peer = conns.get_mut(&host_id).unwrap();

                                setup_data_channel(&data_channel);
                                peer.data_channel = Some(data_channel);
                            })
                        }));

                        let mut conns = conns.lock().await;
                        conns.insert(room.host_id, peer);
                    }
                    M::HostDeletedRoom => todo!(),
                }
            } else if let Ok(msg_from_peer) = serde_json::from_str::<MessageFromClient>(text) {
                println!("Message from peer: {msg_from_peer:?}");
                use MessageFromClient as M;
                match msg_from_peer {
                    M::Offer {
                        from_client_id,
                        offer,
                        ..
                    } => {
                        println!("Offer received from {from_client_id}: {offer:?}");
                        handle_offer(
                            client_id.clone(),
                            from_client_id,
                            offer,
                            server_tx.clone(),
                            conns.clone(),
                        )
                        .await;
                    }
                    M::Answer {
                        from_client_id,
                        answer,
                        ..
                    } => {
                        println!("Answer received from {from_client_id}: {answer:?}");
                        handle_answer(from_client_id, answer, conns.clone()).await;
                    }
                    M::IceCandidate {
                        from_client_id,
                        candidate,
                        ..
                    } => {
                        println!("ICE Candidate received from {from_client_id}: {candidate:?}");
                        handle_ice_candidate(from_client_id, candidate, conns.clone()).await;
                    }
                    _ => println!(
                        "[Should not happen] Unhandled message from peer: {msg_from_peer:?}"
                    ),
                }
            } else {
                println!("[Should not happen] Unknown message format: {ws_msg:?}");
            }
        }
    });

    println!("Connected to ws server");

    println!();
    println!("WebRTC Client");
    println!("Commands:");
    println!("  1. list - List all rooms");
    println!("  2. create - Create a new room");
    println!("  3. join - Join a room");
    println!("  4. send - Send message to room");
    println!("  5. start - Start game & disconnect from server");
    println!("  6. exit - Exit application");
    println!();

    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        match input {
            "list" | "1" => {
                println!("Listing rooms...");
                let text = serde_json::to_string(&MessageFromClient::GetRooms)
                    .unwrap()
                    .into();
                let mut server_tx = server_tx.lock().await;
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "create" | "2" => {
                println!("Creating room...");
                let text = serde_json::to_string(&MessageFromClient::CreateRoom)
                    .unwrap()
                    .into();
                let mut server_tx = server_tx.lock().await;
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "join" | "3" => {
                print!("Enter room ID: ");
                io::stdout().flush().unwrap();
                let mut room_id = String::new();
                io::stdin().read_line(&mut room_id).unwrap();
                room_id = room_id.trim().to_owned();
                println!("Joining room {}...", room_id);
                let Ok(room_id) = room_id.parse::<u32>() else {
                    println!("Room id must be a number");
                    continue;
                };
                let text = serde_json::to_string(&MessageFromClient::JoinRoom { room_id })
                    .unwrap()
                    .into();
                let mut server_tx = server_tx.lock().await;
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "send" | "4" => {
                print!("Enter message: ");
                io::stdout().flush().unwrap();
                let mut message = String::new();
                io::stdin().read_line(&mut message).unwrap();
                message = message.trim().to_owned();
                // Send message through data channel
                let mut conns = conns.lock().await;
                for (peer_id, conn) in conns.iter_mut() {
                    println!("Sending message to peer {peer_id}");
                    if let Some(data_channel) = &mut conn.data_channel {
                        _ = data_channel.send_text(message.clone()).await;
                    } else {
                        println!(
                            "[Should not happen] Attempting to send to peer {peer_id}, who does not have a data channel"
                        );
                    }
                }
            }
            "start" | "5" => {
                println!("Starting game & disconnecting from server...");
                let text = serde_json::to_string(&MessageFromClient::StartGame)
                    .unwrap()
                    .into();
                let mut server_tx = server_tx.lock().await;
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "exit" | "quit" | "6" => {
                println!("Goodbye!");
                break;
            }
            _ => {
                println!("Unknown command: {}", input);
                println!("Type 'list', 'create', 'join', 'send', 'start', or 'exit'");
                continue;
            }
        }

        // Wait for enter press
        io::stdin().read_line(&mut String::new()).unwrap();

        // Blocking receive
        // let mut received = false;
        // while !received {
        //     if let Some(Ok(ws_msg)) = server_rx.next().await {
        //         let ws::Message::Text(text) = ws_msg else {
        //             panic!("Non-text ws message");
        //         };
        //         let msg_from_server: MessageFromServer = serde_json::from_str(&text).unwrap();

        //         println!("Message from server: {msg_from_server:?}");
        //         received = true;
        //     }
        // }
    }
}

async fn setup_peer_connection(
    client_id: String,
    peer_id: impl Into<String>,
    server_tx: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, ws::Message>>>,
) -> Peer {
    let api = APIBuilder::new().build();

    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".to_owned(),
                "stun:stun1.l.google.com:19302".to_owned(),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let rpc_conn = api.new_peer_connection(config).await.unwrap();

    rpc_conn.on_peer_connection_state_change(Box::new(move |state| {
        Box::pin(async move {
            println!("Peer Connection state change event: {state:?}");
        })
    }));

    rpc_conn.on_ice_connection_state_change(Box::new(move |state| {
        Box::pin(async move {
            println!("ICE Connection state change event: {state:?}");
        })
    }));

    let peer_id_clone = peer_id.into().clone();
    let server_tx_clone = server_tx.clone();

    rpc_conn.on_ice_candidate(Box::new(move |candidate| {
        let client_id = client_id.clone();
        let peer_id = peer_id_clone.clone();
        let server_tx = server_tx_clone.clone();
        Box::pin(async move {
            if let Some(candidate) = candidate {
                println!("Received ICE Candidate: {candidate:?}");

                let msg_from_client = MessageFromClient::IceCandidate {
                    to_client_id: peer_id,
                    from_client_id: client_id.clone(),
                    candidate,
                };
                let text = serde_json::to_string(&msg_from_client).unwrap().into();

                let mut server_tx = server_tx.lock().await;
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
        })
    }));

    Peer {
        rpc_conn,
        data_channel: None,
    }
}

fn setup_data_channel(data_channel: &Arc<RTCDataChannel>) {
    data_channel.on_open(Box::new(move || {
        Box::pin(async move {
            println!("Data channel opened");
        })
    }));

    data_channel.on_close(Box::new(move || {
        Box::pin(async move {
            println!("Data channel closed");
        })
    }));

    data_channel.on_message(Box::new(move |msg| {
        Box::pin(async move {
            println!("Data channel message received: {msg:?}");
        })
    }));

    data_channel.on_error(Box::new(move |err| {
        Box::pin(async move {
            println!("Data channel error: {err:?}");
        })
    }));
}

async fn handle_offer(
    client_id: String,
    from_peer_id: String,
    offer: RTCSessionDescription,
    server_tx: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, ws::Message>>>,
    conns: Arc<Mutex<HashMap<String, Peer>>>,
) {
    let peer = setup_peer_connection(client_id.clone(), &from_peer_id, server_tx.clone()).await;

    let peer_id_clone = from_peer_id.clone();
    let conns_clone = conns.clone();

    // Guest needs to listen for the data channel created by the host
    peer.rpc_conn.on_data_channel(Box::new(move |data_channel| {
        let peer_id = peer_id_clone.clone();
        let conns = conns_clone.clone();
        Box::pin(async move {
            println!("Data channel received from host");

            let mut conns = conns.lock().await;
            let peer = conns.get_mut(&peer_id).unwrap();

            setup_data_channel(&data_channel);
            peer.data_channel = Some(data_channel);
        })
    }));

    peer.rpc_conn.set_remote_description(offer).await.unwrap();
    let answer = peer.rpc_conn.create_answer(None).await.unwrap();
    peer.rpc_conn
        .set_local_description(answer.clone())
        .await
        .unwrap();

    let mut conns = conns.lock().await;
    conns.insert(from_peer_id.clone(), peer);

    let msg_from_client = MessageFromClient::Answer {
        to_client_id: from_peer_id,
        from_client_id: client_id,
        answer,
    };
    let text = serde_json::to_string(&msg_from_client).unwrap().into();

    let mut server_tx = server_tx.lock().await;
    _ = server_tx.send(ws::Message::Text(text)).await;
}

async fn handle_answer(
    from_peer_id: String,
    answer: RTCSessionDescription,
    conns: Arc<Mutex<HashMap<String, Peer>>>,
) {
    let mut conns = conns.lock().await;
    let peer = conns.get_mut(&from_peer_id).unwrap();
    peer.rpc_conn.set_remote_description(answer).await.unwrap();
}

async fn handle_ice_candidate(
    from_peer_id: String,
    candidate: RTCIceCandidate,
    conns: Arc<Mutex<HashMap<String, Peer>>>,
) {
    let mut conns = conns.lock().await;
    let peer = conns.get_mut(&from_peer_id).unwrap();
    peer.rpc_conn
        .add_ice_candidate(candidate.to_json().unwrap())
        .await
        .unwrap();
}
