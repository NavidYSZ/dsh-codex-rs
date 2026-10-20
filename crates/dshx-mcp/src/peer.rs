//! JSON-RPC 2.0 client side of one MCP connection: request/response
//! matching, timeouts and cancellation (`notifications/cancelled`), answers
//! to server requests, and the notifications the supervisor cares about.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::McpError;

/// What a transport delivers to the peer.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// One JSON-RPC message (or batch array).
    Message(Value),
    /// The connection ended (process exit, stream end, expired session).
    Closed(String),
}

/// One live connection to a server.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    /// Sends one message; for HTTP this also delivers the reply to the inbox.
    async fn send(&self, message: Value) -> Result<(), McpError>;
    /// Called once the initialize handshake finished.
    fn initialized(&self, _protocol_version: &str) {}
    /// Ends the connection. `true` when the shutdown is confirmed (the
    /// process tree is gone or the session was released).
    async fn close(&self) -> bool;
}

/// Connection-level events for the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PeerEvent {
    ToolsChanged,
    Closed(String),
}

type Pending = HashMap<i64, oneshot::Sender<Result<Value, McpError>>>;

pub(crate) struct Peer {
    transport: Arc<dyn Transport>,
    pending: Mutex<Pending>,
    next_id: AtomicI64,
    closed: AtomicBool,
    label: String,
}

impl Peer {
    /// Starts the reader over `inbox`; events go to the returned receiver.
    pub fn start(
        transport: Arc<dyn Transport>,
        mut inbox: mpsc::UnboundedReceiver<Incoming>,
        label: String,
    ) -> (Arc<Peer>, mpsc::UnboundedReceiver<PeerEvent>) {
        let peer = Arc::new(Peer {
            transport,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(0),
            closed: AtomicBool::new(false),
            label,
        });
        let (events, rx) = mpsc::unbounded_channel();
        let weak = Arc::downgrade(&peer);
        tokio::spawn(async move {
            while let Some(incoming) = inbox.recv().await {
                let Some(peer) = weak.upgrade() else { return };
                match incoming {
                    Incoming::Message(Value::Array(batch)) => {
                        for message in batch {
                            peer.dispatch(message, &events);
                        }
                    }
                    Incoming::Message(message) => peer.dispatch(message, &events),
                    Incoming::Closed(reason) => {
                        peer.fail_all();
                        let _ = events.send(PeerEvent::Closed(reason));
                        return;
                    }
                }
            }
            if let Some(peer) = weak.upgrade() {
                peer.fail_all();
            }
            let _ = events.send(PeerEvent::Closed("connection ended".into()));
        });
        (peer, rx)
    }

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn fail_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(McpError::Closed));
        }
    }

    fn dispatch(self: &Arc<Self>, message: Value, events: &mpsc::UnboundedSender<PeerEvent>) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { return };
                let Some(tx) = self.pending.lock().unwrap().remove(&id) else {
                    return;
                };
                let result = match message.get("error") {
                    Some(error) => Err(McpError::from_rpc(error)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
            (Some(method), Some(id)) => {
                // Server request: answer ping; this client offers no capabilities.
                let reply = if method == "ping" {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "error": {
                        "code": -32601, "message": format!("Method not found: {method}")}})
                };
                let transport = self.transport.clone();
                tokio::spawn(async move {
                    let _ = transport.send(reply).await;
                });
            }
            (Some(method), None) => match method {
                "notifications/tools/list_changed" => {
                    let _ = events.send(PeerEvent::ToolsChanged);
                }
                "notifications/message" => {
                    tracing::debug!("{}: server log: {}", self.label, message["params"]);
                }
                _ => {}
            },
            (None, None) => tracing::debug!("{}: ignoring malformed message", self.label),
        }
    }

    /// Sends a notification; failures are logged only.
    pub async fn notify(&self, method: &str, params: Option<Value>) {
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        if let Err(e) = self.transport.send(message).await {
            tracing::debug!("{}: notification {method} failed: {e}", self.label);
        }
    }

    /// Sends a request and waits for its result, at most `timeout`; on
    /// timeout or `cancel` the server receives `notifications/cancelled`.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, McpError> {
        if self.is_closed() {
            return Err(McpError::NotConnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        let exchange = async {
            self.transport.send(message).await?;
            rx.await.unwrap_or(Err(McpError::Closed))
        };
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        let outcome = tokio::select! {
            r = exchange => r,
            _ = tokio::time::sleep(timeout) => Err(McpError::Timeout),
            _ = cancel.cancelled() => Err(McpError::Cancelled),
        };
        if outcome.is_err() {
            let was_pending = self.pending.lock().unwrap().remove(&id).is_some();
            let reason = match &outcome {
                Err(McpError::Timeout) => Some("Request timed out"),
                Err(McpError::Cancelled) => Some("The request was cancelled"),
                _ => None,
            };
            if let (true, Some(reason)) = (was_pending, reason)
                && !self.is_closed()
            {
                self.notify(
                    "notifications/cancelled",
                    Some(json!({"requestId": id, "reason": reason})),
                )
                .await;
            }
        }
        outcome
    }
}
