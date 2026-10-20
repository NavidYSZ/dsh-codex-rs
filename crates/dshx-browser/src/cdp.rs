//! DevTools protocol over `--remote-debugging-pipe`: Chrome reads commands on
//! fd 3 and writes replies and events on fd 4, one NUL-terminated JSON
//! message each. No debugging port is ever opened.
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::js;

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// An event Chrome pushed (a message with `method` and no pending `id`).
pub(crate) struct Event {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Reply = Result<Value, String>;

pub(crate) struct Cdp {
    /// Outgoing messages; a writer task owns the pipe so a cancelled caller
    /// can never leave half a message in it.
    outbox: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

/// Removes a pending call when its caller gives up (timeout or cancellation).
struct PendingGuard<'a> {
    cdp: &'a Cdp,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.cdp.pending.lock().unwrap().remove(&self.id);
    }
}

impl Cdp {
    /// Wires up both pipe ends and starts the reader and writer tasks.
    /// `on_event` runs on the reader task and must not block; `on_close`
    /// runs once when Chrome closes the pipe.
    pub fn start(
        writer: OwnedFd,
        reader: OwnedFd,
        mut on_event: impl FnMut(&Arc<Cdp>, Event) + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> io::Result<Arc<Cdp>> {
        let mut tx = pipe::Sender::from_owned_fd(writer)?;
        let mut rx = pipe::Receiver::from_owned_fd(reader)?;
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let cdp = Arc::new(Cdp {
            outbox: Mutex::new(Some(out_tx)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if tx.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            // Dropping `tx` closes fd 3 on our side; Chrome then shuts down.
        });
        let reader_cdp = cdp.clone();
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
            let mut chunk = vec![0u8; 1 << 16];
            let mut scanned = 0;
            loop {
                let n = match rx.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                while let Some(end) = buf[scanned..].iter().position(|&b| b == 0) {
                    let end = scanned + end;
                    let msg = serde_json::from_slice::<Value>(&buf[..end]).ok();
                    buf.drain(..=end);
                    scanned = 0;
                    if let Some(msg) = msg {
                        reader_cdp.dispatch(msg, &mut on_event);
                    }
                }
                scanned = buf.len();
            }
            reader_cdp.fail_all();
            on_close();
        });
        Ok(cdp)
    }

    fn dispatch(self: &Arc<Self>, mut msg: Value, on_event: &mut impl FnMut(&Arc<Cdp>, Event)) {
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            let waiter = self.pending.lock().unwrap().remove(&id);
            if let Some(waiter) = waiter {
                let reply = match msg.get("error") {
                    Some(err) if !err.is_null() => {
                        let message = js::display(err.get("message"));
                        Err(match err.get("data") {
                            Some(d) if is_truthy(d) => {
                                format!("{message}: {}", js::display(Some(d)))
                            }
                            _ => message,
                        })
                    }
                    _ => Ok(match msg.get_mut("result").map(Value::take) {
                        Some(r) if !r.is_null() => r,
                        _ => Value::Object(Map::new()),
                    }),
                };
                let _ = waiter.send(reply);
                return;
            }
        }
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let event = Event {
            method,
            params: msg
                .get_mut("params")
                .map(Value::take)
                .unwrap_or(Value::Null),
            session_id: msg
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        on_event(self, event);
    }

    fn fail_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        // Dropping the senders wakes every waiter with "browser closed".
        self.pending.lock().unwrap().clear();
        self.outbox.lock().unwrap().take();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Closes our end of the command pipe.
    pub fn shutdown(&self) {
        self.outbox.lock().unwrap().take();
    }

    /// Sends one command and waits for its result.
    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _guard = PendingGuard { cdp: self, id };
        if self.is_closed() {
            return Err("browser closed".into());
        }
        let mut msg = Map::new();
        msg.insert("id".into(), id.into());
        msg.insert("method".into(), method.into());
        msg.insert("params".into(), params);
        if let Some(s) = session_id {
            msg.insert("sessionId".into(), s.into());
        }
        let mut bytes = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
        bytes.push(0);
        let queued = match self.outbox.lock().unwrap().as_ref() {
            Some(out) => out.send(bytes).is_ok(),
            None => false,
        };
        if !queued {
            return Err("browser closed".into());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("browser closed".into()),
            Err(_) => Err(format!(
                "browser did not answer {method} within {}s",
                js::num(timeout.as_secs_f64())
            )),
        }
    }

    /// Fire-and-forget command (dialog answers, screencast acks).
    pub fn post(self: &Arc<Self>, method: &'static str, params: Value, session_id: Option<String>) {
        let cdp = self.clone();
        tokio::spawn(async move {
            let _ = cdp
                .send(method, params, session_id.as_deref(), DEFAULT_TIMEOUT)
                .await;
        });
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Starts Chrome in its own process group with the DevTools pipe on fds 3
/// and 4. Returns the child and our ends (command writer, event reader).
pub(crate) fn spawn_chrome(
    binary: &Path,
    args: &[String],
) -> io::Result<(Child, OwnedFd, OwnedFd)> {
    let (cmd_read, cmd_write) = io::pipe()?; // we write commands, Chrome reads fd 3
    let (evt_read, evt_write) = io::pipe()?; // Chrome writes fd 4, we read
    let fd3 = cmd_read.as_raw_fd();
    let fd4 = evt_write.as_raw_fd();
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Own group, so renderers and helpers can be killed together.
        .process_group(0);
    // SAFETY: the closure runs between fork and exec and only calls the
    // async-signal-safe fcntl/dup2/close on descriptors that stay open in the
    // parent until `spawn` returns. Both are first moved above fd 10 so the
    // dup2 calls cannot clobber each other, and dup2 clears close-on-exec.
    unsafe {
        command.pre_exec(move || {
            let a = libc::fcntl(fd3, libc::F_DUPFD, 10);
            let b = libc::fcntl(fd4, libc::F_DUPFD, 10);
            if a < 0 || b < 0 || libc::dup2(a, 3) < 0 || libc::dup2(b, 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::close(a);
            libc::close(b);
            Ok(())
        });
    }
    let child = command.spawn()?;
    // Only Chrome may hold its ends, or we would never see EOF when it exits.
    drop(cmd_read);
    drop(evt_write);
    Ok((child, cmd_write.into(), evt_read.into()))
}

/// SIGKILL to Chrome's whole process group.
pub(crate) fn kill_group(pgid: i32) {
    if pgid > 0 {
        // SAFETY: plain syscall on a process group we created (Chrome is its
        // leader, see `process_group(0)`); no memory is touched.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Whether any process of the group still exists (zombies included).
pub(crate) fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permissions.
    pgid > 0 && unsafe { libc::kill(-pgid, 0) } == 0
}
