use std::sync::Arc;
use crate::domain::models::{AnswerCode, InviteCode};
use crate::domain::ports::{IpcEmitterPort, P2pPort};

/// 房间会话管理器：负责串联底层网络和 IPC 通信
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

    /// 用例 1：处理启动器发来的建房请求
    pub async fn handle_create_room(&self, req_id: String) {
        self.ipc_emitter.send_log("INFO", "收到 CREATE_ROOM 指令，开始构建 P2P 房间...");

        // 调用领域端口，执行真正的打洞逻辑
        match self.p2p_engine.create_offer().await {
            Ok(invite_code) => {
                self.ipc_emitter.send_log("INFO", "房间创建成功，已生成邀请码");
                // 将结果封装成标准响应发回给启动器
                self.ipc_emitter.send_response(
                    &req_id,
                    "success",
                    serde_json::json!({ "invite_code": invite_code.0 }),
                );
            }
            Err(e) => {
                self.ipc_emitter.send_log("ERROR", &format!("建房失败: {}", e));
                self.ipc_emitter.send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }

    /// 用例 2：处理客户端加入房间请求
    pub async fn handle_join_room(&self, req_id: String, invite_code_str: String) {
        self.ipc_emitter.send_log("INFO", "收到 JOIN_ROOM 指令，正在解析邀请码...");
        let invite_code = InviteCode(invite_code_str);

        match self.p2p_engine.generate_answer(&invite_code).await {
            Ok(answer_code) => {
                self.ipc_emitter.send_log("INFO", "成功生成回应码，等待房主确认");
                self.ipc_emitter.send_response(
                    &req_id,
                    "success",
                    serde_json::json!({ "answer_code": answer_code.0 }),
                );
            }
            Err(e) => {
                self.ipc_emitter.send_log("ERROR", &format!("加入房间失败: {}", e));
                self.ipc_emitter.send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }

    /// 用例 3：处理房主确认握手请求
    pub async fn handle_accept_answer(&self, req_id: String, answer_code_str: String) {
        self.ipc_emitter.send_log("INFO", "收到 HOST_ACCEPT_ANSWER 指令，正在彻底打通隧道...");
        let answer_code = AnswerCode(answer_code_str);

        match self.p2p_engine.accept_answer(&answer_code).await {
            Ok(_) => {
                self.ipc_emitter.send_log("INFO", "隧道底层握手完成！");
                self.ipc_emitter.send_response(&req_id, "success", serde_json::json!({}));
            }
            Err(e) => {
                self.ipc_emitter.send_log("ERROR", &format!("握手确认失败: {}", e));
                self.ipc_emitter.send_response(&req_id, "error", serde_json::json!({}));
            }
        }
    }
}