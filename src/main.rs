use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

// ==========================================
// 1. IPC 协议数据结构
// ==========================================
#[derive(Deserialize, Debug)]
struct RpcRequest {
    req_id: String,
    action: String,
    payload: serde_json::Value,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type")]
enum RpcOutput {
    #[serde(rename = "response")]
    Response { req_id: String, status: String, data: serde_json::Value },
    #[serde(rename = "event")]
    Event { event_name: String, data: serde_json::Value },
    #[serde(rename = "log")]
    Log { level: String, message: String },
}

fn send_to_launcher(output: RpcOutput) {
    if let Ok(json_str) = serde_json::to_string(&output) {
        println!("{}", json_str);
    }
}

// ==========================================
// 2. WebRTC 核心引擎逻辑
// ==========================================

/// 初始化 WebRTC PeerConnection (带公共 STUN/TURN 配置)
async fn create_peer_connection() -> Result<Arc<RTCPeerConnection>> {
    let api = APIBuilder::new().build();
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()], // TODO: 换成国内可用的 STUN
            ..Default::default()
        }],
        ..Default::default()
    };
    let pc = api.new_peer_connection(config).await?;
    
    // 监听底层连接状态变化，推送到启动器
    pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        let state_str = s.to_string();
        Box::pin(async move {
            send_to_launcher(RpcOutput::Log {
                level: "INFO".to_string(),
                message: format!("📶 底层 P2P 状态变更: {}", state_str),
            });
            if s == RTCPeerConnectionState::Connected {
                send_to_launcher(RpcOutput::Event {
                    event_name: "TUNNEL_READY".to_string(),
                    data: serde_json::json!({"status": "connected"}),
                });
            }
        })
    }));

    Ok(Arc::new(pc))
}

/// 房主：生成建房邀请码 (Offer SDP)
async fn generate_offer(pc: Arc<RTCPeerConnection>) -> Result<String> {
    // 核心细节：必须在创建 Offer 前创建 DataChannel，否则 SDP 不包含数据通道信息
    let _dc = pc.create_data_channel("mc_tcp_tunnel", Some(RTCDataChannelInit {
        ordered: Some(true), // 保证 TCP 顺序
        ..Default::default()
    })).await?;

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;

    // Vanilla ICE 魔法：阻塞等待本地网络所有候选者 (IP) 收集完毕
    let mut gather_complete = pc.gathering_complete_promise().await;
    let _ = gather_complete.recv().await;

    // 此时拿到的 SDP 包含了完整的内网/外网 IP 候选者
    let local_desc = pc.local_description().await.context("无法获取 LocalDescription")?;
    let json_sdp = serde_json::to_string(&local_desc)?;
    Ok(general_purpose::STANDARD.encode(json_sdp))
}

/// 客户端：加入房间，解析邀请码并生成回应 (Answer SDP)
async fn generate_answer(pc: Arc<RTCPeerConnection>, offer_b64: &str) -> Result<String> {
    // 客户端监听 DataChannel 的开启事件 (这里将是未来 TCP 流量劫持的入口)
    pc.on_data_channel(Box::new(move |d| {
        let label = d.label().to_owned();
        Box::pin(async move {
            send_to_launcher(RpcOutput::Log {
                level: "INFO".to_string(),
                message: format!("📦 数据通道已开启: {}", label),
            });
            // TODO: 这里将来对接 tokio::io::copy_bidirectional 连接 MC 端口
        })
    }));

    // 解码房主的邀请码
    let offer_json = String::from_utf8(general_purpose::STANDARD.decode(offer_b64)?)?;
    let offer_sdp: RTCSessionDescription = serde_json::from_str(&offer_json)?;
    pc.set_remote_description(offer_sdp).await?;

    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;

    // 同样等待客户端本地 IP 收集完毕
    let mut gather_complete = pc.gathering_complete_promise().await;
    let _ = gather_complete.recv().await;

    let local_desc = pc.local_description().await.context("无法获取 LocalDescription")?;
    let json_sdp = serde_json::to_string(&local_desc)?;
    Ok(general_purpose::STANDARD.encode(json_sdp))
}

/// 房主：接收客户端的回应码，彻底打通隧道
async fn host_accept_answer(pc: Arc<RTCPeerConnection>, answer_b64: &str) -> Result<()> {
    let answer_json = String::from_utf8(general_purpose::STANDARD.decode(answer_b64)?)?;
    let answer_sdp: RTCSessionDescription = serde_json::from_str(&answer_json)?;
    pc.set_remote_description(answer_sdp).await?;
    Ok(())
}

// ==========================================
// 3. Stdio 异步监听主循环
// ==========================================

#[tokio::main]
async fn main() -> Result<()> {
    send_to_launcher(RpcOutput::Log { level: "INFO".to_string(), message: "🚀 NetCore-Sidecar (WebRTC) 就绪".to_string() });

    // 引擎生命周期内全局维护的连接对象
    let mut current_pc: Option<Arc<RTCPeerConnection>> = None;

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(req) = serde_json::from_str::<RpcRequest>(&line) {
            match req.action.as_str() {
                "CREATE_ROOM" => {
                    let pc = create_peer_connection().await?;
                    current_pc = Some(Arc::clone(&pc));
                    
                    match generate_offer(pc).await {
                        Ok(invite_code) => {
                            send_to_launcher(RpcOutput::Response {
                                req_id: req.req_id, status: "success".to_string(),
                                data: serde_json::json!({ "invite_code": invite_code }),
                            });
                        }
                        Err(e) => send_to_launcher(RpcOutput::Log { level: "ERROR".to_string(), message: e.to_string() })
                    }
                }
                "JOIN_ROOM" => {
                    let pc = create_peer_connection().await?;
                    current_pc = Some(Arc::clone(&pc));
                    
                    if let Some(invite_code) = req.payload.get("invite_code").and_then(|v| v.as_str()) {
                        match generate_answer(pc, invite_code).await {
                            Ok(answer_code) => {
                                send_to_launcher(RpcOutput::Response {
                                    req_id: req.req_id, status: "success".to_string(),
                                    data: serde_json::json!({ "answer_code": answer_code }), // 客户端生成的回复码
                                });
                            }
                            Err(e) => send_to_launcher(RpcOutput::Log { level: "ERROR".to_string(), message: e.to_string() })
                        }
                    }
                }
                "HOST_ACCEPT_ANSWER" => {
                    // 房主拿到客户端的 Answer 后，执行此步骤完成打洞
                    if let Some(pc) = current_pc.as_ref() {
                        if let Some(answer_code) = req.payload.get("answer_code").and_then(|v| v.as_str()) {
                            if let Err(e) = host_accept_answer(Arc::clone(pc), answer_code).await {
                                send_to_launcher(RpcOutput::Log { level: "ERROR".to_string(), message: e.to_string() });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}