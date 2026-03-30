use super::models::{AnswerCode, InviteCode};
use anyhow::Result;
use std::future::Future; // 引入 Future

/// ==========================================
/// 🔌 核心网络引擎出端口 (P2pPort)
/// ==========================================
pub trait P2pPort: Send + Sync {
    /// 房主：生成建房邀请码
    fn create_offer(&self) -> impl Future<Output = Result<InviteCode>> + Send;
    
    /// 客户端：根据邀请码加入，并生成回应码
    fn generate_answer(&self, offer: &InviteCode) -> impl Future<Output = Result<AnswerCode>> + Send;
    
    /// 房主：接收客户端的回应码，彻底打通隧道
    fn accept_answer(&self, answer: &AnswerCode) -> impl Future<Output = Result<()>> + Send;
}

/// ==========================================
/// 📢 进程间通信出端口 (IpcEmitterPort)
/// ==========================================
pub trait IpcEmitterPort: Send + Sync {
    fn send_response(&self, req_id: &str, status: &str, data: serde_json::Value);
    fn send_event(&self, event_name: &str, data: serde_json::Value);
    fn send_log(&self, level: &str, message: &str);
}