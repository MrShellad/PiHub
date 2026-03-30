use crate::domain::models::{AnswerCode, InviteCode};
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

pub struct WebRtcEngine {
    /// 内部状态：维持当前房间的 P2P 连接实例
    current_pc: Mutex<Option<Arc<RTCPeerConnection>>>,
    /// 注入的通信端口：用于底层异步事件的向上抛出
    ipc_emitter: Arc<dyn IpcEmitterPort>,
}

impl WebRtcEngine {
    pub fn new(ipc_emitter: Arc<dyn IpcEmitterPort>) -> Self {
        Self {
            current_pc: Mutex::new(None),
            ipc_emitter,
        }
    }

    /// 内部方法：构建基础的 PeerConnection 并挂载事件监听
    async fn build_pc(&self) -> Result<Arc<RTCPeerConnection>> {
        let api = APIBuilder::new().build();
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_string()], // TODO: 替换国内 STUN
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);
        let emitter_clone = Arc::clone(&self.ipc_emitter);

        // 监听连接状态
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            let state_str = s.to_string();
            let emitter = Arc::clone(&emitter_clone);
            Box::pin(async move {
                emitter.send_log("INFO", &format!("📶 P2P 状态变更: {}", state_str));
                if s == RTCPeerConnectionState::Connected {
                    emitter.send_event("TUNNEL_READY", serde_json::json!({"status": "connected"}));
                }
            })
        }));

        Ok(pc)
    }
}

// 真正实现 P2pPort 契约
impl P2pPort for WebRtcEngine {
    async fn create_offer(&self) -> Result<InviteCode> {
        let pc = self.build_pc().await?;
        
        // 房主必须主动创建 DataChannel 才能触发底层的网络通路
        let _dc = pc.create_data_channel("mc_tcp_tunnel", Some(RTCDataChannelInit {
            ordered: Some(true),
            ..Default::default()
        })).await?;

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;

        // 阻塞等待本地所有候选网络路线收集完毕
        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let local_desc = pc.local_description().await.context("获取 SDP 失败")?;
        let json_sdp = serde_json::to_string(&local_desc)?;
        let invite_b64 = general_purpose::STANDARD.encode(json_sdp);

        // 保存状态
        *self.current_pc.lock().await = Some(Arc::clone(&pc));

        Ok(InviteCode(invite_b64))
    }

    async fn generate_answer(&self, offer: &InviteCode) -> Result<AnswerCode> {
        let pc = self.build_pc().await?;
        let emitter_clone = Arc::clone(&self.ipc_emitter);

        // 客户端监听数据通道开启
        pc.on_data_channel(Box::new(move |d| {
            let label = d.label().to_owned();
            let emitter = Arc::clone(&emitter_clone);
            Box::pin(async move {
                emitter.send_log("INFO", &format!("📦 数据通道开启: {}", label));
                // TODO: 对接 TCP 本地代理端口流量
            })
        }));

        // 解析邀请码并设置远程描述
        let offer_json = String::from_utf8(general_purpose::STANDARD.decode(&offer.0)?)?;
        let offer_sdp: RTCSessionDescription = serde_json::from_str(&offer_json)?;
        pc.set_remote_description(offer_sdp).await?;

        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;

        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let local_desc = pc.local_description().await.context("获取 SDP 失败")?;
        let json_sdp = serde_json::to_string(&local_desc)?;
        let answer_b64 = general_purpose::STANDARD.encode(json_sdp);

        *self.current_pc.lock().await = Some(Arc::clone(&pc));

        Ok(AnswerCode(answer_b64))
    }

    async fn accept_answer(&self, answer: &AnswerCode) -> Result<()> {
        let pc_guard = self.current_pc.lock().await;
        let pc = pc_guard.as_ref().context("没有找到初始化好的 P2P 连接")?;

        let answer_json = String::from_utf8(general_purpose::STANDARD.decode(&answer.0)?)?;
        let answer_sdp: RTCSessionDescription = serde_json::from_str(&answer_json)?;
        
        pc.set_remote_description(answer_sdp).await?;
        Ok(())
    }
}