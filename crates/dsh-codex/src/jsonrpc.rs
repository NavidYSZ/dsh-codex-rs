//! Newline-delimited JSON-RPC 2.0 over stdio, as spoken by the dsh SDK
//! (`packages/sdk/protocol/src/transport.ts`): one JSON object per line,
//! handler failures become `-32603` errors, malformed lines are ignored.
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INTERNAL_ERROR: i64 = -32603;

/// A handler error with an explicit JSON-RPC code.
#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        RpcError {
            code: INTERNAL_ERROR,
            message: e.to_string(),
        }
    }
}

#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

/// Sends notifications to the client; cheap to clone.
#[derive(Clone)]
pub struct Notifier {
    tx: mpsc::UnboundedSender<Value>,
}

impl Notifier {
    pub fn notify(&self, method: &str, params: Value) {
        let _ = self
            .tx
            .send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }
}

/// Creates the outgoing channel. Notifications and responses share one ordered queue.
pub fn channel() -> (Notifier, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Notifier { tx }, rx)
}

/// Serves requests from `input` until EOF or until the handler returns from a
/// `shutdown` request; all output (responses + notifications) goes to `output`.
pub async fn serve<R, W>(
    input: R,
    output: W,
    handler: Arc<dyn Handler>,
    notifier: Notifier,
    mut outgoing: mpsc::UnboundedReceiver<Value>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let output = Arc::new(Mutex::new(output));
    let writer = {
        let output = output.clone();
        tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                // `Null` is our end marker: everything queued before it is written.
                if frame.is_null() {
                    break;
                }
                let mut line = serde_json::to_string(&frame).unwrap_or_default();
                line.push('\n');
                let mut out = output.lock().await;
                if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
                    break;
                }
            }
        })
    };

    let mut lines = BufReader::new(input).lines();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let id = frame.get("id").cloned();
        let handler = handler.clone();
        let tx = notifier.tx.clone();
        let is_shutdown = method == "shutdown";
        let task = async move {
            let result = handler.handle(&method, params).await;
            if let Some(id) = id {
                let frame = match result {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err(e) => {
                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
                    }
                };
                let _ = tx.send(frame);
            }
        };
        if is_shutdown {
            // Shutdown runs to completion before we stop reading.
            task.await;
            break;
        }
        tasks.spawn(task);
    }
    while tasks.join_next().await.is_some() {}
    // Long-lived notifier clones (agents, subagents) must not keep the writer
    // alive: queue the end marker and let it drain what came before.
    let _ = notifier.tx.send(Value::Null);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl Handler for Echo {
        async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
            match method {
                "echo" => Ok(params),
                "shutdown" => Ok(json!({})),
                "fail" => Err(RpcError::internal("boom")),
                _ => Err(RpcError {
                    code: METHOD_NOT_FOUND,
                    message: format!("unknown method {method}"),
                }),
            }
        }
    }

    #[tokio::test]
    async fn answers_requests_and_stops_on_shutdown() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\",\"params\":{\"a\":1}}\n",
            "not json\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"fail\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"nope\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"shutdown\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"echo\"}\n",
        );
        let (client, server_out) = tokio::io::duplex(1 << 16);
        let (notifier, rx) = channel();
        notifier.notify("session.status", json!({ "status": "idle" }));
        serve(input.as_bytes(), server_out, Arc::new(Echo), notifier, rx)
            .await
            .unwrap();
        let mut text = String::new();
        let mut client = client;
        tokio::io::AsyncReadExt::read_to_string(&mut client, &mut text)
            .await
            .unwrap();
        let frames: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(frames[0]["method"], "session.status");
        let by_id = |id: i64| frames.iter().find(|f| f["id"] == id).cloned();
        assert_eq!(by_id(1).unwrap()["result"], json!({ "a": 1 }));
        assert_eq!(by_id(2).unwrap()["error"]["code"], INTERNAL_ERROR);
        assert_eq!(by_id(3).unwrap()["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(by_id(4).unwrap()["result"], json!({}));
        assert!(by_id(5).is_none(), "nothing is served after shutdown");
    }
}
