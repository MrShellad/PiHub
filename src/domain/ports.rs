use super::models::{AnswerCode, InviteCode};
use anyhow::Result;

/// ==========================================
/// 🔌 核心网络引擎出端口 (P2pPort)
/// ==========================================
/// 任何想要作为底层打洞引擎的模块，都必须实现这个接口。
/// 未来如果从 WebRTC 换成 QUIC，只需提供一个新的实现即可。
pub trait P2pPort: Send + Sync {
    /// 房主：生成建房邀请码
    async fn create_offer(&self) -> Result<InviteCode>;
    
    /// 客户端：根据邀请码加入，并生成回应码
    async fn generate_answer(&self, offer: &InviteCode) -> Result<AnswerCode>;
    
    /// 房主：接收客户端的回应码，彻底打通隧道
    async fn accept_answer(&self, answer: &AnswerCode) -> Result<()>;
}

/// ==========================================
/// 📢 进程间通信出端口 (IpcEmitterPort)
/// ==========================================
/// 负责向启动器发送状态。不管是走 Stdio 还是 WebSocket，都必须符合这个契约。
pub trait IpcEmitterPort: Send + Sync {
    /// 发送对某条指令的明确响应
    fn send_response(&self, req_id: &str, status: &str, data: serde_json::Value);
    
    /// 发送无需请求的异步事件 (如打洞成功、玩家加入)
    fn send_event(&self, event_name: &str, data: serde_json::Value);
    
    /// 发送标准化结构日志
    fn send_log(&self, level: &str, message: &str);
}