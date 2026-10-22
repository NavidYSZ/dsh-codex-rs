//! stdio transport: a child process speaking newline-delimited JSON-RPC.
//!
//! The child runs in its own process group (Unix) so shutdown reaches every
//! process the server started: close stdin, wait, then SIGTERM and SIGKILL
//! to the group. Its environment is the parent's without credential-like
//! names and `DSH_*` facts (dsh `scrubbedParentEnv`), plus the configured env.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, watch};

use crate::error::McpError;
use crate::log::ServerLog;
use crate::peer::{Incoming, Transport};

/// How long a server may take to exit after its stdin closed, and after SIGTERM.
const EXIT_GRACE: Duration = Duration::from_secs(2);

pub(crate) struct StdioTransport {
    stdin: Mutex<Option<ChildStdin>>,
    pid: Option<u32>,
    exited: watch::Receiver<bool>,
    /// Set once `close` finished; the drop guard then has nothing to do.
    shut_down: AtomicBool,
    label: String,
}

/// The child environment: scrubbed parent environment plus `extra`.
pub(crate) fn child_env(extra: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| {
            !dshx_tools::env::is_sensitive(k)
                && !k
                    .to_ascii_uppercase()
                    .starts_with(dshx_tools::env::DSH_ENV_PREFIX)
        })
        .collect();
    for (k, v) in extra {
        env.retain(|(key, _)| key != k);
        env.push((k.clone(), v.clone()));
    }
    env
}

impl StdioTransport {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&PathBuf>,
        label: &str,
        log: Arc<ServerLog>,
    ) -> Result<(Arc<StdioTransport>, mpsc::UnboundedReceiver<Incoming>), McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .env_clear()
            .envs(child_env(env))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| McpError::Transport(format!("spawn {command}: {e}")))?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (inbox, rx) = mpsc::unbounded_channel();
        let (exited_tx, exited) = watch::channel(false);

        let reader_inbox = inbox.clone();
        let reader_label = label.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(line) {
                            Ok(message) => {
                                let _ = reader_inbox.send(Incoming::Message(message));
                            }
                            Err(e) => {
                                tracing::debug!("{reader_label}: ignoring non-JSON output: {e}")
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!("{reader_label}: stdout read failed: {e}");
                        break;
                    }
                }
            }
            let _ = reader_inbox.send(Incoming::Closed("server closed its stdout".into()));
        });

        let err_label = label.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("{err_label} stderr: {line}");
                log.push("stderr", line);
            }
        });

        tokio::spawn(async move {
            let status = child.wait().await;
            let _ = exited_tx.send(true);
            let reason = match status {
                Ok(status) => format!("server process exited ({status})"),
                Err(e) => format!("server process wait failed: {e}"),
            };
            let _ = inbox.send(Incoming::Closed(reason));
        });

        Ok((
            Arc::new(StdioTransport {
                stdin: Mutex::new(stdin),
                pid,
                exited,
                shut_down: AtomicBool::new(false),
                label: label.to_owned(),
            }),
            rx,
        ))
    }

    async fn wait_exit(&self, timeout: Duration) -> bool {
        let mut exited = self.exited.clone();
        tokio::time::timeout(timeout, exited.wait_for(|e| *e))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Signals the whole process group; `false` when no process is left.
    #[cfg(unix)]
    fn signal_group(&self, signal: libc::c_int) -> bool {
        let Some(pid) = self.pid else { return false };
        // SAFETY: plain syscall on the process group this transport created
        // (`process_group(0)` makes the child's pid the group id).
        unsafe { libc::kill(-(pid as libc::pid_t), signal) == 0 }
    }

    #[cfg(unix)]
    async fn reap_group(&self) {
        if !self.signal_group(libc::SIGTERM) {
            return;
        }
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if !self.signal_group(0) {
                return;
            }
        }
        self.signal_group(libc::SIGKILL);
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, message: Value) -> Result<(), McpError> {
        let mut line =
            serde_json::to_string(&message).map_err(|e| McpError::Protocol(e.to_string()))?;
        line.push('\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(McpError::NotConnected)?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(format!("write to server: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(format!("write to server: {e}")))
    }

    async fn close(&self) -> bool {
        // Closing stdin is the polite stop signal for stdio servers.
        // A write blocked on a full pipe holds the lock; the signals below end it.
        if let Ok(mut stdin) = tokio::time::timeout(EXIT_GRACE, self.stdin.lock()).await {
            drop(stdin.take());
        }
        let mut gone = self.wait_exit(EXIT_GRACE).await;
        #[cfg(unix)]
        {
            if !gone {
                self.signal_group(libc::SIGTERM);
                gone = self.wait_exit(EXIT_GRACE).await;
            }
            if !gone {
                self.signal_group(libc::SIGKILL);
                gone = self.wait_exit(EXIT_GRACE).await;
            }
            // Grandchildren the server left behind share its process group.
            self.reap_group().await;
        }
        self.shut_down.store(true, Ordering::SeqCst);
        if !gone {
            tracing::error!(
                "{}: server process did not exit during shutdown",
                self.label
            );
        }
        gone
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Last resort when close() never ran; kill_on_drop covers the child only.
        #[cfg(unix)]
        if !self.shut_down.load(Ordering::SeqCst) {
            self.signal_group(libc::SIGKILL);
        }
    }
}
