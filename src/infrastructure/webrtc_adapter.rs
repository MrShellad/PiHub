use crate::domain::models::{AnswerCode, InviteCode};
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use crate::infrastructure::network_bridge::NetworkBridge;
use crate::infrastructure::signaling_client::{self, SignalEvent, SignalingSocket};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::api::APIBuilder;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

pub struct WebRtcEngine {
    current_pc: Mutex<Option<Arc<RTCPeerConnection>>>,
    ipc_emitter: Arc<dyn IpcEmitterPort>,
}

impl WebRtcEngine {
    pub fn new(ipc_emitter: Arc<dyn IpcEmitterPort>) -> Self {
        Self {
            current_pc: Mutex::new(None),
            ipc_emitter,
        }
    }

    async fn build_pc(&self) -> Result<Arc<RTCPeerConnection>> {
        let mut setting_engine = SettingEngine::default();
        setting_engine.detach_data_channels();

        let api = APIBuilder::new()
            .with_setting_engine(setting_engine)
            .build();
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);
        let emitter = Arc::clone(&self.ipc_emitter);
        pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            let emitter = Arc::clone(&emitter);
            let state_text = state.to_string();
            Box::pin(async move {
                emitter.send_log(
                    "INFO",
                    &format!("[WebRTC] PeerConnection state: {state_text}"),
                );
            })
        }));

        Ok(pc)
    }

    async fn encode_local_description(pc: &Arc<RTCPeerConnection>) -> Result<String> {
        let local_desc = pc
            .local_description()
            .await
            .context("failed to get local SDP")?;
        let json_sdp = serde_json::to_string(&local_desc)?;
        Ok(general_purpose::STANDARD.encode(json_sdp))
    }

    async fn apply_answer_if_needed(pc: &Arc<RTCPeerConnection>, answer_b64: &str) -> Result<bool> {
        if pc.remote_description().await.is_some() {
            return Ok(false);
        }

        let answer_json = String::from_utf8(general_purpose::STANDARD.decode(answer_b64)?)?;
        let answer_sdp: RTCSessionDescription = serde_json::from_str(&answer_json)?;
        pc.set_remote_description(answer_sdp).await?;
        Ok(true)
    }

    async fn listen_for_signaled_answer(
        pc: Arc<RTCPeerConnection>,
        mut socket: SignalingSocket,
        ipc_emitter: Arc<dyn IpcEmitterPort>,
    ) {
        loop {
            match signaling_client::recv_event(&mut socket).await {
                Ok(SignalEvent::PlayerAnswer { answer }) => {
                    match Self::apply_answer_if_needed(&pc, &answer).await {
                        Ok(true) => ipc_emitter.send_log(
                            "INFO",
                            "[Signal] Received PLAYER_ANSWER and applied remote SDP",
                        ),
                        Ok(false) => ipc_emitter.send_log(
                            "INFO",
                            "[Signal] PLAYER_ANSWER arrived after remote SDP was already set",
                        ),
                        Err(err) => ipc_emitter.send_log(
                            "ERROR",
                            &format!("[Signal] Failed to apply PLAYER_ANSWER: {err}"),
                        ),
                    }
                    break;
                }
                Ok(SignalEvent::Error { message }) => {
                    ipc_emitter.send_log(
                        "ERROR",
                        &format!(
                            "[Signal] Signaling server error while waiting for answer: {message}"
                        ),
                    );
                    break;
                }
                Ok(other) => {
                    ipc_emitter.send_log(
                        "WARN",
                        &format!("[Signal] Ignoring unexpected signaling event: {other:?}"),
                    );
                }
                Err(err) => {
                    ipc_emitter.send_log(
                        "ERROR",
                        &format!("[Signal] Signaling listener stopped: {err}"),
                    );
                    break;
                }
            }
        }
    }
}

impl P2pPort for WebRtcEngine {
    async fn create_offer(
        &self,
        target_mc_port: u16,
        signaling_server: Option<String>,
    ) -> Result<InviteCode> {
        let pc = self.build_pc().await?;

        NetworkBridge::start_host_bridge(
            Arc::clone(&pc),
            Arc::clone(&self.ipc_emitter),
            target_mc_port,
        )
        .await?;

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;

        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let offer_b64 = Self::encode_local_description(&pc).await?;
        let invite_code = if let Some(server) = signaling_server {
            let mut socket = signaling_client::connect(&server).await?;
            let room_id = signaling_client::create_room(&mut socket, offer_b64).await?;

            let listener_pc = Arc::clone(&pc);
            let listener_emitter = Arc::clone(&self.ipc_emitter);
            tokio::spawn(async move {
                Self::listen_for_signaled_answer(listener_pc, socket, listener_emitter).await;
            });

            self.ipc_emitter.send_log(
                "INFO",
                &format!("[Signal] Room created on signaling server: {room_id}"),
            );
            InviteCode(room_id)
        } else {
            InviteCode(offer_b64)
        };

        *self.current_pc.lock().await = Some(Arc::clone(&pc));
        Ok(invite_code)
    }

    async fn generate_answer(
        &self,
        offer: &InviteCode,
        local_proxy_port: u16,
        signaling_server: Option<String>,
    ) -> Result<AnswerCode> {
        let pc = self.build_pc().await?;

        let (offer_b64, mut signaling_socket) = if let Some(server) = signaling_server {
            let mut socket = signaling_client::connect(&server).await?;
            let offer_b64 = signaling_client::join_room(&mut socket, &offer.0).await?;
            self.ipc_emitter.send_log(
                "INFO",
                &format!("[Signal] Loaded room {} from signaling server", offer.0),
            );
            (offer_b64, Some(socket))
        } else {
            (offer.0.clone(), None)
        };

        NetworkBridge::start_client_bridge(
            Arc::clone(&pc),
            Arc::clone(&self.ipc_emitter),
            local_proxy_port,
        )
        .await?;

        let offer_json = String::from_utf8(general_purpose::STANDARD.decode(&offer_b64)?)?;
        let offer_sdp: RTCSessionDescription = serde_json::from_str(&offer_json)?;
        pc.set_remote_description(offer_sdp).await?;

        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;

        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let answer_b64 = Self::encode_local_description(&pc).await?;

        if let Some(socket) = signaling_socket.as_mut() {
            signaling_client::send_answer(socket, &offer.0, answer_b64.clone()).await?;
            self.ipc_emitter.send_log(
                "INFO",
                &format!("[Signal] Sent answer back to signaling room {}", offer.0),
            );
        }

        *self.current_pc.lock().await = Some(Arc::clone(&pc));
        Ok(AnswerCode(answer_b64))
    }

    async fn accept_answer(&self, answer: &AnswerCode) -> Result<()> {
        let pc_guard = self.current_pc.lock().await;
        let pc = pc_guard
            .as_ref()
            .context("no active peer connection for accept_answer")?;

        let applied = Self::apply_answer_if_needed(pc, &answer.0).await?;
        if !applied {
            self.ipc_emitter.send_log(
                "INFO",
                "[WebRTC] Remote answer already applied, skipping duplicate HOST_ACCEPT_ANSWER",
            );
        }

        Ok(())
    }
}
