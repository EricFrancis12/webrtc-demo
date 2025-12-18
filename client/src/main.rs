use std::io::{self, Write};

use futures::{SinkExt, StreamExt};
use rand;
use tokio_tungstenite::{connect_async, tungstenite as ws};

use server::{MessageFromClient, MessageFromServer};

#[tokio::main]
async fn main() {
    let client_id: u32 = rand::random();
    let url = format!("ws://localhost:3000/ws?client_id={}", client_id);

    println!("Connecting to ws server at {url}");

    let (ws_stream, _) = connect_async(url).await.unwrap();
    let (mut server_tx, mut server_rx) = ws_stream.split();

    // _ = tokio::spawn(async move {
    //     while let Some(Ok(ws_msg)) = server_rx.next().await {
    //         println!("Message from server: {ws_msg:?}");
    //     }
    // });

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

    loop {
        println!();
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
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "create" | "2" => {
                println!("Creating room...");
                let text = serde_json::to_string(&MessageFromClient::CreateRoom)
                    .unwrap()
                    .into();
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
                _ = server_tx.send(ws::Message::Text(text)).await;
            }
            "send" | "4" => {
                print!("Enter message: ");
                io::stdout().flush().unwrap();
                let mut message = String::new();
                io::stdin().read_line(&mut message).unwrap();
                println!("Sending: {}", message.trim());
                // TODO: Send message through data channel
            }
            "start" | "5" => {
                println!("Starting game & disconnecting from server...");
                // TODO: ...
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

        // Blocking receive
        let mut received = false;
        while !received {
            if let Some(Ok(ws_msg)) = server_rx.next().await {
                let ws::Message::Text(text) = ws_msg else {
                    panic!("Non-text ws message");
                };
                let msg_from_server: MessageFromServer = serde_json::from_str(&text).unwrap();

                println!("Message from server: {msg_from_server:?}");
                received = true;
            }
        }
    }
}
