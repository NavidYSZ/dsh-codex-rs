//! One `pi --mode rpc` child: arguments, a cleared environment with an
//! allowlist, its own process group, the sandbox around the whole process,
//! and the stdout/stderr readers.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dshx_core::SandboxMode;
use dshx_tools::sandbox::{self, Runner};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::install::Install;
use crate::rpc::{LineSplitter, Ring, Rpc};

/// Bytes of stderr kept for crash reports.
const STDERR_TAIL: usize = 64 * 1024;

/// Aether's Pi folders below `$DSHX_HOME/pi`.
#[derive(Debug, Clone)]
pub struct PiDirs {
    pub root: PathBuf,
}

impl PiDirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        PiDirs { root: root.into() }
    }
    /// `PI_CODING_AGENT_DIR`: Aether's own, never the user's `~/.pi`.
    pub fn agent(&self) -> PathBuf {
        self.root.join("agent")
    }
    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }
    /// Bridge sockets (mode 0700).
    pub fn run(&self) -> PathBuf {
        self.root.join("run")
    }
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }
    pub fn ext(&self) -> PathBuf {
        self.root.join("ext")
    }

    pub fn create(&self) -> std::io::Result<()> {
        for dir in [
            self.agent(),
            self.sessions(),
            self.run(),
            self.tmp(),
            self.ext(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(self.run(), std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(self.agent(), std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Pi's session file of an Aether chat (`<time>_<id>.jsonl`).
    pub fn session_files(&self, session_id: &str) -> Vec<PathBuf> {
        let suffix = format!("_{session_id}.jsonl");
        std::fs::read_dir(self.sessions())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&suffix))
            })
            .collect()
    }

    fn writable(&self) -> Vec<PathBuf> {
        vec![self.agent(), self.sessions(), self.run(), self.tmp()]
    }
}

/// What one start of Pi needs.
pub struct Spawn {
    pub install: Install,
    pub dirs: PiDirs,
    pub cwd: PathBuf,
    pub session_id: String,
    pub name: Option<String>,
    pub extensions: Vec<PathBuf>,
    /// Socket and token of the Aether bridge.
    pub bridge: Option<(PathBuf, String)>,
    pub sandbox: SandboxMode,
    pub runner: Runner,
}

/// What the process says.
#[derive(Debug)]
pub enum Output {
    /// An event or extension UI record (responses go to waiting requests).
    Record(Value),
    /// The process ended; `stderr` is its tail.
    Exited { code: Option<i32>, stderr: String },
}

/// Environment variables passed through to Pi (everything else is dropped).
fn allowed_env(key: &str) -> bool {
    matches!(key, "PATH" | "HOME" | "USER" | "LOGNAME" | "LANG" | "SHELL") || key.starts_with("LC_")
}

pub fn argv(spec: &Spawn) -> Vec<String> {
    let mut args = vec![
        spec.install.node.display().to_string(),
        spec.install.cli().display().to_string(),
        "--mode".into(),
        "rpc".into(),
        "--session-dir".into(),
        spec.dirs.sessions().display().to_string(),
        "--session-id".into(),
        spec.session_id.clone(),
        "--no-extensions".into(),
    ];
    for ext in &spec.extensions {
        args.push("-e".into());
        args.push(ext.display().to_string());
    }
    args.extend(
        [
            "--no-skills",
            "--no-mcp",
            "--no-approve",
            "--no-prompt-templates",
            "--no-themes",
        ]
        .map(String::from),
    );
    if let Some(name) = spec.name.as_deref().filter(|n| !n.trim().is_empty()) {
        args.push("--name".into());
        args.push(name.chars().take(120).collect());
    }
    args
}

fn sbpl(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Folders the whole Pi process may write in `mode` (`None` = no fence).
pub fn writable_roots(mode: SandboxMode, cwd: &Path, dirs: &PiDirs) -> Option<Vec<PathBuf>> {
    let mut roots = match mode {
        SandboxMode::DangerFullAccess => return None,
        SandboxMode::WorkspaceWrite => sandbox::writable_roots(mode, cwd),
        SandboxMode::ReadOnly => Vec::new(),
    };
    for dir in dirs.writable() {
        let dir = sandbox::canonical_path(&dir);
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    Some(roots)
}

/// `inner` wrapped by the host's runner for `mode`.
pub fn confine(
    runner: &Runner,
    inner: Vec<String>,
    mode: SandboxMode,
    cwd: &Path,
    dirs: &PiDirs,
) -> Vec<String> {
    let Some(roots) = writable_roots(mode, cwd, dirs) else {
        return inner;
    };
    let mut argv: Vec<String> = match runner {
        Runner::Seatbelt => {
            let mut forms = vec![
                "(version 1)".to_owned(),
                "(allow default)".to_owned(),
                "(deny file-write*)".to_owned(),
                format!(
                    "(allow file-write* (literal {}))",
                    sbpl(Path::new("/dev/null"))
                ),
            ];
            let subpaths: Vec<String> = roots
                .iter()
                .map(|r| format!("(subpath {})", sbpl(r)))
                .collect();
            forms.push(format!("(allow file-write* {})", subpaths.join(" ")));
            vec!["/usr/bin/sandbox-exec".into(), "-p".into(), forms.join(" ")]
        }
        Runner::Bwrap | Runner::Command { .. } => {
            let mut prefix = match runner {
                Runner::Command { argv, .. } => argv.clone(),
                _ => vec!["bwrap".to_owned()],
            };
            prefix.extend(
                [
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                    "--unshare-pid",
                    "--proc",
                    "/proc",
                    "--die-with-parent",
                ]
                .map(String::from),
            );
            if mode == SandboxMode::WorkspaceWrite {
                prefix.extend(["--tmpfs".into(), "/tmp".into()]);
            }
            for root in &roots {
                let r = root.display().to_string();
                if r == "/tmp" {
                    continue;
                }
                prefix.extend(["--bind".into(), r.clone(), r]);
            }
            prefix
        }
        Runner::Unconfined => return inner,
    };
    argv.push("--".into());
    argv.extend(inner);
    argv
}

/// A running Pi.
pub struct PiProcess {
    pub rpc: Arc<Rpc>,
    pid: Option<i32>,
    exited: Arc<AtomicBool>,
    stderr: Arc<Mutex<Ring>>,
}

fn signal_group(pid: i32, signal: i32) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid, signal);
    }
}

impl PiProcess {
    pub fn spawn(spec: &Spawn) -> anyhow::Result<(PiProcess, mpsc::UnboundedReceiver<Output>)> {
        spec.dirs.create()?;
        let inner = argv(spec);
        let full = confine(&spec.runner, inner, spec.sandbox, &spec.cwd, &spec.dirs);
        let mut cmd = tokio::process::Command::new(&full[0]);
        cmd.args(&full[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(std::env::vars_os().filter_map(|(k, v)| {
                let key = k.to_str()?;
                allowed_env(key).then(|| (k.clone(), v))
            }))
            .env("TERM", "dumb")
            .env("TMPDIR", spec.dirs.tmp())
            .env("PI_CODING_AGENT_DIR", spec.dirs.agent())
            .env("PI_TELEMETRY", "0")
            .env("PI_OFFLINE", "1")
            .env("PI_SKIP_VERSION_CHECK", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some((socket, token)) = &spec.bridge {
            cmd.env("AETHER_BRIDGE", socket)
                .env("AETHER_BRIDGE_TOKEN", token);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("starting Pi ({}): {e}", full[0]))?;
        let pid = child.id().map(|p| p as i32);
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let rpc = Arc::new(Rpc::new(stdin));
        let stderr = Arc::new(Mutex::new(Ring::new(STDERR_TAIL)));
        let exited = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::unbounded_channel();

        let err_ring = stderr.clone();
        let err_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            while let Ok(n) = stderr_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                err_ring.lock().unwrap().push(&buf[..n]);
            }
        });
        let reader_rpc = rpc.clone();
        let reader_ring = stderr.clone();
        let reader_exited = exited.clone();
        tokio::spawn(async move {
            let mut splitter = LineSplitter::default();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for line in splitter.push(&buf[..n]) {
                            match serde_json::from_slice::<Value>(&line) {
                                Ok(record) => {
                                    if let Some(record) = reader_rpc.route(record) {
                                        let _ = tx.send(Output::Record(record));
                                    }
                                }
                                Err(_) => tracing::debug!(
                                    "pi: unparsable stdout record ({} bytes)",
                                    line.len()
                                ),
                            }
                        }
                    }
                }
            }
            reader_rpc.fail_all();
            let status = child.wait().await.ok();
            let _ = tokio::time::timeout(Duration::from_secs(1), err_task).await;
            reader_exited.store(true, Ordering::SeqCst);
            let code = status.and_then(|s| s.code());
            let tail = reader_ring.lock().unwrap().tail(4000);
            let _ = tx.send(Output::Exited { code, stderr: tail });
        });
        Ok((
            PiProcess {
                rpc,
                pid,
                exited,
                stderr,
            },
            rx,
        ))
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    pub fn stderr_tail(&self, max: usize) -> String {
        self.stderr.lock().unwrap().tail(max)
    }

    /// SIGTERM to the group, SIGKILL after `grace`.
    pub async fn kill(&self, grace: Duration) {
        let Some(pid) = self.pid else { return };
        if self.has_exited() {
            return;
        }
        signal_group(pid, libc::SIGTERM);
        let deadline = tokio::time::Instant::now() + grace;
        while !self.has_exited() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !self.has_exited() {
            signal_group(pid, libc::SIGKILL);
        }
    }

    /// Closes stdin (Pi's orderly shutdown) and kills the group after 5 s.
    pub async fn shutdown(&self) {
        self.rpc.close().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.has_exited() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !self.has_exited()
            && let Some(pid) = self.pid
        {
            signal_group(pid, libc::SIGKILL);
        }
    }
}

impl Drop for PiProcess {
    fn drop(&mut self) {
        if !self.has_exited()
            && let Some(pid) = self.pid
        {
            signal_group(pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dirs: &PiDirs) -> Spawn {
        Spawn {
            install: Install {
                node: "/opt/node".into(),
                root: "/opt/pi".into(),
                version: "1.5.0".into(),
            },
            dirs: dirs.clone(),
            cwd: "/work".into(),
            session_id: "session-1".into(),
            name: Some("Fix tests".into()),
            extensions: vec!["/h/pi/ext/aether-bridge-abc.ts".into()],
            bridge: None,
            sandbox: SandboxMode::WorkspaceWrite,
            runner: Runner::Unconfined,
        }
    }

    #[test]
    fn pi_starts_without_project_resources() {
        let dirs = PiDirs::new("/h/pi");
        let args = argv(&spec(&dirs));
        for flag in ["--no-extensions", "--no-skills", "--no-mcp", "--no-approve"] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--session-id" && w[1] == "session-1")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1].ends_with("aether-bridge-abc.ts"))
        );
    }

    #[test]
    fn the_fence_covers_the_whole_process() {
        let dirs = PiDirs::new("/h/pi");
        let inner = vec!["node".to_owned(), "cli.js".to_owned()];
        let full = confine(
            &Runner::Seatbelt,
            inner.clone(),
            SandboxMode::ReadOnly,
            Path::new("/work"),
            &dirs,
        );
        assert_eq!(full[0], "/usr/bin/sandbox-exec");
        assert!(full[2].contains("(subpath \"/h/pi/sessions\")"));
        assert!(!full[2].contains("/work"));
        assert_eq!(&full[full.len() - 2..], &inner[..]);
        let open = confine(
            &Runner::Seatbelt,
            inner.clone(),
            SandboxMode::DangerFullAccess,
            Path::new("/work"),
            &dirs,
        );
        assert_eq!(open, inner);
        assert!(!allowed_env("DEEPSEEK_API_KEY") && allowed_env("LC_ALL"));
    }
}
