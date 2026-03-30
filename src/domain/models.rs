use serde::{Deserialize, Serialize};

/// 值对象：邀请码 (封装底层复杂的 Base64 SDP 字符串)
/// 使用元组结构体，在类型层面上防止与普通 String 混淆
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteCode(pub String);

/// 值对象：回应码 (客户端生成的 Answer)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerCode(pub String);

/// 领域枚举：当前 P2P 隧道使用的路由方式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RouteType {
    IPv6Direct,
    LocalNetwork,
    UdpP2p,
    TcpP2p,
    TurnRelay,
}

/// 领域事件：引擎内部发生的状态变更，准备向外层(如启动器)推送
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 隧道建立成功，TCP 本地代理已就绪
    TunnelReady {
        local_proxy_port: u16,
        route_method: RouteType,
        latency_ms: u32,
    },
    /// 有新玩家加入 (适用于房主端)
    PlayerJoined {
        peer_id: String,
        route_method: RouteType,
    },
    /// 连接异常断开
    Disconnected(String),
}