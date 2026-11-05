//! Pi's RPC wire: strict JSONL (split only at LF, strip a preceding CR;
//! U+2028/U+2029 stay inside records), command/response correlation by `id`,
//! and a bounded stderr tail for crash reports.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

/// Splits a byte stream into records at LF only.
#[derive(Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(rel) = chunk[start..].iter().position(|b| *b == b'\n') {
            let end = start + rel;
            let mut line = std::mem::take(&mut self.buf);
            line.extend_from_slice(&chunk[start..end]);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if !line.is_empty() {
                out.push(line);
            }
            start = end + 1;
        }
        self.buf.extend_from_slice(&chunk[start..]);
        out
    }
}

/// The last `cap` bytes written.
pub struct Ring {
    cap: usize,
    data: VecDeque<u8>,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Ring {
            cap,
            data: VecDeque::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes);
        while self.data.len() > self.cap {
            self.data.pop_front();
        }
    }

    /// The tail as text, at most `max` characters.
    pub fn tail(&self, max: usize) -> String {
        let bytes: Vec<u8> = self.data.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        let chars: Vec<char> = text.chars().collect();
        let from = chars.len().saturating_sub(max);
        chars[from..].iter().collect::<String>().trim().to_owned()
    }
}

/// Command side of one Pi process.
pub struct Rpc {
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    next: AtomicU64,
}

impl Rpc {
    pub fn new(stdin: tokio::process::ChildStdin) -> Self {
        Rpc {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// Writes one record (a command without reply, or an
    /// `extension_ui_response`).
    pub async fn send(&self, record: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("the Pi engine is shutting down"))?;
        pipe.write_all(&line).await?;
        pipe.flush().await?;
        Ok(())
    }

    /// Sends `command` with a fresh `id` and waits for its response; a
    /// `success: false` response is an error with Pi's message. Returns `data`.
    pub async fn request(&self, mut command: Value, timeout: Duration) -> anyhow::Result<Value> {
        let id = format!("a{}", self.next.fetch_add(1, Ordering::SeqCst));
        command["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        if let Err(e) = self.send(&command).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        let kind = command["type"].as_str().unwrap_or("command").to_owned();
        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => anyhow::bail!("the Pi engine exited during {kind}"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                anyhow::bail!("Pi did not answer {kind} within {}s", timeout.as_secs())
            }
        };
        if response["success"] == true {
            Ok(response.get("data").cloned().unwrap_or(Value::Null))
        } else {
            let error = response["error"].as_str().unwrap_or("command failed");
            anyhow::bail!("{error}")
        }
    }

    /// Routes a stdout record: a response to a waiting request is consumed
    /// (`None`), everything else is returned.
    pub fn route(&self, record: Value) -> Option<Value> {
        if record["type"] == "response"
            && let Some(id) = record["id"].as_str()
            && let Some(tx) = self.pending.lock().unwrap().remove(id)
        {
            let _ = tx.send(record);
            return None;
        }
        if record["type"] == "response" {
            // A response nobody waits for (timed out, parse error): not an event.
            return None;
        }
        Some(record)
    }

    /// Closing stdin asks Pi for an orderly shutdown.
    pub async fn close(&self) {
        self.stdin.lock().await.take();
    }

    /// Fails every waiting request (the process exited).
    pub fn fail_all(&self) {
        self.pending.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_only_at_lf() {
        let mut s = LineSplitter::default();
        let rec = "{\"t\":\"a\u{2028}b\"}";
        let mut out = s.push(format!("{rec}\r\n{{\"x\":").as_bytes());
        out.extend(s.push(b"1}\n\n"));
        let texts: Vec<String> = out
            .into_iter()
            .map(|l| String::from_utf8(l).unwrap())
            .collect();
        assert_eq!(texts, vec![rec.to_owned(), "{\"x\":1}".to_owned()]);
    }

    #[test]
    fn ring_keeps_the_tail() {
        let mut r = Ring::new(8);
        r.push(b"0123456789abc");
        assert_eq!(r.tail(100), "56789abc");
        assert_eq!(r.tail(3), "abc");
    }
}
