use serde::{Deserialize, Serialize};
use lazy_static::lazy_static;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SignalingEnvConfig {
    pub version: String,
    pub updated_at: u64,
    pub ttl: u32,
    pub servers: Vec<SignalingServer>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SignalingServer {
    pub id: String,
    pub url: String,
    pub region: String,
    pub provider: String,
    pub priority: i32,
    pub weight: i32,
    pub secure: bool,
    pub features: Features,
    pub limits: Limits,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Features {
    pub p2p: bool,
    pub relay: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Limits {
    pub max_connections: u32,
}

#[derive(Clone)]
struct ServerCache {
    config: Option<SignalingEnvConfig>,
    last_fetch_at: u64,
}

lazy_static! {
    static ref CACHE: Arc<RwLock<ServerCache>> = Arc::new(RwLock::new(ServerCache {
        config: None,
        last_fetch_at: 0,
    }));
}

impl SignalingEnvConfig {
    /// 默认托底配置（硬编码 fallback）
    pub fn default_config() -> Self {
        let default_json = r#"{
  "version": "1.0",
  "updated_at": 1712345678,
  "ttl": 300,
  "servers": [
    {
      "id": "cn-1",
      "url": "wss://cn1.example.com",
      "region": "CN",
      "provider": "official",
      "priority": 100,
      "weight": 1,
      "secure": true,
      "features": {
        "p2p": true,
        "relay": false
      },
      "limits": {
        "max_connections": 1000
      }
    }
  ]
}"#;
        serde_json::from_str(default_json).unwrap()
    }

    /// 获取目前的信令服务器配置（自带 TTL 缓存机制，API崩溃自动托底）
    pub async fn get_config() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        let (needs_fetch, _ttl) = {
            let cache = CACHE.read().await;
            let current_ttl = cache.config.as_ref().map(|c| c.ttl as u64).unwrap_or(300);
            let time_elapsed = now.saturating_sub(cache.last_fetch_at);
            (time_elapsed >= current_ttl, current_ttl)
        };

        if needs_fetch {
            // 尝试从远程 API 读取
            let api_url = std::env::var("FLOWCORE_API_URL")
                .unwrap_or_else(|_| "https://flowcore.app/api/signaling-servers".to_string());
            let api_key = std::env::var("FLOWCORE_API_KEY").unwrap_or_default();

            let client = reqwest::Client::new();
            let mut req = client.get(&api_url);
            if !api_key.is_empty() {
                req = req.header("X-API-Key", api_key);
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(mut new_config) = resp.json::<SignalingEnvConfig>().await {
                        // 强制覆盖拉取时间作为 ttl 刻度点
                        new_config.updated_at = now; 
                        let mut cache = CACHE.write().await;
                        cache.config = Some(new_config.clone());
                        cache.last_fetch_at = now;
                        return new_config;
                    }
                }
                _ => {
                    // 请求失败或者解析失败，进入托底 (如果已有缓存，继续使用缓存；否则用出厂配置)
                    let cache = CACHE.read().await;
                    if let Some(ref c) = cache.config {
                        return c.clone();
                    }
                }
            }
        }

        // 直接返回内存中的缓存，或者兜底代码
        let cache = CACHE.read().await;
        if let Some(ref c) = cache.config {
            c.clone()
        } else {
            Self::default_config()
        }
    }
}
