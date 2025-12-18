use std::io::{self, Write};

fn main() {
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
                // TODO: Send GetRooms message to server
            }
            "create" | "2" => {
                println!("Creating room...");
                // TODO: Send CreateRoom message to server
            }
            "join" | "3" => {
                print!("Enter room ID: ");
                io::stdout().flush().unwrap();
                let mut room_id = String::new();
                io::stdin().read_line(&mut room_id).unwrap();
                println!("Joining room {}...", room_id.trim());
                // TODO: Send JoinRoom message to server
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
            }
        }
    }
}
