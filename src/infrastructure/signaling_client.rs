use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

pub type SignalingSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Serialize)]
#[serde(tag = "action")]
pub enum SignalRequest {
    #[serde(rename = "CREATE_ROOM")]
    CreateRoom,
    #[serde(rename = "JOIN_ROOM")]
    JoinRoom { room_id: String },
    #[serde(rename = "SEND_OFFER")]
    SendOffer { room_id: String, target_id: String, offer: String },
    #[serde(rename = "SEND_ANSWER")]
    SendAnswer { room_id: String, target_id: String, answer: String },
    #[serde(rename = "SEND_ICE_CANDIDATE")]
    SendIceCandidate { room_id: String, target_id: String, candidate: String },
    #[serde(rename = "LEAVE_ROOM")]
    LeaveRoom { room_id: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event")]
pub enum SignalEvent {
    #[serde(rename = "ROOM_CREATED")]
    RoomCreated { room_id: String },
    #[serde(rename = "PEER_JOINED")]
    PeerJoined { client_id: String },
    #[serde(rename = "PEER_LEFT")]
    PeerLeft { client_id: String },
    #[serde(rename = "ROOM_OFFER")]
    RoomOffer { from_id: String, offer: String },
    #[serde(rename = "PLAYER_ANSWER")]
    PlayerAnswer { from_id: String, answer: String },
    #[serde(rename = "ICE_CANDIDATE")]
    IceCandidate { from_id: String, candidate: String },
    #[serde(rename = "ERROR")]
    Error { message: String },
}

pub async fn connect(server: &str) -> Result<SignalingSocket> {
    let ws_url = normalize_signaling_url(server)?;
    let (socket, _) = connect_async(ws_url.as_str())
        .await
        .with_context(|| format!("failed to connect signaling server {ws_url}"))?;
    Ok(socket)
}

pub async fn create_room(socket: &mut SignalingSocket) -> Result<String> {
    send_request(socket, SignalRequest::CreateRoom).await?;
    match recv_event(socket).await? {
        SignalEvent::RoomCreated { room_id } => Ok(room_id),
        SignalEvent::Error { message } => bail!("signaling server error: {message}"),
        event => bail!("unexpected signaling event while creating room: {event:?}"),
    }
}

pub async fn join_room(socket: &mut SignalingSocket, room_id: &str) -> Result<()> {
    send_request(
        socket,
        SignalRequest::JoinRoom {
            room_id: room_id.to_owned(),
        },
    )
    .await
}

pub async fn send_request(socket: &mut SignalingSocket, request: SignalRequest) -> Result<()> {
    let payload = serde_json::to_string(&request).context("failed to encode signaling request")?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .context("failed to send signaling request")
}

pub async fn recv_event(socket: &mut SignalingSocket) -> Result<SignalEvent> {
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| anyhow!("signaling socket closed"))?
            .context("failed to read signaling message")?;

        match message {
            Message::Text(text) => {
                return serde_json::from_str::<SignalEvent>(&text)
                    .context("failed to parse signaling event");
            }
            Message::Close(_) => bail!("signaling socket closed"),
            Message::Ping(payload) => socket
                .send(Message::Pong(payload))
                .await
                .context("failed to answer signaling ping")?,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn normalize_signaling_url(server: &str) -> Result<String> {
    let mut url = server.trim().to_owned();
    if url.is_empty() {
        bail!("signaling server is empty");
    }

    if let Some(stripped) = url.strip_prefix("http://") {
        url = format!("ws://{stripped}");
    } else if let Some(stripped) = url.strip_prefix("https://") {
        url = format!("wss://{stripped}");
    } else if !url.starts_with("ws://") && !url.starts_with("wss://") {
        url = format!("ws://{url}");
    }

    let scheme_end = url
        .find("://")
        .ok_or_else(|| anyhow!("invalid signaling server URL: {url}"))?
        + 3;
    let path_index = url[scheme_end..].find('/').map(|idx| scheme_end + idx);

    match path_index {
        None => url.push_str("/ws"),
        Some(idx) if &url[idx..] == "/" => url.push_str("ws"),
        _ => {}
    }

    Ok(url)
}
