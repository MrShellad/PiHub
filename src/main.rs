mod application;
mod domain;
mod env;
mod infrastructure;
mod presentation;

use application::use_cases::SessionManager;
use domain::ports::IpcEmitterPort;
use infrastructure::{stdio_adapter::StdioEmitter, webrtc_adapter::WebRtcEngine};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // 1. 初始化基础设施：IPC 发射器 (向启动器发消息)
    let ipc_emitter = Arc::new(StdioEmitter::new());

    // 2. 初始化基础设施：底层 P2P 引擎
    // 注意：我们将 ipc_emitter 注入给了 WebRtcEngine，让它有能力在底层网络状态变化时，直接向上层抛出事件
    let p2p_engine = Arc::new(WebRtcEngine::new(
        Arc::clone(&ipc_emitter) as Arc<dyn IpcEmitterPort>
    ));

    // 3. 初始化应用服务层 (业务大脑)
    // 把 P2P 打洞能力和 IPC 通信能力赋予它
    let session_manager = Arc::new(SessionManager::new(
        Arc::clone(&p2p_engine),
        Arc::clone(&ipc_emitter),
    ));

    // 启动宣告
    ipc_emitter.send_log(
        "INFO",
        "🚀 NetCore-Sidecar (DDD 架构重构版) 已就绪，等待启动器指令...",
    );

    // 4. 启动表现层事件循环 (监听标准输入)，接管主线程
    presentation::ipc_listener::start_event_loop(session_manager).await;
}
