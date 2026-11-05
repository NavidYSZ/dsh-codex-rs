//! The Aether side of `aether-bridge.ts`: a Unix socket per Pi process
//! (folder 0700, socket 0600, a 32-byte token compared in constant time)
//! speaking JSON lines.
//!
//! | Message | Direction | Content |
//! |---|---|---|
//! | `hello {token, pi}` → `config` | extension → Aether → extension | `{session, cwd, sandbox, tools, providers}` |
//! | `gate {id, callId, tool, input}` → `gate {id, allow, reason?}` | extension → Aether | Pi's `tool_call` for `write`, `edit` and bridge tools |
//! | `call {id, callId, tool, args}` → `result {id, content, isError, details}` | extension → Aether | runs an Aether tool |
//! | `cancel {id}` | extension → Aether | the tool's abort signal |
//! | `providers {providers}` | Aether → extension | keys or providers changed |
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::rpc::LineSplitter;

/// What the bridge asks of its chat.
#[async_trait]
pub trait BridgeHost: Send + Sync {
    /// The `config` answer to a valid `hello` (keys included).
    fn config(&self) -> Value;
    /// `Ok` lets the call run; `Err` blocks it with the text the model sees.
    fn gate(&self, tool: &str, input: &Value) -> Result<(), String>;
    /// Runs an Aether tool; returns `{content, isError, details}`.
    async fn call(
        &self,
        call_id: &str,
        tool: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Value;
}

/// Equal-length comparison whose time does not depend on where they differ.
pub fn same_token(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A fresh 32-byte token as hex.
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// One listening bridge.
pub struct Bridge {
    path: PathBuf,
    token: String,
    stop: CancellationToken,
    push: Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>,
}

impl Bridge {
    /// Binds `path` (replacing a stale socket) and serves `host`.
    pub fn start(path: &Path, host: Arc<dyn BridgeHost>) -> anyhow::Result<Bridge> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)
            .map_err(|e| anyhow::anyhow!("bridge socket {}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let token = new_token();
        let stop = CancellationToken::new();
        let push = Arc::new(Mutex::new(None));
        let (t, s, p) = (token.clone(), stop.clone(), push.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = s.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let (host, token, stop, push) = (host.clone(), t.clone(), s.clone(), p.clone());
                            tokio::spawn(async move { serve(stream, host, token, stop, push).await });
                        }
                        Err(e) => {
                            tracing::warn!("pi bridge: accept failed: {e}");
                            break;
                        }
                    },
                }
            }
        });
        Ok(Bridge {
            path: path.to_path_buf(),
            token,
            stop,
            push,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Sends a message to the connected extension (dropped when none is).
    pub fn push(&self, message: Value) {
        if let Some(tx) = self.push.lock().unwrap().as_ref() {
            let _ = tx.send(message);
        }
    }

    /// Stops listening, cancels running calls and removes the socket.
    pub fn stop(&self) {
        self.stop.cancel();
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn serve(
    stream: UnixStream,
    host: Arc<dyn BridgeHost>,
    token: String,
    stop: CancellationToken,
    push: Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let mut line = serde_json::to_vec(&message).unwrap_or_default();
            line.push(b'\n');
            if writer.write_all(&line).await.is_err() {
                break;
            }
        }
    });
    let calls: Arc<Mutex<HashMap<String, CancellationToken>>> = Arc::default();
    let mut splitter = LineSplitter::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut authed = false;
    'read: loop {
        let n = tokio::select! {
            _ = stop.cancelled() => break,
            n = reader.read(&mut buf) => match n { Ok(0) | Err(_) => break, Ok(n) => n },
        };
        for line in splitter.push(&buf[..n]) {
            let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                break 'read;
            };
            let kind = message["type"].as_str().unwrap_or_default();
            if !authed {
                if kind != "hello" || !same_token(message["token"].as_str().unwrap_or(""), &token) {
                    tracing::warn!("pi bridge: rejected a connection without the token");
                    break 'read;
                }
                authed = true;
                *push.lock().unwrap() = Some(tx.clone());
                let mut config = host.config();
                config["type"] = json!("config");
                let _ = tx.send(config);
                continue;
            }
            let id = message["id"].clone();
            match kind {
                "gate" => {
                    let answer = match host.gate(
                        message["tool"].as_str().unwrap_or_default(),
                        &message["input"],
                    ) {
                        Ok(()) => json!({"type": "gate", "id": id, "allow": true}),
                        Err(reason) => {
                            json!({"type": "gate", "id": id, "allow": false, "reason": reason})
                        }
                    };
                    let _ = tx.send(answer);
                }
                "call" => {
                    let cancel = stop.child_token();
                    let key = id.to_string();
                    calls.lock().unwrap().insert(key.clone(), cancel.clone());
                    let (host, tx, calls) = (host.clone(), tx.clone(), calls.clone());
                    tokio::spawn(async move {
                        let call_id = message["callId"].as_str().unwrap_or_default().to_owned();
                        let tool = message["tool"].as_str().unwrap_or_default().to_owned();
                        let mut result = host
                            .call(&call_id, &tool, message["args"].clone(), cancel)
                            .await;
                        calls.lock().unwrap().remove(&key);
                        result["type"] = json!("result");
                        result["id"] = id;
                        let _ = tx.send(result);
                    });
                }
                "cancel" => {
                    if let Some(c) = calls.lock().unwrap().remove(&id.to_string()) {
                        c.cancel();
                    }
                }
                _ => {}
            }
        }
    }
    for (_, c) in calls.lock().unwrap().drain() {
        c.cancel();
    }
    push.lock().unwrap().take();
    drop(tx);
    let _ = writer_task.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        let t = new_token();
        assert_eq!(t.len(), 64);
        assert!(same_token(&t, &t.clone()));
        assert!(!same_token(&t, &t[..63]));
        assert!(!same_token("abcd", "abce"));
    }
}
