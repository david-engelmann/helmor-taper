//! Async WebSocket client for the MCP bridge.
//!
//! - Port-scan connect: tries `base_port..base_port+port_scan` until
//!   the bridge accepts a connection, with a deadline.
//! - Request/response correlation: each outbound [`BridgeRequest`]
//!   gets a UUID; responses are routed back to their waiter via a
//!   `HashMap<id, oneshot::Sender>`.
//! - Background reader task: a dedicated tokio task owns the
//!   `WebSocketStream` reader half + the pending map; the main `Bridge`
//!   handle owns the writer half. On reader exit (peer close, error)
//!   it drops all pending senders, which makes any waiter's `.recv()`
//!   error cleanly instead of hanging.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::protocol::{BridgeError, BridgeRequest, BridgeResponse};
use super::{DEFAULT_BASE_PORT, DEFAULT_HOST, DEFAULT_PORT_SCAN};

/// How the bridge reaches Helmor.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub host: String,
    pub base_port: u16,
    pub port_scan: u16,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            base_port: DEFAULT_BASE_PORT,
            port_scan: DEFAULT_PORT_SCAN,
            connect_timeout: Duration::from_secs(8),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Connection-time errors. Separated from runtime [`BridgeError`] so
/// callers can distinguish "couldn't reach Helmor" from "Helmor replied
/// with an error."
#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("no MCP bridge on {host}:{base_port}..+{port_scan} within {timeout_ms}ms")]
    NoBridge {
        host: String,
        base_port: u16,
        port_scan: u16,
        timeout_ms: u128,
    },
    #[error("websocket handshake failed against {url}: {source}")]
    Handshake {
        url: String,
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<BridgeResponse>>>>;
type WsWriter = Arc<
    Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>,
>;

/// Public handle. Cloneable — both clones share the same connection
/// and pending map under `Arc<Mutex<_>>`.
#[derive(Clone)]
pub struct Bridge {
    writer: WsWriter,
    pending: PendingMap,
    config: BridgeConfig,
    port: u16,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't try to print the WebSocket writer / pending map — they
        // hold async-locked state. Just identify the connection.
        f.debug_struct("Bridge")
            .field("host", &self.config.host)
            .field("port", &self.port)
            .finish()
    }
}

impl Bridge {
    /// Port-scan `config.host:base_port..+port_scan` until a WebSocket
    /// handshake succeeds, then spawn the reader task and return a
    /// ready-to-use handle.
    pub async fn connect(config: BridgeConfig) -> Result<Self, ConnectError> {
        let start = Instant::now();
        let deadline = start + config.connect_timeout;

        loop {
            for p in config.base_port..config.base_port + config.port_scan {
                let url = format!("ws://{}:{}", config.host, p);
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((stream, _resp)) => {
                        return Ok(Self::spawn_reader(stream, config, p));
                    }
                    Err(_) => continue,
                }
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::NoBridge {
                    host: config.host,
                    base_port: config.base_port,
                    port_scan: config.port_scan,
                    timeout_ms: config.connect_timeout.as_millis(),
                });
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Skip the port scan and connect to a single known URL.
    /// Used in tests against a mock server bound to a known port.
    pub async fn connect_direct(url: &str, config: BridgeConfig) -> Result<Self, ConnectError> {
        let (stream, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|source| ConnectError::Handshake {
                url: url.to_string(),
                source,
            })?;
        // Port doesn't really matter when we're given an explicit URL —
        // record 0 as the "n/a" sentinel.
        Ok(Self::spawn_reader(stream, config, 0))
    }

    fn spawn_reader(
        stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        config: BridgeConfig,
        port: u16,
    ) -> Self {
        let (write, mut read) = stream.split();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let writer = Arc::new(Mutex::new(write));

        let pending_for_reader = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                let Ok(msg) = msg else { break };
                let Message::Text(text) = msg else { continue };
                let parsed: Result<BridgeResponse, _> = serde_json::from_str(&text);
                let response = match parsed {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::warn!(?err, raw = %text, "bridge: malformed response frame");
                        continue;
                    }
                };
                let id = response.id.clone();
                let waiter = pending_for_reader.lock().await.remove(&id);
                if let Some(tx) = waiter {
                    let _ = tx.send(response);
                } else {
                    tracing::warn!(id = %id, "bridge: response with no matching pending request");
                }
            }
            // Reader exited — drop all pending senders so waiters error
            // out instead of hanging.
            let mut map = pending_for_reader.lock().await;
            let pending_count = map.len();
            map.clear();
            if pending_count > 0 {
                tracing::warn!(
                    pending = pending_count,
                    "bridge: reader exited with pending requests; cleared map"
                );
            }
        });

        Self {
            writer,
            pending,
            config,
            port,
        }
    }

    /// Port the connection landed on. Useful for tests + diagnostic
    /// messages ("connected to bridge on port 9224").
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Send a request and wait for the matching response. Times out at
    /// `config.request_timeout`.
    pub async fn request(
        &self,
        command: impl Into<String>,
        args: Value,
    ) -> Result<Value, BridgeError> {
        let req = BridgeRequest::new(command, args);
        let id = req.id.clone();
        let frame = serde_json::to_string(&req).map_err(BridgeError::from)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        // Write under lock. If the write fails we MUST remove the
        // pending entry so it doesn't leak — the reader won't see a
        // matching response.
        {
            let mut writer = self.writer.lock().await;
            if let Err(err) = writer.send(Message::Text(frame)).await {
                self.pending.lock().await.remove(&id);
                return Err(BridgeError::Io(format!("write failed: {err}")));
            }
        }

        match timeout(self.config.request_timeout, rx).await {
            Ok(Ok(response)) => response.into_result(),
            Ok(Err(_)) => {
                // Sender dropped — reader task exited before replying.
                let pending = self.pending.lock().await.len();
                Err(BridgeError::ConnectionClosed { pending })
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(BridgeError::Timeout {
                    id,
                    timeout_ms: self.config.request_timeout.as_millis() as u64,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bridge_config_default_matches_protocol_constants() {
        let c = BridgeConfig::default();
        assert_eq!(c.host, DEFAULT_HOST);
        assert_eq!(c.base_port, DEFAULT_BASE_PORT);
        assert_eq!(c.port_scan, DEFAULT_PORT_SCAN);
    }

    #[tokio::test]
    async fn connect_returns_no_bridge_error_when_nothing_listening() {
        // Use a tiny timeout + a port range we know isn't bound.
        let config = BridgeConfig {
            host: "127.0.0.1".into(),
            base_port: 60_000, // unlikely to be bound on the dev machine
            port_scan: 5,
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_secs(1),
        };
        let err = Bridge::connect(config)
            .await
            .expect_err("nothing listening — connect must fail");
        match err {
            ConnectError::NoBridge {
                base_port,
                port_scan,
                ..
            } => {
                assert_eq!(base_port, 60_000);
                assert_eq!(port_scan, 5);
            }
            other => panic!("expected NoBridge, got {other:?}"),
        }
    }

    /// Tiny in-process WebSocket server that echoes requests back as
    /// responses. Used to exercise the round-trip without hitting a
    /// real Helmor instance.
    async fn spawn_echo_server() -> u16 {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let (mut write, mut read) = ws.split();
                    while let Some(Ok(msg)) = read.next().await {
                        if let Message::Text(text) = msg {
                            let req: BridgeRequest = match serde_json::from_str(&text) {
                                Ok(r) => r,
                                Err(_) => continue,
                            };
                            // Echo command + args back as the data payload.
                            let resp = BridgeResponse::ok(
                                &req.id,
                                json!({"echoed_command": req.command, "echoed_args": req.args}),
                            );
                            let frame = serde_json::to_string(&resp).unwrap();
                            let _ = write.send(Message::Text(frame)).await;
                        }
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn request_round_trips_against_echo_server() {
        let port = spawn_echo_server().await;
        let url = format!("ws://127.0.0.1:{port}");
        let bridge = Bridge::connect_direct(&url, BridgeConfig::default())
            .await
            .expect("echo server should accept handshake");

        let res = bridge
            .request("list_windows", json!({}))
            .await
            .expect("echo server should respond");
        assert_eq!(res["echoed_command"], "list_windows");
    }

    #[tokio::test]
    async fn concurrent_requests_route_responses_by_id() {
        // 10 parallel requests must each get their own response —
        // proves the pending-map correlation works.
        let port = spawn_echo_server().await;
        let url = format!("ws://127.0.0.1:{port}");
        let bridge = Bridge::connect_direct(&url, BridgeConfig::default())
            .await
            .unwrap();

        let mut handles = vec![];
        for i in 0..10 {
            let b = bridge.clone();
            handles.push(tokio::spawn(async move {
                let res = b
                    .request("execute_js", json!({"script": format!("test_{i}")}))
                    .await
                    .expect("each parallel request should resolve");
                let echoed = res["echoed_args"]["script"].as_str().unwrap().to_string();
                (i, echoed)
            }));
        }
        for h in handles {
            let (i, echoed) = h.await.unwrap();
            assert_eq!(echoed, format!("test_{i}"));
        }
    }

    /// Server that accepts the request but never replies — verifies the
    /// request timeout fires cleanly.
    async fn spawn_silent_server() -> u16 {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let (_w, mut read) = ws.split();
                    // Drain reads forever, never write.
                    while let Some(Ok(_)) = read.next().await {}
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn request_times_out_when_server_never_replies() {
        let port = spawn_silent_server().await;
        let url = format!("ws://127.0.0.1:{port}");
        let cfg = BridgeConfig {
            request_timeout: Duration::from_millis(200),
            ..Default::default()
        };
        let bridge = Bridge::connect_direct(&url, cfg).await.unwrap();

        let err = bridge
            .request("list_windows", json!({}))
            .await
            .expect_err("must time out");
        match err {
            BridgeError::Timeout { timeout_ms, .. } => {
                assert_eq!(timeout_ms, 200);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_returns_command_error_when_server_replies_error() {
        // Server that always replies with an error frame.
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let (mut write, mut read) = ws.split();
                    while let Some(Ok(msg)) = read.next().await {
                        if let Message::Text(text) = msg {
                            let req: BridgeRequest = serde_json::from_str(&text).unwrap();
                            let resp = BridgeResponse::err(&req.id, "no such command");
                            let _ = write
                                .send(Message::Text(serde_json::to_string(&resp).unwrap()))
                                .await;
                        }
                    }
                });
            }
        });

        let url = format!("ws://127.0.0.1:{port}");
        let bridge = Bridge::connect_direct(&url, BridgeConfig::default())
            .await
            .unwrap();

        let err = bridge
            .request("nope", json!({}))
            .await
            .expect_err("server replied error");
        match err {
            BridgeError::Command { message } => assert_eq!(message, "no such command"),
            other => panic!("expected Command, got {other:?}"),
        }
    }
}
