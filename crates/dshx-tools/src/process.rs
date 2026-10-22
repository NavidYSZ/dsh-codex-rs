//! Process plumbing for model commands (dsh `subprocess-local/spawn.ts`): a
//! detached process group, bounded stdout/stderr capture with spill files, an
//! optional live output hook (job rings), SIGTERM → SIGKILL escalation for the
//! whole group, and a bounded wait for inherited pipes after the exit.
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use crate::output::{Collected, OutputCollector, StreamRead};

/// SIGTERM → SIGKILL grace, also the wait for inherited pipes (dsh `graceMs`).
pub const DEFAULT_GRACE: Duration = Duration::from_millis(3_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stdout,
    Stderr,
}

/// Live output callback: called with decoded text in stream order.
pub type OutputHook = Arc<dyn Fn(Channel, &str) + Send + Sync>;

pub struct SpawnSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub stdout_max: usize,
    pub stderr_max: usize,
    pub max_spill: Option<u64>,
    pub grace: Duration,
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    /// Terminating signal name (`SIGTERM`), when signal-killed.
    pub signal: Option<String>,
    /// dsh status `killed`: a kill was requested or a signal ended it.
    pub killed: bool,
    /// The process never started (`spawn bash ENOENT`).
    pub spawn_error: Option<String>,
}

struct Inner {
    pid: Option<i32>,
    stdout: Mutex<OutputCollector>,
    stderr: Mutex<OutputCollector>,
    kill_requested: AtomicBool,
    settled: watch::Sender<Option<Outcome>>,
    grace: Duration,
}

/// A started command. Cheap to clone; output stays readable after the exit.
#[derive(Clone)]
pub struct Process {
    inner: Arc<Inner>,
}

/// Name of a POSIX signal number (Node's `NodeJS.Signals` spelling).
pub fn signal_name(signal: i32) -> String {
    let known: &[(i32, &str)] = &[
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGCHLD, "SIGCHLD"),
        (libc::SIGCONT, "SIGCONT"),
        (libc::SIGSTOP, "SIGSTOP"),
        (libc::SIGTSTP, "SIGTSTP"),
        (libc::SIGTTIN, "SIGTTIN"),
        (libc::SIGTTOU, "SIGTTOU"),
        (libc::SIGURG, "SIGURG"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
        (libc::SIGVTALRM, "SIGVTALRM"),
        (libc::SIGPROF, "SIGPROF"),
        (libc::SIGWINCH, "SIGWINCH"),
        (libc::SIGIO, "SIGIO"),
        (libc::SIGSYS, "SIGSYS"),
    ];
    known
        .iter()
        .find(|(number, _)| *number == signal)
        .map(|(_, name)| (*name).to_owned())
        .unwrap_or_else(|| format!("SIG{signal}"))
}

/// Node's spelling of a failed spawn (`spawn bash ENOENT`).
pub(crate) fn spawn_error_text(program: &str, error: &std::io::Error) -> String {
    let code = match error.raw_os_error() {
        Some(libc::ENOENT) => Some("ENOENT"),
        Some(libc::EACCES) => Some("EACCES"),
        Some(libc::ENOTDIR) => Some("ENOTDIR"),
        Some(libc::EPERM) => Some("EPERM"),
        Some(libc::E2BIG) => Some("E2BIG"),
        _ => None,
    };
    match code {
        Some(code) => format!("spawn {program} {code}"),
        None => format!("spawn {program}: {error}"),
    }
}

pub(crate) fn signal_group(pid: i32, signal: i32) -> bool {
    // SAFETY: plain kill(2) on our own child's process group; failures (ESRCH)
    // are reported through the return value only.
    unsafe { libc::kill(-pid, signal) == 0 }
}

/// Decodes a byte stream into UTF-8 text without splitting characters
/// between chunks.
#[derive(Default)]
struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    fn push(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let valid_up_to = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let rest = self.pending.split_off(valid_up_to);
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending = rest;
        text
    }

    fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

async fn pump<R: tokio::io::AsyncRead + Unpin>(
    mut stream: R,
    inner: Arc<Inner>,
    channel: Channel,
    hook: Option<OutputHook>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry = Utf8Carry::default();
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let collector = match channel {
            Channel::Stdout => &inner.stdout,
            Channel::Stderr => &inner.stderr,
        };
        collector.lock().unwrap().push(&buf[..n]);
        if let Some(hook) = &hook {
            let text = carry.push(&buf[..n]);
            if !text.is_empty() {
                hook(channel, &text);
            }
        }
    }
    if let Some(hook) = &hook {
        let text = carry.finish();
        if !text.is_empty() {
            hook(channel, &text);
        }
    }
}

impl Process {
    /// Starts `spec.argv` in its own process group. Never fails: a spawn error
    /// settles the handle at once with `spawn_error` set.
    pub fn spawn(spec: SpawnSpec, hook: Option<OutputHook>) -> Process {
        let (settled, _) = watch::channel(None);
        let program = spec.argv.first().cloned().unwrap_or_default();
        let mut command = tokio::process::Command::new(&program);
        command
            .args(spec.argv.iter().skip(1))
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(false);
        let spawned = command.spawn();
        let pid = spawned
            .as_ref()
            .ok()
            .and_then(|child| child.id())
            .map(|id| id as i32);
        let inner = Arc::new(Inner {
            pid,
            stdout: Mutex::new(OutputCollector::new(
                "stdout",
                spec.stdout_max,
                spec.max_spill,
            )),
            stderr: Mutex::new(OutputCollector::new(
                "stderr",
                spec.stderr_max,
                spec.max_spill,
            )),
            kill_requested: AtomicBool::new(false),
            settled,
            grace: spec.grace,
        });
        let process = Process {
            inner: inner.clone(),
        };
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                inner.settled.send_replace(Some(Outcome {
                    exit_code: None,
                    signal: None,
                    killed: true,
                    spawn_error: Some(spawn_error_text(&program, &error)),
                }));
                return process;
            }
        };
        let stdout = child
            .stdout
            .take()
            .map(|s| tokio::spawn(pump(s, inner.clone(), Channel::Stdout, hook.clone())));
        let stderr = child
            .stderr
            .take()
            .map(|s| tokio::spawn(pump(s, inner.clone(), Channel::Stderr, hook)));
        tokio::spawn(async move {
            let status = child.wait().await;
            // Inherited pipes (a backgrounded grandchild) may stay open: give
            // them the grace period, then stop reading.
            let readers: Vec<_> = stdout.into_iter().chain(stderr).collect();
            let aborts: Vec<_> = readers.iter().map(|r| r.abort_handle()).collect();
            let drained =
                tokio::time::timeout(inner.grace, futures::future::join_all(readers)).await;
            if drained.is_err() {
                for abort in aborts {
                    abort.abort();
                }
            }
            inner.stdout.lock().unwrap().seal();
            inner.stderr.lock().unwrap().seal();
            let (exit_code, signal) = match status {
                Ok(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    (status.code(), status.signal().map(signal_name))
                }
                Err(_) => (None, None),
            };
            let killed = inner.kill_requested.load(Ordering::SeqCst) || signal.is_some();
            inner.settled.send_replace(Some(Outcome {
                exit_code,
                signal,
                killed,
                spawn_error: None,
            }));
        });
        process
    }

    pub fn pid(&self) -> Option<i32> {
        self.inner.pid
    }

    pub fn is_settled(&self) -> bool {
        self.inner.settled.borrow().is_some()
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.inner.settled.borrow().clone()
    }

    /// Terminates the process group: SIGTERM now, SIGKILL after the grace
    /// period if anything in the group survived. False when the process had
    /// already settled or a kill was already requested.
    pub fn kill(&self) -> bool {
        if self.is_settled() || self.inner.kill_requested.swap(true, Ordering::SeqCst) {
            return false;
        }
        let Some(pid) = self.inner.pid else {
            return true;
        };
        signal_group(pid, libc::SIGTERM);
        let grace = self.inner.grace;
        let escalate = move || {
            // Signal 0 probes whether any member of the group is still alive.
            if signal_group(pid, 0) {
                signal_group(pid, libc::SIGKILL);
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    tokio::time::sleep(grace).await;
                    escalate();
                });
            }
            Err(_) => {
                std::thread::spawn(move || {
                    std::thread::sleep(grace);
                    escalate();
                });
            }
        }
        true
    }

    pub fn kill_requested(&self) -> bool {
        self.inner.kill_requested.load(Ordering::SeqCst)
    }

    /// Waits until the process exited and its pipes drained.
    pub async fn wait(&self) -> Outcome {
        let mut rx = self.inner.settled.subscribe();
        let outcome = rx.wait_for(Option::is_some).await.map(|o| o.clone());
        match outcome {
            Ok(Some(outcome)) => outcome,
            _ => Outcome {
                exit_code: None,
                signal: None,
                killed: true,
                spawn_error: None,
            },
        }
    }

    pub fn stdout(&self) -> Collected {
        self.inner.stdout.lock().unwrap().finalize()
    }

    pub fn stderr(&self) -> Collected {
        self.inner.stderr.lock().unwrap().finalize()
    }

    pub fn read_stream(&self, channel: Channel, from: u64) -> StreamRead {
        match channel {
            Channel::Stdout => self.inner.stdout.lock().unwrap().read_from(from),
            Channel::Stderr => self.inner.stderr.lock().unwrap().read_from(from),
        }
    }

    /// Spill files of both streams, when they overflowed.
    pub fn spill_paths(&self) -> Vec<String> {
        [Channel::Stdout, Channel::Stderr]
            .into_iter()
            .filter_map(|channel| self.read_stream(channel, u64::MAX).spill_path)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cmd: &str) -> SpawnSpec {
        SpawnSpec {
            argv: vec!["bash".into(), "-c".into(), cmd.into()],
            cwd: std::env::temp_dir(),
            env: std::env::vars_os().collect(),
            stdout_max: 1000,
            stderr_max: 1000,
            max_spill: Some(10_000),
            grace: Duration::from_millis(300),
        }
    }

    #[tokio::test]
    async fn captures_split_streams_and_exit_code() {
        let p = Process::spawn(spec("echo out; echo err >&2; exit 3"), None);
        let outcome = p.wait().await;
        assert_eq!(outcome.exit_code, Some(3));
        assert!(!outcome.killed);
        assert_eq!(p.stdout().text, "out\n");
        assert_eq!(p.stderr().text, "err\n");
    }

    #[tokio::test]
    async fn kill_terminates_the_group() {
        let p = Process::spawn(spec("sleep 30 & sleep 30; echo never"), None);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(p.kill());
        assert!(!p.kill());
        let outcome = tokio::time::timeout(Duration::from_secs(5), p.wait())
            .await
            .unwrap();
        assert!(outcome.killed);
        assert_eq!(outcome.signal.as_deref(), Some("SIGTERM"));
    }

    #[tokio::test]
    async fn spawn_failure_settles_immediately() {
        let mut s = spec("true");
        s.cwd = PathBuf::from("/nonexistent/dir/for/dshx");
        let p = Process::spawn(s, None);
        let outcome = p.wait().await;
        assert_eq!(outcome.spawn_error.as_deref(), Some("spawn bash ENOENT"));
    }

    #[tokio::test]
    async fn hook_sees_live_output() {
        let seen = Arc::new(Mutex::new(String::new()));
        let sink = seen.clone();
        let hook: OutputHook = Arc::new(move |_, text| sink.lock().unwrap().push_str(text));
        let p = Process::spawn(spec("printf 'h\\xc3\\xa9llo'"), Some(hook));
        p.wait().await;
        assert_eq!(*seen.lock().unwrap(), "héllo");
    }
}
