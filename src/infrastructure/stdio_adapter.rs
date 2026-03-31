use crate::domain::ports::IpcEmitterPort;
use serde::Serialize;

/// Stdio 适配器：将事件和响应转化为按行分割的 JSON 写入标准输出
pub struct StdioEmitter;

impl StdioEmitter {
    pub fn new() -> Self {
        Self {}
    }

    /// 内部辅助方法：将结构体转为单行 JSON 并打印
    fn emit<T: Serialize>(&self, output: &T) {
        if let Ok(json_str) = serde_json::to_string(output) {
            // 在 Sidecar 模式中，println! 会直接将数据推入父进程的 stdout 管道
            println!("{}", json_str);
        }
    }
}

// 定义符合 IPC 规范的内部输出结构
#[derive(Serialize)]
#[serde(tag = "type")]
enum RpcOutput<'a> {
    #[serde(rename = "response")]
    Response {
        req_id: &'a str,
        status: &'a str,
        data: serde_json::Value,
    },
    #[serde(rename = "event")]
    Event {
        event_name: &'a str,
        data: serde_json::Value,
    },
    #[serde(rename = "log")]
    Log { level: &'a str, message: &'a str },
}

impl IpcEmitterPort for StdioEmitter {
    fn send_response(&self, req_id: &str, status: &str, data: serde_json::Value) {
        self.emit(&RpcOutput::Response {
            req_id,
            status,
            data,
        });
    }

    fn send_event(&self, event_name: &str, data: serde_json::Value) {
        self.emit(&RpcOutput::Event { event_name, data });
    }

    fn send_log(&self, level: &str, message: &str) {
        self.emit(&RpcOutput::Log { level, message });
    }
}
