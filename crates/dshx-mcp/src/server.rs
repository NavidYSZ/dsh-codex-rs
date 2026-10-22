//! Connection supervisor for one server (dsh `mcp-client/connection.ts`):
//! connect + initialize + tool discovery, tool-list re-syncs, and reconnects
//! with exponential backoff after a lost connection. One outage shares one
//! attempt budget; a connection that stays up for `max_delay` resets it.
//! Exhausting the budget removes the server's tools and instructions.
//! A server that answers with an authorization challenge is not retried: it
//! waits (without tools) until it is restarted after a login.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::config::{AuthSpec, ServerSpec, TransportSpec};
use crate::error::McpError;
use crate::http::HttpTransport;
use crate::log::ServerLog;
use crate::names::public_tool_name;
use crate::oauth::{AuthChallenge, EnvBearer, OAuthManager, TokenProvider};
use crate::peer::{Peer, PeerEvent, Transport};
use crate::protocol::{self, RawTool};
use crate::stdio::StdioTransport;

/// Maximum pages of an aggregated list walk (SDK `listMaxPages`).
pub(crate) const LIST_MAX_PAGES: usize = 64;

/// A bridged tool of the current generation.
#[derive(Debug, Clone)]
pub(crate) struct ToolEntry {
    pub public_name: String,
    pub raw: RawTool,
}

/// Where the supervisor stands.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Phase {
    #[default]
    Connecting,
    Connected,
    /// Waiting for the next attempt after a failure or a lost connection.
    Retrying {
        error: String,
    },
    /// The server wants a login; no further attempts until a restart.
    NeedsLogin,
    /// Gave up, or reconnecting is off.
    Failed {
        error: String,
    },
}

/// Why a connection attempt failed.
pub(crate) enum ConnectError {
    Auth(AuthChallenge),
    Other(String),
}

impl From<String> for ConnectError {
    fn from(e: String) -> Self {
        ConnectError::Other(e)
    }
}

impl From<McpError> for ConnectError {
    fn from(e: McpError) -> Self {
        match e {
            McpError::Unauthorized(challenge) => ConnectError::Auth(*challenge),
            other => ConnectError::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Auth(_) => f.write_str("authorization required"),
            ConnectError::Other(e) => f.write_str(e),
        }
    }
}

#[derive(Default)]
struct State {
    phase: Phase,
    /// The live generation; `None` while connecting, down or given up.
    peer: Option<Arc<Peer>>,
    capabilities: Value,
    tools: Vec<ToolEntry>,
    /// Attributed instructions (`### MCP server: <name>\n\n...`) or empty.
    instructions: String,
}

pub(crate) struct Server {
    pub spec: ServerSpec,
    state: Mutex<State>,
    /// Bumped on every change of tools or instructions (shared by all servers).
    changes: Arc<watch::Sender<u64>>,
    disposed: CancellationToken,
    stopped: Mutex<Option<oneshot::Receiver<()>>>,
    /// Bearer tokens of an HTTP server.
    auth: Option<Arc<dyn TokenProvider>>,
    /// The server's last authorization challenge.
    challenge: Arc<Mutex<Option<AuthChallenge>>>,
    /// stderr, failed requests and connection events (kept across restarts).
    pub log: Arc<ServerLog>,
}

/// One established connection before it is published.
struct Connection {
    peer: Arc<Peer>,
    events: mpsc::UnboundedReceiver<PeerEvent>,
    capabilities: Value,
    tools: Vec<ToolEntry>,
    instructions: String,
}

impl Server {
    /// Starts the supervisor; the receiver resolves after the first attempt.
    pub fn start(
        spec: ServerSpec,
        changes: Arc<watch::Sender<u64>>,
        oauth: Option<&Arc<OAuthManager>>,
        log: Arc<ServerLog>,
    ) -> (Arc<Server>, oneshot::Receiver<Result<(), String>>) {
        let (first_tx, first_rx) = oneshot::channel();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let auth: Option<Arc<dyn TokenProvider>> = match &spec.transport {
            TransportSpec::Http {
                auth: AuthSpec::OAuth(_),
                url,
                ..
            } => oauth.map(|m| m.provider(url)),
            TransportSpec::Http {
                auth: AuthSpec::BearerEnv(var),
                ..
            } => Some(Arc::new(EnvBearer(var.clone()))),
            _ => None,
        };
        let server = Arc::new(Server {
            spec,
            state: Mutex::new(State::default()),
            changes,
            disposed: CancellationToken::new(),
            stopped: Mutex::new(Some(stopped_rx)),
            auth,
            challenge: Arc::new(Mutex::new(None)),
            log,
        });
        let supervisor = server.clone();
        tokio::spawn(async move {
            supervisor.run(first_tx).await;
            let _ = stopped_tx.send(());
        });
        (server, first_rx)
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    pub fn peer(&self) -> Option<Arc<Peer>> {
        self.state
            .lock()
            .unwrap()
            .peer
            .clone()
            .filter(|p| !p.is_closed())
    }

    pub fn tools(&self) -> Vec<ToolEntry> {
        self.state.lock().unwrap().tools.clone()
    }

    pub fn instructions(&self) -> String {
        self.state.lock().unwrap().instructions.clone()
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.state.lock().unwrap().capabilities.get(name).is_some()
    }

    pub fn phase(&self) -> Phase {
        self.state.lock().unwrap().phase.clone()
    }

    /// The last 401/403 challenge (a 403 `insufficient_scope` also on a
    /// live connection).
    pub fn challenge(&self) -> Option<AuthChallenge> {
        self.challenge.lock().unwrap().clone()
    }

    fn set_phase(&self, phase: Phase) {
        self.state.lock().unwrap().phase = phase;
    }

    fn notify(&self) {
        self.changes.send_modify(|v| *v += 1);
    }

    /// Stops reconnecting, closes the live connection (killing a stdio
    /// server's process group) and waits until the supervisor finished.
    pub async fn dispose(&self) {
        self.disposed.cancel();
        let stopped = self.stopped.lock().unwrap().take();
        if let Some(stopped) = stopped {
            let _ = stopped.await;
        }
    }

    async fn run(self: &Arc<Self>, first: oneshot::Sender<Result<(), String>>) {
        let label = self.spec.label();
        let policy = self.spec.reconnect.clone();
        let mut first = Some(first);
        let mut failed_attempts: u32 = 0;
        loop {
            let attempt = tokio::select! {
                biased;
                _ = self.disposed.cancelled() => return,
                attempt = self.connect() => attempt,
            };
            let mut connected_at = None;
            match attempt {
                Ok(connection) => {
                    let Connection {
                        peer,
                        mut events,
                        capabilities,
                        tools,
                        instructions,
                    } = connection;
                    {
                        let mut state = self.state.lock().unwrap();
                        state.phase = Phase::Connected;
                        state.peer = Some(peer.clone());
                        state.capabilities = capabilities;
                        state.tools = tools;
                        state.instructions = instructions;
                    }
                    *self.challenge.lock().unwrap() = None;
                    self.notify();
                    self.log
                        .info(format!("connected, {} tools", self.tools().len()));
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Ok(()));
                    }
                    if failed_attempts > 0 {
                        tracing::info!(
                            "{label}: reconnected and re-synced tools (attempt {failed_attempts}/{})",
                            policy.max_attempts
                        );
                    }
                    let since = Instant::now();
                    connected_at = Some(since);
                    let lost = loop {
                        let event = tokio::select! {
                            biased;
                            _ = self.disposed.cancelled() => None,
                            event = events.recv() => Some(event),
                        };
                        match event {
                            None => {
                                self.state.lock().unwrap().peer = None;
                                Self::close_peer(&peer, &label).await;
                                return;
                            }
                            Some(Some(PeerEvent::ToolsChanged)) => {
                                tracing::info!("{label}: tool list changed, re-syncing");
                                self.log.info("tool list changed");
                                let synced = tokio::select! {
                                    biased;
                                    _ = self.disposed.cancelled() => continue,
                                    r = self.list_tools(&peer) => r,
                                };
                                match synced {
                                    Ok(tools) => {
                                        self.state.lock().unwrap().tools = tools;
                                        self.notify();
                                    }
                                    Err(e) => {
                                        tracing::error!("{label}: tool re-sync failed: {e}");
                                        self.log.error(format!("tool re-sync failed: {e}"));
                                    }
                                }
                                // A 403 insufficient_scope changes the reported status.
                                self.notify();
                            }
                            Some(Some(PeerEvent::Closed(reason))) => break reason,
                            Some(None) => break "connection ended".to_owned(),
                        }
                    };
                    tracing::warn!("{label}: connection lost: {lost}");
                    self.log.error(format!("connection lost: {lost}"));
                    {
                        let mut state = self.state.lock().unwrap();
                        state.peer = None;
                        state.phase = Phase::Retrying {
                            error: format!("connection lost: {lost}"),
                        };
                    }
                    // Reap what is left of the old generation before a new one starts.
                    Self::close_peer(&peer, &label).await;
                    if since.elapsed() >= policy.max_delay {
                        failed_attempts = 0;
                    }
                }
                Err(ConnectError::Auth(challenge)) => {
                    tracing::info!("{label}: login required; waiting for a restart after login");
                    self.log.info("sign-in required");
                    *self.challenge.lock().unwrap() = Some(challenge);
                    {
                        let mut state = self.state.lock().unwrap();
                        state.phase = Phase::NeedsLogin;
                        state.tools.clear();
                        state.instructions.clear();
                    }
                    self.notify();
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Err("authorization required".into()));
                    }
                    self.disposed.cancelled().await;
                    return;
                }
                Err(ConnectError::Other(e)) => {
                    tracing::warn!("{label}: connection attempt failed: {e}");
                    self.log.error(format!("connection failed: {e}"));
                    self.set_phase(Phase::Retrying { error: e.clone() });
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Err(e));
                    }
                }
            }
            if self.disposed.is_cancelled() {
                return;
            }
            if !policy.enabled {
                let message = if connected_at.is_some() {
                    "connection lost and reconnect is disabled — registered tools will fail until dsh-codex restarts"
                } else {
                    "connection failed and reconnect is disabled — no tools were registered; restart dsh-codex to connect"
                };
                tracing::error!("{label}: {message}");
                self.log.error(message);
                let error = match self.phase() {
                    Phase::Retrying { error } => error,
                    _ => message.to_owned(),
                };
                self.set_phase(Phase::Failed { error });
                return;
            }
            failed_attempts += 1;
            if failed_attempts > policy.max_attempts {
                {
                    let mut state = self.state.lock().unwrap();
                    let error = match &state.phase {
                        Phase::Retrying { error } => error.clone(),
                        _ => "connection failed".to_owned(),
                    };
                    state.phase = Phase::Failed {
                        error: format!("gave up after {} attempts: {error}", policy.max_attempts),
                    };
                    state.tools.clear();
                    state.instructions.clear();
                }
                self.notify();
                tracing::error!(
                    "{label}: giving up after {} consecutive failed reconnect attempts — tools unregistered; restart dsh-codex to reconnect",
                    policy.max_attempts
                );
                self.log
                    .error(format!("gave up after {} attempts", policy.max_attempts));
                return;
            }
            let delay = policy.delay(failed_attempts);
            let action = if connected_at.is_some() {
                "connection lost; reconnecting"
            } else {
                "connection failed; retrying"
            };
            tracing::warn!(
                "{label}: {action} in {}ms (attempt {failed_attempts}/{})",
                delay.as_millis(),
                policy.max_attempts
            );
            self.log.info(format!(
                "next attempt in {} ms ({failed_attempts}/{})",
                delay.as_millis(),
                policy.max_attempts
            ));
            tokio::select! {
                _ = self.disposed.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    async fn close_peer(peer: &Arc<Peer>, label: &str) {
        if !peer.transport().close().await {
            tracing::error!(
                "{label}: transport closure could not be confirmed — server shutdown may be incomplete"
            );
        }
    }

    /// One attempt: transport, `initialize`, `notifications/initialized`,
    /// instructions, tool discovery. A failed attempt closes its transport.
    async fn connect(&self) -> Result<Connection, ConnectError> {
        let label = self.spec.label();
        let (transport, inbox, http): (Arc<dyn Transport>, _, Option<Arc<HttpTransport>>) =
            match &self.spec.transport {
                TransportSpec::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => {
                    self.log.info(format!(
                        "starting {}",
                        [&[command.clone()], &args[..]].concat().join(" ")
                    ));
                    let (t, inbox) = StdioTransport::spawn(
                        command,
                        args,
                        env,
                        cwd.as_ref(),
                        &label,
                        self.log.clone(),
                    )
                    .map_err(|e| e.to_string())?;
                    (t, inbox, None)
                }
                TransportSpec::Http { url, headers, .. } => {
                    self.log.info(format!("connecting to {url}"));
                    let (t, inbox) = HttpTransport::new(
                        url.clone(),
                        headers,
                        &label,
                        self.auth.clone(),
                        self.challenge.clone(),
                        self.log.clone(),
                    )
                    .map_err(|e| e.to_string())?;
                    (t.clone(), inbox, Some(t))
                }
            };
        let (peer, events) = Peer::start(transport, inbox, label.clone());
        match self.handshake(&peer).await {
            Ok((capabilities, instructions)) => {
                if let Some(http) = http {
                    http.start_listening();
                }
                let tools = if capabilities.get("tools").is_some() {
                    match self.list_tools(&peer).await {
                        Ok(tools) => tools,
                        Err(e) => {
                            Self::close_peer(&peer, &label).await;
                            return Err(e);
                        }
                    }
                } else {
                    Vec::new()
                };
                Ok(Connection {
                    peer,
                    events,
                    capabilities,
                    tools,
                    instructions,
                })
            }
            Err(e) => {
                Self::close_peer(&peer, &label).await;
                Err(e)
            }
        }
    }

    async fn handshake(&self, peer: &Arc<Peer>) -> Result<(Value, String), ConnectError> {
        let label = self.spec.label();
        let params = json!({
            "protocolVersion": protocol::PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "dsh-codex", "version": env!("CARGO_PKG_VERSION")},
        });
        let result = peer
            .request("initialize", Some(params), INIT_TIMEOUT, None)
            .await?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !protocol::SUPPORTED_VERSIONS.contains(&version.as_str()) {
            return Err(format!("Server's protocol version is not supported: {version}").into());
        }
        peer.transport().initialized(&version);
        peer.notify("notifications/initialized", None).await;
        let capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let text = result
            .get("instructions")
            .and_then(Value::as_str)
            .map(|t| t.trim_end_matches(is_js_space))
            .unwrap_or_default();
        let instructions = if text.is_empty() {
            String::new()
        } else {
            format!("### MCP server: {}\n\n{text}", self.spec.name)
        };
        if instructions.len() > self.spec.max_instruction_bytes {
            return Err(format!(
                "{label}: server instructions exceed maxInstructionBytes ({})",
                self.spec.max_instruction_bytes
            )
            .into());
        }
        Ok((capabilities, instructions))
    }

    /// The complete tool list (all pages) under public names.
    async fn list_tools(&self, peer: &Arc<Peer>) -> Result<Vec<ToolEntry>, ConnectError> {
        let pages = list_all(peer, "tools/list", INIT_TIMEOUT, None).await?;
        let mut entries: Vec<ToolEntry> = Vec::new();
        for page in pages {
            let (tools, _) = protocol::tools_page(&page)
                .map_err(|e| ConnectError::Other(format!("Invalid result for tools/list: {e}")))?;
            for raw in tools {
                let public_name = public_tool_name(&self.spec.name, &raw.name);
                if entries.iter().any(|e| e.public_name == public_name) {
                    return Err(format!(
                        "{}: server listed tool \"{}\" more than once — invalid tool list",
                        self.spec.label(),
                        raw.name
                    )
                    .into());
                }
                entries.push(ToolEntry { public_name, raw });
            }
        }
        Ok(entries)
    }
}

/// Timeout of handshake and discovery requests (the SDK's request default).
const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Walks every page of a paginated list method from the first page; a
/// repeated cursor ends the walk, more than [`LIST_MAX_PAGES`] pages fail.
pub(crate) async fn list_all(
    peer: &Peer,
    method: &str,
    timeout: std::time::Duration,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<Value>, McpError> {
    let mut pages = vec![peer.request(method, None, timeout, cancel).await?];
    let mut seen = std::collections::HashSet::new();
    while let Some(cursor) = pages
        .last()
        .and_then(|p| p.get("nextCursor"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        if !seen.insert(cursor.clone()) {
            break;
        }
        if pages.len() >= LIST_MAX_PAGES {
            return Err(McpError::Protocol(format!(
                "{method}: exceeded listMaxPages ({LIST_MAX_PAGES}); server pagination did not terminate"
            )));
        }
        pages.push(
            peer.request(method, Some(json!({"cursor": cursor})), timeout, cancel)
                .await?,
        );
    }
    Ok(pages)
}

pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
