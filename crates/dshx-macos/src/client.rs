//! Talks to "DSH Computer Use.app" over its Unix socket: one JSON line per
//! request, one per response, one connection per request (port of
//! `lib/cua-client.js`). Starts the helper on demand.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::js;

/// Default per-request timeout (`timeoutMs = 60_000`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const LAUNCH_ATTEMPTS: u32 = 40;
const LAUNCH_INTERVAL: Duration = Duration::from_millis(150);

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `~/Library/Application Support/dsh-codex/cua.sock`, where the helper listens.
pub fn default_socket_path() -> PathBuf {
    home().join("Library/Application Support/dsh-codex/cua.sock")
}

/// `~/Applications/DSH Computer Use.app`, where `helper/build.sh --install` puts the helper.
pub fn default_app_path() -> PathBuf {
    home().join("Applications/DSH Computer Use.app")
}

/// Per-request options of [`CuaClient::call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOptions {
    pub timeout: Duration,
    /// `false` skips starting the helper (for frames and polls).
    pub launch: bool,
}

impl Default for CallOptions {
    fn default() -> Self {
        CallOptions {
            timeout: DEFAULT_TIMEOUT,
            launch: true,
        }
    }
}

impl CallOptions {
    pub fn timeout_ms(ms: u64) -> Self {
        CallOptions {
            timeout: Duration::from_millis(ms),
            ..Default::default()
        }
    }
    pub fn no_launch(mut self) -> Self {
        self.launch = false;
        self
    }
}

/// Transport failures. Messages follow the JS client.
#[derive(Debug, thiserror::Error)]
pub enum CuaError {
    #[error("connect {code} {path}")]
    Connect {
        code: String,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("computer-use helper timed out on \"{0}\"")]
    Timeout(String),
    #[error("computer-use helper closed the connection without answering")]
    NoAnswer,
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("computer-use helper not installed at {0} (run helper/build.sh --install)")]
    NotInstalled(String),
    #[error("{0}")]
    Launch(String),
    #[error("computer-use helper did not start")]
    DidNotStart,
    #[error("the computer-use helper needs macOS")]
    Unsupported,
}

impl CuaError {
    fn connect(source: io::Error, path: &Path) -> Self {
        let code = match source.kind() {
            io::ErrorKind::NotFound => "ENOENT".to_owned(),
            io::ErrorKind::ConnectionRefused => "ECONNREFUSED".to_owned(),
            io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
            _ => source.to_string(),
        };
        CuaError::Connect {
            code,
            path: path.display().to_string(),
            source,
        }
    }

    /// ENOENT / ECONNREFUSED: the helper is not running (yet).
    pub fn helper_not_running(&self) -> bool {
        let kind = match self {
            CuaError::Connect { source, .. } | CuaError::Io(source) => source.kind(),
            _ => return false,
        };
        matches!(
            kind,
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    }
}

/// Client for the helper socket. Request ids count up per client, starting at 1.
#[derive(Debug)]
pub struct CuaClient {
    socket_path: PathBuf,
    app_path: PathBuf,
    open_program: PathBuf,
    next_id: AtomicU64,
}

impl Default for CuaClient {
    fn default() -> Self {
        CuaClient::new()
    }
}

impl CuaClient {
    /// The installed helper at its default socket.
    pub fn new() -> Self {
        CuaClient::with_socket_path(default_socket_path())
    }

    /// A helper (or a fake one in tests) listening on `path`.
    pub fn with_socket_path(path: impl Into<PathBuf>) -> Self {
        CuaClient {
            socket_path: path.into(),
            app_path: default_app_path(),
            open_program: PathBuf::from("/usr/bin/open"),
            next_id: AtomicU64::new(1),
        }
    }

    /// App bundle started when the socket is missing.
    pub fn with_app_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.app_path = path.into();
        self
    }

    /// Program run as `<program> -g <app>` to start the helper (default `/usr/bin/open`).
    pub fn with_open_program(mut self, path: impl Into<PathBuf>) -> Self {
        self.open_program = path.into();
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn app_path(&self) -> &Path {
        &self.app_path
    }

    /// Sends one request (`{"cmd": …, …}`) and returns the helper's answer.
    /// Starts the helper when it is not running, unless `options.launch` is false.
    pub async fn call(
        &self,
        request: &Map<String, Value>,
        options: CallOptions,
    ) -> Result<Value, CuaError> {
        match self.send(request, options.timeout).await {
            Ok(answer) => return Ok(answer),
            Err(err) if !err.helper_not_running() || !options.launch => return Err(err),
            Err(_) => {}
        }
        self.launch().await?;
        for _ in 0..LAUNCH_ATTEMPTS {
            tokio::time::sleep(LAUNCH_INTERVAL).await;
            match self.send(request, options.timeout).await {
                Ok(answer) => return Ok(answer),
                Err(err) if err.helper_not_running() => {}
                Err(err) => return Err(err),
            }
        }
        Err(CuaError::DidNotStart)
    }

    #[cfg(unix)]
    async fn send(
        &self,
        request: &Map<String, Value>,
        timeout: Duration,
    ) -> Result<Value, CuaError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let exchange = async {
            let mut stream = tokio::net::UnixStream::connect(&self.socket_path)
                .await
                .map_err(|e| CuaError::connect(e, &self.socket_path))?;
            // `{ id: nextId++, ...request }`: the id is taken once connected.
            let mut line = Map::new();
            line.insert(
                "id".into(),
                Value::from(self.next_id.fetch_add(1, Ordering::Relaxed)),
            );
            for (k, v) in request {
                line.insert(k.clone(), v.clone());
            }
            let mut bytes = serde_json::to_vec(&Value::Object(line))?;
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            // The helper answers with one line and closes.
            let mut answer = Vec::new();
            stream.read_to_end(&mut answer).await?;
            Ok::<_, CuaError>(answer)
        };
        let answer = tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| CuaError::Timeout(js::text(request.get("cmd"))))??;
        let text = String::from_utf8_lossy(&answer);
        let text = text.trim();
        if text.is_empty() {
            return Err(CuaError::NoAnswer);
        }
        Ok(serde_json::from_str(text)?)
    }

    #[cfg(not(unix))]
    async fn send(
        &self,
        _request: &Map<String, Value>,
        _timeout: Duration,
    ) -> Result<Value, CuaError> {
        Err(CuaError::Unsupported)
    }

    async fn launch(&self) -> Result<(), CuaError> {
        if !self.app_path.exists() {
            return Err(CuaError::NotInstalled(self.app_path.display().to_string()));
        }
        tracing::debug!(app = %self.app_path.display(), "starting the computer-use helper");
        // -g: do not bring it to the front; it is a menu-less accessory app.
        let program = self.open_program.display().to_string();
        let out = tokio::process::Command::new(&self.open_program)
            .arg("-g")
            .arg(&self.app_path)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| CuaError::Launch(format!("spawn {program}: {e}")))?;
        if !out.status.success() {
            return Err(CuaError::Launch(format!(
                "Command failed: {program} -g {}\n{}",
                self.app_path.display(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }
}
