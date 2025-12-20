use std::collections::HashSet;

use axum::extract::ws::{self, Utf8Bytes};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use webrtc::{
    ice_transport::ice_candidate::RTCIceCandidate,
    peer_connection::sdp::session_description::RTCSessionDescription,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: u32,
    // TODO: ...
    // max_size: usize,
    // password: Option<String>,
    pub host_id: Uuid,
    pub guest_ids: HashSet<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageFromClient {
    StartGame,

    // Room CRUD
    GetRooms,
    CreateRoom,
    JoinRoom {
        room_id: u32,
    },
    DeleteRoom {
        room_id: u32,
    },

    // WebRTC
    Offer {
        to_client_id: Uuid,
        from_client_id: Uuid,
        offer: RTCSessionDescription,
    },
    Answer {
        to_client_id: Uuid,
        from_client_id: Uuid,
        answer: RTCSessionDescription,
    },
    IceCandidate {
        to_client_id: Uuid,
        from_client_id: Uuid,
        candidate: RTCIceCandidate,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageFromServer {
    Ok,
    BadRequest,
    ClientNotFound,
    Disconnect { reason: DisconnectReason },
    Rooms { rooms: Vec<Room> },
    RoomCreated { room_id: u32 },
    GuestJoined { guest_id: Uuid },
    JoinedRoom { room: Room },
    HostDeletedRoom,
}

impl From<MessageFromServer> for ws::Message {
    fn from(msg_from_server: MessageFromServer) -> Self {
        let text: Utf8Bytes = serde_json::to_string(&msg_from_server)
            .expect("MessageFromServer should serialize")
            .into();
        Self::Text(text)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DisconnectReason {
    InvalidClientId,
    ClientIdTaken,
    HostStartedGame,
}
