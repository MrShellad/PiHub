use crate::domain::models::{AnswerCode, InviteCode};
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use std::sync::Arc;

pub struct SessionManager<P: P2pPort, I: IpcEmitterPort> {
    p2p_engine: Arc<P>,
    ipc_emitter: Arc<I>,
}

impl<P: P2pPort, I: IpcEmitterPort> SessionManager<P, I> {
    pub fn new(p2p_engine: Arc<P>, ipc_emitter: Arc<I>) -> Self {
        Self {
            p2p_engine,
            ipc_emitter,
        }
    }

    pub async fn handle_create_room(
        &self,
        req_id: String,
        target_mc_port: u16,
        signaling_server: Option<String>,
    ) {
        self.ipc_emitter.send_log(
            "INFO",
            &format!(
                "Received CREATE_ROOM, target_mc_port={target_mc_port}, signaling_server={}",
                signaling_server.as_deref().unwrap_or("manual"),
            ),
        );

        match self
            .p2p_engine
            .create_offer(target_mc_port, signaling_server)
            .await
        {
            Ok(invite_code) => {
                self.ipc_emitter
                    .send_log("INFO", "Room created successfully and invite code is ready");
                self.ipc_emitter.send_response(
                    &req_id,
                    "success",
                    serde_json::json!({ "invite_code": invite_code.0 }),
                );
            }
            Err(err) => {
                self.ipc_emitter
                    .send_log("ERROR", &format!("CREATE_ROOM failed: {err}"));
                self.ipc_emitter
                    .send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }

    pub async fn handle_join_room(
        &self,
        req_id: String,
        invite_code_str: String,
        local_proxy_port: u16,
        signaling_server: Option<String>,
    ) {
        self.ipc_emitter.send_log(
            "INFO",
            &format!(
                "Received JOIN_ROOM, local_proxy_port={local_proxy_port}, signaling_server={}",
                signaling_server.as_deref().unwrap_or("manual"),
            ),
        );

        let invite_code = InviteCode(invite_code_str);
        match self
            .p2p_engine
            .generate_answer(&invite_code, local_proxy_port, signaling_server)
            .await
        {
            Ok(answer_code) => {
                self.ipc_emitter
                    .send_log("INFO", "JOIN_ROOM succeeded and the answer is ready");
                self.ipc_emitter.send_response(
                    &req_id,
                    "success",
                    serde_json::json!({ "answer_code": answer_code.0 }),
                );
            }
            Err(err) => {
                self.ipc_emitter
                    .send_log("ERROR", &format!("JOIN_ROOM failed: {err}"));
                self.ipc_emitter
                    .send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }

    pub async fn handle_accept_answer(&self, req_id: String, answer_code_str: String) {
        self.ipc_emitter
            .send_log("INFO", "Received HOST_ACCEPT_ANSWER");

        let answer_code = AnswerCode(answer_code_str);
        match self.p2p_engine.accept_answer(&answer_code).await {
            Ok(()) => {
                self.ipc_emitter
                    .send_log("INFO", "Remote answer applied successfully");
                self.ipc_emitter
                    .send_response(&req_id, "success", serde_json::json!({}));
            }
            Err(err) => {
                self.ipc_emitter
                    .send_log("ERROR", &format!("HOST_ACCEPT_ANSWER failed: {err}"));
                self.ipc_emitter
                    .send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }

    pub async fn handle_get_signaling_servers(&self, req_id: String) {
        self.ipc_emitter
            .send_log("INFO", "Received GET_SIGNALING_SERVERS request");

        let config = crate::env::SignalingEnvConfig::get_config().await;
        // Just serialize the whole thing and send it over IPC
        self.ipc_emitter.send_response(&req_id, "success", serde_json::to_value(config).unwrap());
    }
}
