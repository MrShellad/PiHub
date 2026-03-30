use crate::application::use_cases::SessionManager;
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, BufReader};

/// 接收自启动器的 JSON-RPC 请求结构
#[derive(Deserialize, Debug)]
struct RpcRequest {
    req_id: String,
    action: String,
    payload: serde_json::Value,
}

/// 启动 IPC 事件循环
/// 泛型约束：只要是实现了对应 Port 接口的 SessionManager 都可以传入
pub async fn start_event_loop<P: P2pPort + 'static, I: IpcEmitterPort + 'static>(
    app_service: Arc<SessionManager<P, I>>,
) {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    // 核心循环：异步等待启动器从 stdin 写入的每一行指令
    while let Ok(Some(line)) = reader.next_line().await {
        // 尝试解析 JSON
        if let Ok(req) = serde_json::from_str::<RpcRequest>(&line) {
            // 克隆 Arc 指针，准备跨任务移动
            let app_clone = Arc::clone(&app_service);

            // ⚡ 核心解耦：为每一条指令开启独立的绿色线程 (Task)，绝不阻塞主接收循环
            tokio::spawn(async move {
                match req.action.as_str() {
                    "CREATE_ROOM" => {
                        app_clone.handle_create_room(req.req_id).await;
                    }
                    "JOIN_ROOM" => {
                        if let Some(invite_code) = req.payload.get("invite_code").and_then(|v| v.as_str()) {
                            app_clone.handle_join_room(req.req_id, invite_code.to_string()).await;
                        }
                    }
                    "HOST_ACCEPT_ANSWER" => {
                        if let Some(answer_code) = req.payload.get("answer_code").and_then(|v| v.as_str()) {
                            app_clone.handle_accept_answer(req.req_id, answer_code.to_string()).await;
                        }
                    }
                    _ => {
                        // 未知指令，忽略或记录（这部分日志可以通过 IpcEmitter 走，这里从简）
                    }
                }
            });
        }
    }
}