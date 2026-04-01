use crate::application::use_cases::SessionManager;
use crate::domain::ports::{IpcEmitterPort, P2pPort};
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, BufReader};

#[derive(Deserialize, Debug)]
struct RpcRequest {
    req_id: String,
    action: String,
    payload: serde_json::Value,
}

pub async fn start_event_loop<P: P2pPort + 'static, I: IpcEmitterPort + 'static>(
    app_service: Arc<SessionManager<P, I>>,
) {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(req) = serde_json::from_str::<RpcRequest>(&line) {
            let app_clone = Arc::clone(&app_service);

            tokio::spawn(async move {
                match req.action.as_str() {
                    "CREATE_ROOM" => {
                        let target_mc_port = req
                            .payload
                            .get("target_mc_port")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u16)
                            .unwrap_or(25565);
                        let signaling_server = req
                            .payload
                            .get("signaling_server")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned);

                        app_clone
                            .handle_create_room(req.req_id, target_mc_port, signaling_server)
                            .await;
                    }
                    "JOIN_ROOM" => {
                        let local_proxy_port = req
                            .payload
                            .get("local_proxy_port")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u16)
                            .unwrap_or(50001);
                        let signaling_server = req
                            .payload
                            .get("signaling_server")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned);

                        if let Some(invite_code) =
                            req.payload.get("invite_code").and_then(|v| v.as_str())
                        {
                            app_clone
                                .handle_join_room(
                                    req.req_id,
                                    invite_code.to_owned(),
                                    local_proxy_port,
                                    signaling_server,
                                )
                                .await;
                        }
                    }
                    "HOST_ACCEPT_ANSWER" => {
                        if let Some(answer_code) =
                            req.payload.get("answer_code").and_then(|v| v.as_str())
                        {
                            app_clone
                                .handle_accept_answer(req.req_id, answer_code.to_owned())
                                .await;
                        }
                    }
                    "GET_SIGNALING_SERVERS" => {
                        app_clone.handle_get_signaling_servers(req.req_id).await;
                    }
                    _ => {}
                }
            });
        }
    }
}
