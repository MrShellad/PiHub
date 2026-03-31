use super::models::{AnswerCode, InviteCode};
use anyhow::Result;
use std::future::Future;

pub trait P2pPort: Send + Sync {
    fn create_offer(
        &self,
        target_mc_port: u16,
        signaling_server: Option<String>,
    ) -> impl Future<Output = Result<InviteCode>> + Send;

    fn generate_answer(
        &self,
        offer: &InviteCode,
        local_proxy_port: u16,
        signaling_server: Option<String>,
    ) -> impl Future<Output = Result<AnswerCode>> + Send;

    fn accept_answer(&self, answer: &AnswerCode) -> impl Future<Output = Result<()>> + Send;
}

pub trait IpcEmitterPort: Send + Sync {
    fn send_response(&self, req_id: &str, status: &str, data: serde_json::Value);
    fn send_event(&self, event_name: &str, data: serde_json::Value);
    fn send_log(&self, level: &str, message: &str);
}
