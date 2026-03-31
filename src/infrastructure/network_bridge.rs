use crate::domain::ports::IpcEmitterPort;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use webrtc::data::data_channel::{DataChannel as DetachedDataChannel, PollDataChannel};
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::peer_connection::RTCPeerConnection;

pub struct NetworkBridge;

impl NetworkBridge {
    pub async fn start_host_bridge(
        pc: Arc<RTCPeerConnection>,
        ipc_emitter: Arc<dyn IpcEmitterPort>,
        target_mc_port: u16,
    ) -> Result<()> {
        let dc = pc
            .create_data_channel(
                "mc_p2p_tunnel",
                Some(RTCDataChannelInit {
                    ordered: Some(true),
                    ..Default::default()
                }),
            )
            .await
            .context("[Bridge] Failed to create host data channel")?;

        let emitter = Arc::clone(&ipc_emitter);
        let target_addr = format!("127.0.0.1:{target_mc_port}");
        emitter.send_log(
            "INFO",
            &format!("[Bridge] Host created data channel [{}]", dc.label()),
        );

        let emitter_for_open = Arc::clone(&emitter);
        let dc_for_open = Arc::clone(&dc);
        dc.on_open(Box::new(move || {
            let emitter = Arc::clone(&emitter_for_open);
            let dc = Arc::clone(&dc_for_open);
            let target_addr = target_addr.clone();

            Box::pin(async move {
                emitter.send_log("INFO", "[Bridge] Host data channel is open");

                let detached = match dc.detach().await {
                    Ok(detached) => detached,
                    Err(err) => {
                        emitter.send_log(
                            "ERROR",
                            &format!("[Bridge] Failed to detach host data channel: {err}"),
                        );
                        return;
                    }
                };
                let mut dc_stream = PollDataChannel::new(detached);

                match TcpStream::connect(&target_addr).await {
                    Ok(mut tcp_stream) => {
                        emitter.send_log(
                            "INFO",
                            &format!(
                                "[Bridge] Host connected tunnel to local port {target_mc_port}"
                            ),
                        );

                        if let Err(err) =
                            copy_bidirectional(&mut dc_stream, &mut tcp_stream).await
                        {
                            emitter.send_log(
                                "WARN",
                                &format!("[Bridge] Host tunnel interrupted: {err}"),
                            );
                        }

                        emitter.send_log("INFO", "[Bridge] Host tunnel closed");
                    }
                    Err(err) => {
                        emitter.send_log(
                            "ERROR",
                            &format!(
                                "[Bridge] Failed to connect to host target port {target_mc_port}: {err}"
                            ),
                        );
                    }
                }
            })
        }));

        Ok(())
    }

    pub async fn start_client_bridge(
        pc: Arc<RTCPeerConnection>,
        ipc_emitter: Arc<dyn IpcEmitterPort>,
        local_proxy_port: u16,
    ) -> Result<()> {
        let addr = format!("127.0.0.1:{local_proxy_port}");
        let listener = TcpListener::bind(&addr).await.with_context(|| {
            format!("[Bridge] Failed to bind local proxy port {local_proxy_port}")
        })?;

        let emitter = Arc::clone(&ipc_emitter);
        let detached_dc = Arc::new(Mutex::new(None::<Result<Arc<DetachedDataChannel>, String>>));
        let ready = Arc::new(Notify::new());

        let emitter_for_channel = Arc::clone(&emitter);
        let detached_for_channel = Arc::clone(&detached_dc);
        let ready_for_channel = Arc::clone(&ready);
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let emitter = Arc::clone(&emitter_for_channel);
            let detached_dc = Arc::clone(&detached_for_channel);
            let ready = Arc::clone(&ready_for_channel);

            Box::pin(async move {
                emitter.send_log(
                    "INFO",
                    &format!("[Bridge] Client received data channel [{}]", dc.label()),
                );

                let emitter_for_open = Arc::clone(&emitter);
                let dc_for_open = Arc::clone(&dc);
                let detached_for_open = Arc::clone(&detached_dc);
                let ready_for_open = Arc::clone(&ready);
                dc.on_open(Box::new(move || {
                    let emitter = Arc::clone(&emitter_for_open);
                    let dc = Arc::clone(&dc_for_open);
                    let detached_dc = Arc::clone(&detached_for_open);
                    let ready = Arc::clone(&ready_for_open);

                    Box::pin(async move {
                        let state = match dc.detach().await {
                            Ok(detached) => {
                                emitter.send_log("INFO", "[Bridge] Client data channel is open");
                                emitter.send_event(
                                    "TUNNEL_READY",
                                    serde_json::json!({ "proxy_port": local_proxy_port }),
                                );
                                Ok(detached)
                            }
                            Err(err) => {
                                let err = err.to_string();
                                emitter.send_log(
                                    "ERROR",
                                    &format!(
                                        "[Bridge] Failed to detach client data channel: {err}"
                                    ),
                                );
                                Err(err)
                            }
                        };

                        *detached_dc.lock().await = Some(state);
                        ready.notify_waiters();
                    })
                }));
            })
        }));

        tokio::spawn(async move {
            emitter.send_log(
                "INFO",
                &format!("[Bridge] Client proxy listening on {addr}"),
            );

            let (mut tcp_stream, _) = match listener.accept().await {
                Ok(incoming) => incoming,
                Err(err) => {
                    emitter.send_log(
                        "ERROR",
                        &format!("[Bridge] Failed to accept local proxy connection: {err}"),
                    );
                    return;
                }
            };

            emitter.send_log(
                "INFO",
                "[Bridge] Local TCP client connected, waiting for host data channel",
            );

            let detached = loop {
                let state = { detached_dc.lock().await.clone() };
                match state {
                    Some(Ok(detached)) => break detached,
                    Some(Err(err)) => {
                        emitter.send_log("ERROR", &format!("[Bridge] Tunnel setup failed: {err}"));
                        return;
                    }
                    None => ready.notified().await,
                }
            };

            let mut dc_stream = PollDataChannel::new(detached);
            emitter.send_log("INFO", "[Bridge] Tunnel forwarding started");

            if let Err(err) = copy_bidirectional(&mut tcp_stream, &mut dc_stream).await {
                emitter.send_log("WARN", &format!("[Bridge] Tunnel interrupted: {err}"));
            }

            emitter.send_log("INFO", "[Bridge] Tunnel forwarding finished");
        });

        Ok(())
    }
}
