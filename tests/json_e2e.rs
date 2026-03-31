use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    seen: Vec<String>,
}

impl Session {
    fn start() -> Self {
        let exe = std::env::var("CARGO_BIN_EXE_PiHub")
            .expect("CARGO_BIN_EXE_PiHub is not available for integration tests");

        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn PiHub");

        let stdin = child.stdin.take().expect("missing child stdin");
        let stdout = child.stdout.take().expect("missing child stdout");
        let stderr = child.stderr.take().expect("missing child stderr");

        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, tx.clone(), "");
        spawn_reader(stderr, tx, "[stderr] ");

        Self {
            child,
            stdin,
            rx,
            seen: Vec::new(),
        }
    }

    fn send_json(&mut self, value: Value) {
        let line = serde_json::to_string(&value).expect("failed to encode JSON");
        writeln!(self.stdin, "{line}").expect("failed to write child stdin");
        self.stdin.flush().expect("failed to flush child stdin");
    }

    fn wait_for_json<F>(&mut self, timeout: Duration, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let remaining = timeout
                .checked_sub(start.elapsed())
                .unwrap_or_else(|| Duration::from_millis(1));

            match self
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        if predicate(&value) {
                            return value;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        panic!(
            "timed out waiting for JSON message. Seen lines:\n{}",
            self.seen.join("\n")
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_reader<R>(reader: R, tx: mpsc::Sender<String>, prefix: &'static str)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let formatted = if prefix.is_empty() {
                        line
                    } else {
                        format!("{prefix}{line}")
                    };
                    let _ = tx.send(formatted);
                }
                Err(_) => break,
            }
        }
    });
}

fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("failed to allocate free port");
    let port = listener.local_addr().expect("missing local addr").port();
    drop(listener);
    port
}

#[test]
fn json_offer_answer_tunnel_connects() {
    let host_port = free_port();
    let proxy_port = free_port();
    let payload = b"pihub-e2e-test";

    let echo_listener =
        TcpListener::bind(("127.0.0.1", host_port)).expect("failed to bind host echo server");
    let echo_handle = thread::spawn(move || -> Vec<u8> {
        let (mut socket, _) = echo_listener.accept().expect("echo server accept failed");
        let mut buf = [0_u8; 1024];
        let read = socket.read(&mut buf).expect("echo server read failed");
        socket
            .write_all(&buf[..read])
            .expect("echo server write failed");
        buf[..read].to_vec()
    });

    let mut host = Session::start();
    let mut client = Session::start();

    host.send_json(json!({
        "req_id": "req-create",
        "action": "CREATE_ROOM",
        "payload": {
            "target_mc_port": host_port
        }
    }));
    let create_response = host.wait_for_json(Duration::from_secs(30), |value| {
        value["type"] == "response" && value["req_id"] == "req-create"
    });
    let invite_code = create_response["data"]["invite_code"]
        .as_str()
        .expect("missing invite_code")
        .to_owned();

    client.send_json(json!({
        "req_id": "req-join",
        "action": "JOIN_ROOM",
        "payload": {
            "invite_code": invite_code,
            "local_proxy_port": proxy_port
        }
    }));
    let join_response = client.wait_for_json(Duration::from_secs(30), |value| {
        value["type"] == "response" && value["req_id"] == "req-join"
    });
    let answer_code = join_response["data"]["answer_code"]
        .as_str()
        .expect("missing answer_code")
        .to_owned();

    host.send_json(json!({
        "req_id": "req-accept",
        "action": "HOST_ACCEPT_ANSWER",
        "payload": {
            "answer_code": answer_code
        }
    }));
    let accept_response = host.wait_for_json(Duration::from_secs(30), |value| {
        value["type"] == "response" && value["req_id"] == "req-accept"
    });
    assert_eq!(accept_response["status"], "success");

    let tunnel_ready = client.wait_for_json(Duration::from_secs(30), |value| {
        value["type"] == "event" && value["event_name"] == "TUNNEL_READY"
    });
    assert_eq!(tunnel_ready["data"]["proxy_port"], proxy_port);

    thread::sleep(Duration::from_millis(500));

    let mut proxy = TcpStream::connect(("127.0.0.1", proxy_port))
        .expect("failed to connect to local proxy port");
    proxy
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("failed to set read timeout");
    proxy
        .write_all(payload)
        .expect("failed to write to proxy socket");

    let mut echoed = vec![0_u8; payload.len()];
    proxy
        .read_exact(&mut echoed)
        .expect("failed to read echoed payload");

    let server_received = echo_handle.join().expect("echo thread join failed");

    assert_eq!(echoed, payload);
    assert_eq!(server_received, payload);
}
