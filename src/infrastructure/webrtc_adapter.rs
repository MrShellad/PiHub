use crate::domain::models::{AnswerCode, InviteCode};
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use crate::infrastructure::network_bridge::NetworkBridge;
use crate::infrastructure::signaling_client::{SignalEvent, SignalRequest, SignalingSocket};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use base64::{Engine as _, engine::general_purpose};

pub struct WebRtcEngine {
    current_pcs: Mutex<HashMap<String, Arc<RTCPeerConnection>>>,
    ipc_emitter: Arc<dyn IpcEmitterPort>,
}

impl WebRtcEngine {
    pub fn new(ipc_emitter: Arc<dyn IpcEmitterPort>) -> Self {
        Self {
            current_pcs: Mutex::new(HashMap::new()),
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

    async fn apply_answer_if_needed(
        pc: &Arc<RTCPeerConnection>,
        answer_json: &str,
    ) -> Result<bool> {
        if pc.remote_description().await.is_some() {
            return Ok(false);
        }
        let answer_sdp: RTCSessionDescription = serde_json::from_str(answer_json)?;
        pc.set_remote_description(answer_sdp).await?;
        Ok(true)
    }

    async fn apply_ice_candidate(
        pc: &Arc<RTCPeerConnection>,
        candidate_json: &str,
    ) -> Result<()> {
        let candidate: RTCIceCandidateInit = serde_json::from_str(candidate_json)?;
        pc.add_ice_candidate(candidate).await?;
        Ok(())
    }

    async fn host_loop(
        engine: Arc<Self>,
        socket: SignalingSocket,
        room_id: String,
        target_mc_port: u16,
    ) {
        let (mut sender, mut receiver) = socket.split();
        let (tx, mut rx) = mpsc::channel::<Message>(100);

        let tx_send_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let _ = sender.send(msg).await;
            }
        });

        while let Some(Ok(message)) = receiver.next().await {
            match message {
                Message::Text(text) => {
                    if let Ok(event) = serde_json::from_str::<SignalEvent>(&text) {
                        match event {
                            SignalEvent::PeerJoined { client_id } => {
                                engine.ipc_emitter.send_log("INFO", &format!("[Signal] New Client Joined: {}", client_id));
                                
                                // Boot new PC
                                if let Ok(pc) = engine.build_pc().await {
                                    // Link network bridge
                                    if let Err(e) = NetworkBridge::start_host_bridge(Arc::clone(&pc), Arc::clone(&engine.ipc_emitter), target_mc_port).await {
                                        engine.ipc_emitter.send_log("ERROR", &format!("Host bridge failed: {}", e));
                                        continue;
                                    }

                                    engine.current_pcs.lock().await.insert(client_id.clone(), Arc::clone(&pc));

                                    // Setup ICE gather
                                    let tx_ice = tx.clone();
                                    let room_ice = room_id.clone();
                                    let client_ice = client_id.clone();
                                    pc.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
                                        let tx_ice = tx_ice.clone();
                                        let room_ice = room_ice.clone();
                                        let client_ice = client_ice.clone();
                                        Box::pin(async move {
                                            if let Some(candidate) = c {
                                                if let Ok(json) = candidate.to_json() {
                                                    let json_str = serde_json::to_string(&json).unwrap_or_default();
                                                    let req = SignalRequest::SendIceCandidate {
                                                        room_id: room_ice,
                                                        target_id: client_ice,
                                                        candidate: json_str,
                                                    };
                                                    if let Ok(payload) = serde_json::to_string(&req) {
                                                        let _ = tx_ice.send(Message::Text(payload.into())).await;
                                                    }
                                                }
                                            }
                                        })
                                    }));

                                    // Create Offer
                                    if let Ok(offer) = pc.create_offer(None).await {
                                        if pc.set_local_description(offer.clone()).await.is_ok() {
                                            if let Ok(offer_json) = serde_json::to_string(&offer) {
                                                let req = SignalRequest::SendOffer {
                                                    room_id: room_id.clone(),
                                                    target_id: client_id,
                                                    offer: offer_json,
                                                };
                                                if let Ok(payload) = serde_json::to_string(&req) {
                                                    let _ = tx.send(Message::Text(payload.into())).await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            SignalEvent::PlayerAnswer { from_id, answer } => {
                                if let Some(pc) = engine.current_pcs.lock().await.get(&from_id) {
                                    let _ = Self::apply_answer_if_needed(pc, &answer).await;
                                }
                            }
                            SignalEvent::IceCandidate { from_id, candidate } => {
                                if let Some(pc) = engine.current_pcs.lock().await.get(&from_id) {
                                    let _ = Self::apply_ice_candidate(pc, &candidate).await;
                                }
                            }
                            SignalEvent::PeerLeft { client_id } => {
                                engine.current_pcs.lock().await.remove(&client_id);
                                engine.ipc_emitter.send_log("INFO", &format!("[Signal] Client Left: {}", client_id));
                            }
                            SignalEvent::Error { message } => {
                                engine.ipc_emitter.send_log("ERROR", &format!("[Signal] Error: {}", message));
                            }
                            _ => {}
                        }
                    }
                }
                Message::Ping(payload) => {
                    let _ = tx.send(Message::Pong(payload)).await;
                }
                _ => {}
            }
        }
        tx_send_task.abort();
    }

    async fn client_loop(
        engine: Arc<Self>,
        socket: SignalingSocket,
        room_id: String,
        local_proxy_port: u16,
    ) {
        let (mut sender, mut receiver) = socket.split();
        let (tx, mut rx) = mpsc::channel::<Message>(100);

        let tx_send_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let _ = sender.send(msg).await;
            }
        });

        // Fire Join
        let join_req = SignalRequest::JoinRoom { room_id: room_id.clone() };
        if let Ok(payload) = serde_json::to_string(&join_req) {
            let _ = tx.send(Message::Text(payload.into())).await;
        }

        while let Some(Ok(message)) = receiver.next().await {
            match message {
                Message::Text(text) => {
                    if let Ok(event) = serde_json::from_str::<SignalEvent>(&text) {
                        match event {
                            SignalEvent::RoomOffer { from_id, offer } => {
                                engine.ipc_emitter.send_log("INFO", "[Signal] Received Host Offer");
                                
                                if let Ok(pc) = engine.build_pc().await {
                                    if let Err(e) = NetworkBridge::start_client_bridge(Arc::clone(&pc), Arc::clone(&engine.ipc_emitter), local_proxy_port).await {
                                        engine.ipc_emitter.send_log("ERROR", &format!("Client bridge failed: {}", e));
                                        continue;
                                    }

                                    engine.current_pcs.lock().await.insert("host".to_string(), Arc::clone(&pc));

                                    // Setup ICE gather
                                    let tx_ice = tx.clone();
                                    let room_ice = room_id.clone();
                                    let host_id = from_id.clone();
                                    pc.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
                                        let tx_ice = tx_ice.clone();
                                        let room_ice = room_ice.clone();
                                        let host_id = host_id.clone();
                                        Box::pin(async move {
                                            if let Some(candidate) = c {
                                                if let Ok(json) = candidate.to_json() {
                                                    let json_str = serde_json::to_string(&json).unwrap_or_default();
                                                    let req = SignalRequest::SendIceCandidate {
                                                        room_id: room_ice,
                                                        target_id: host_id,
                                                        candidate: json_str,
                                                    };
                                                    if let Ok(payload) = serde_json::to_string(&req) {
                                                        let _ = tx_ice.send(Message::Text(payload.into())).await;
                                                    }
                                                }
                                            }
                                        })
                                    }));

                                    // Set Remote Description and create Answer
                                    if let Ok(offer_sdp) = serde_json::from_str::<RTCSessionDescription>(&offer) {
                                        if pc.set_remote_description(offer_sdp).await.is_ok() {
                                            if let Ok(answer) = pc.create_answer(None).await {
                                                if pc.set_local_description(answer.clone()).await.is_ok() {
                                                    if let Ok(answer_json) = serde_json::to_string(&answer) {
                                                        let req = SignalRequest::SendAnswer {
                                                            room_id: room_id.clone(),
                                                            target_id: from_id.clone(),
                                                            answer: answer_json,
                                                        };
                                                        if let Ok(payload) = serde_json::to_string(&req) {
                                                            let _ = tx.send(Message::Text(payload.into())).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            SignalEvent::IceCandidate { from_id: _, candidate } => {
                                if let Some(pc) = engine.current_pcs.lock().await.get("host") {
                                    let _ = Self::apply_ice_candidate(pc, &candidate).await;
                                }
                            }
                            SignalEvent::Error { message } => {
                                engine.ipc_emitter.send_log("ERROR", &format!("[Signal] Error: {}", message));
                            }
                            _ => {}
                        }
                    }
                }
                Message::Ping(payload) => {
                    let _ = tx.send(Message::Pong(payload)).await;
                }
                _ => {}
            }
        }
        tx_send_task.abort();
    }
}

impl P2pPort for WebRtcEngine {
    async fn create_offer(
        &self,
        target_mc_port: u16,
        signaling_server: Option<String>,
    ) -> Result<InviteCode> {
        if let Some(server) = signaling_server {
            let mut socket = crate::infrastructure::signaling_client::connect(&server).await?;
            let room_id = crate::infrastructure::signaling_client::create_room(&mut socket).await?;

            self.ipc_emitter.send_log(
                "INFO",
                &format!("[Signal] Room {} created. Awaiting clients.", room_id),
            );

            // Create host loop daemon
            let engine_arc = Arc::new(Self {
                current_pcs: Mutex::new(HashMap::new()), 
                ipc_emitter: Arc::clone(&self.ipc_emitter),
            });
            
            let room_id_clone = room_id.clone();
            tokio::spawn(async move {
                Self::host_loop(engine_arc, socket, room_id_clone, target_mc_port).await;
            });

            return Ok(InviteCode(room_id));
        }

        // Fallback for manual mode (Legacy 1-to-1 prep)
        let pc = self.build_pc().await?;
        NetworkBridge::start_host_bridge(
            Arc::clone(&pc),
            Arc::clone(&self.ipc_emitter),
            target_mc_port,
        ).await?;

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;

        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let local_desc = pc.local_description().await.context("failed to get local SDP")?;
        let json_sdp = serde_json::to_string(&local_desc)?;
        let offer_b64 = general_purpose::STANDARD.encode(json_sdp);

        self.current_pcs.lock().await.insert("manual".to_string(), Arc::clone(&pc));
        Ok(InviteCode(offer_b64))
    }

    async fn generate_answer(
        &self,
        offer: &InviteCode,
        local_proxy_port: u16,
        signaling_server: Option<String>,
    ) -> Result<AnswerCode> {
        if let Some(server) = signaling_server {
            let socket = crate::infrastructure::signaling_client::connect(&server).await?;
            
            self.ipc_emitter.send_log(
                "INFO",
                &format!("[Signal] Joining room {} on server.", offer.0),
            );

            let engine_arc = Arc::new(Self {
                current_pcs: Mutex::new(HashMap::new()),
                ipc_emitter: Arc::clone(&self.ipc_emitter),
            });

            let room_id = offer.0.clone();
            tokio::spawn(async move {
                Self::client_loop(engine_arc, socket, room_id, local_proxy_port).await;
            });

            return Ok(AnswerCode("dynamic_signaling".to_string()));
        }

        // Fallback manual mode
        let pc = self.build_pc().await?;
        NetworkBridge::start_client_bridge(
            Arc::clone(&pc),
            Arc::clone(&self.ipc_emitter),
            local_proxy_port,
        ).await?;

        let offer_json = String::from_utf8(general_purpose::STANDARD.decode(&offer.0)?)?;
        let offer_sdp: RTCSessionDescription = serde_json::from_str(&offer_json)?;
        pc.set_remote_description(offer_sdp).await?;

        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;

        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let local_desc = pc.local_description().await.context("failed to get local SDP")?;
        let json_sdp = serde_json::to_string(&local_desc)?;
        let answer_b64 = general_purpose::STANDARD.encode(json_sdp);

        self.current_pcs.lock().await.insert("manual".to_string(), Arc::clone(&pc));
        Ok(AnswerCode(answer_b64))
    }

    async fn accept_answer(&self, answer: &AnswerCode) -> Result<()> {
        if answer.0 == "dynamic_signaling" {
            return Ok(()); // Handled natively by listener
        }

        let pc_guard = self.current_pcs.lock().await;
        let pc = pc_guard
            .get("manual")
            .context("no PC for manual accept_answer")?;

        let answer_json = String::from_utf8(general_purpose::STANDARD.decode(&answer.0)?)?;
        let _ = Self::apply_answer_if_needed(pc, &answer_json).await?;
        Ok(())
    }
}
