//! MCP client of dsh-codex, a port of dsh `mcp-client` and `mcp-resources`.
//!
//! Every `[mcp_servers.<name>]` table of the config becomes one supervised
//! connection (stdio child process or Streamable HTTP). The server's tools
//! are offered as `mcp__<name>__<tool>`, plus the shared resource tools
//! `list_mcp_resources`, `list_mcp_resource_templates` and
//! `read_mcp_resource`. Server instructions and the resource-server list
//! become system prompt sections (dsh order `MCP_SERVERS` = 3100).
//!
//! The server set can change at runtime: [`McpHandle::apply`] starts new
//! and changed servers and stops removed ones, leaving the rest connected.
//! HTTP servers log in through [`OAuthManager`] (see [`oauth`]); the
//! built-in [`catalog`] lists well-known remote servers. Each server keeps
//! a short log ([`McpHandle::log`]), and [`probe`] tries a configuration
//! once without keeping the connection.
//!
//! ```no_run
//! # async fn demo(config: &dshx_core::config::Config) -> anyhow::Result<()> {
//! let (tools, mcp) = dshx_mcp::mcp_tools(config).await?;
//! let mut registry = dshx_core::ToolRegistry::new();
//! for tool in tools {
//!     registry.register(tool);
//! }
//! // Later: rebuild the registry from `mcp.tools()` whenever
//! // `mcp.subscribe()` changes (tool-list changes, reconnects, give-ups,
//! // `apply`), and add `mcp.prompt_sections()` to the system prompt.
//! mcp.shutdown().await;
//! # Ok(()) }
//! ```
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use dshx_core::Tool;
use dshx_core::config::{Config, McpServerConfig};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{oneshot, watch};

pub mod catalog;
mod config;
mod error;
mod http;
mod log;
mod names;
pub mod oauth;
mod peer;
mod protocol;
mod resources;
mod server;
mod stdio;
mod tools;

pub use error::McpError;
pub use log::{LOG_LINES, LogLine};
pub use names::{is_server_name, public_tool_name};
pub use oauth::{AuthChallenge, CallbackParams, LoginRequest, LoginStatus, OAuthManager};
pub use resources::ResourceTool;
pub use tools::McpTool;

use log::ServerLog;
use server::{Phase, Server};

/// dsh `getSectionOrder('MCP_SERVERS')`.
pub const MCP_SECTION_ORDER: i64 = 3100;

/// A system prompt section contributed by MCP. The text is literal: it must
/// not go through `{{variable}}` interpolation (dsh `interpolate: false`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSection {
    /// `mcp-resource-servers` or `mcp:<server>`.
    pub name: String,
    pub order: i64,
    /// Empty when the section currently contributes nothing.
    pub text: String,
}

/// One bridged tool, for status listings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    /// Model-facing name (`mcp__<server>__<tool>`).
    pub name: String,
    /// The server's own name.
    pub raw_name: String,
    pub description: String,
}

/// One server tool with its owner, for per-session tool sets.
#[derive(Clone)]
pub struct ServerTool {
    pub server: String,
    /// The server's own name for the tool (what `disabled_tools` lists).
    pub raw_name: String,
    /// Offered as `mcp__<server>__<tool>`.
    pub tool: Arc<dyn Tool>,
}

/// Names of the shared resource tools.
pub const RESOURCE_TOOLS: [&str; 3] = [
    "list_mcp_resource_templates",
    "list_mcp_resources",
    "read_mcp_resource",
];

/// Whether `name` is a tool an [`McpHandle`] bridges: a server tool
/// (`mcp__…`) or a shared resource tool.
pub fn is_bridged_tool(name: &str) -> bool {
    name.starts_with("mcp__") || RESOURCE_TOOLS.contains(&name)
}

/// State of one configured server.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    /// `connecting`, `connected`, `needs-login` or `error`.
    pub status: &'static str,
    /// Failure text (`error`) or why a login is needed (`needs-login`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `error` only: a reconnect attempt is still pending.
    pub retrying: bool,
    /// `stdio` or `streamable-http`; empty for an invalid entry.
    pub transport: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The last 401/403 challenge (scope hints for the login).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<AuthChallenge>,
    pub tools: Vec<ToolInfo>,
}

/// Owner of all MCP connections. Dropping it does not stop the servers; call
/// [`McpHandle::shutdown`].
pub struct McpHandle {
    servers: Arc<RwLock<Vec<Arc<Server>>>>,
    resource_tools: Vec<Arc<dyn Tool>>,
    changes: Arc<watch::Sender<u64>>,
    oauth: Option<Arc<OAuthManager>>,
    /// Enabled entries that failed validation, with their error.
    invalid: RwLock<BTreeMap<String, String>>,
    /// Serializes `apply` and `restart`.
    reconcile: tokio::sync::Mutex<()>,
    /// Logs by server name; they outlive restarts and config changes.
    logs: Mutex<BTreeMap<String, Arc<ServerLog>>>,
}

impl McpHandle {
    /// A handle without servers.
    pub fn empty() -> Self {
        McpHandle::new(None)
    }

    /// A handle without servers whose HTTP servers log in through `oauth`.
    pub fn new(oauth: Option<Arc<OAuthManager>>) -> Self {
        let servers = Arc::new(RwLock::new(Vec::new()));
        McpHandle {
            resource_tools: resources::resource_tools(servers.clone()),
            servers,
            changes: Arc::new(watch::channel(0).0),
            oauth,
            invalid: RwLock::new(BTreeMap::new()),
            reconcile: tokio::sync::Mutex::new(()),
            logs: Mutex::new(BTreeMap::new()),
        }
    }

    fn log_of(&self, name: &str) -> Arc<ServerLog> {
        self.logs
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default()
            .clone()
    }

    /// The last [`LOG_LINES`] lines of a server, oldest first: its stderr,
    /// failed HTTP requests and connection events.
    pub fn log(&self, name: &str) -> Vec<LogLine> {
        self.logs
            .lock()
            .unwrap()
            .get(name)
            .map(|log| log.lines())
            .unwrap_or_default()
    }

    /// The input schemas of a server's current tools, by the server's own
    /// tool names.
    pub fn tool_schemas(&self, name: &str) -> Vec<(String, Value)> {
        self.current()
            .iter()
            .find(|s| s.name() == name)
            .map(|s| {
                s.tools()
                    .into_iter()
                    .map(|t| (t.raw.name, t.raw.input_schema))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn oauth(&self) -> Option<&Arc<OAuthManager>> {
        self.oauth.as_ref()
    }

    fn current(&self) -> Vec<Arc<Server>> {
        self.servers.read().unwrap().clone()
    }

    fn notify(&self) {
        self.changes.send_modify(|v| *v += 1);
    }

    /// The current tool set: resource tools (when any server is configured)
    /// and every server's current tools.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let servers = self.current();
        let mut tools = if servers.is_empty() {
            Vec::new()
        } else {
            self.resource_tools.clone()
        };
        for server in &servers {
            for entry in server.tools() {
                tools.push(Arc::new(McpTool::new(server.clone(), entry)));
            }
        }
        tools
    }

    /// Every server's current tools with owner and raw name, in server
    /// order (a per-session selection of [`McpHandle::tools`]).
    pub fn server_tools(&self) -> Vec<ServerTool> {
        let mut out = Vec::new();
        for server in &self.current() {
            for entry in server.tools() {
                out.push(ServerTool {
                    server: server.name().to_owned(),
                    raw_name: entry.raw.name.clone(),
                    tool: Arc::new(McpTool::new(server.clone(), entry)),
                });
            }
        }
        out
    }

    /// The shared resource tools (they reach every configured server).
    pub fn resource_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.resource_tools.clone()
    }

    /// A server's attributed instructions (`### MCP server: <name>\n\n…`),
    /// empty when it has none or is not running.
    pub fn instructions(&self, name: &str) -> String {
        self.current()
            .iter()
            .find(|s| s.name() == name)
            .map(|s| s.instructions())
            .unwrap_or_default()
    }

    /// Whether a running server announced the `resources` capability.
    pub fn has_resources(&self, name: &str) -> bool {
        self.current()
            .iter()
            .any(|s| s.name() == name && s.has_capability("resources"))
    }

    /// Changes whenever a server's tools or instructions change, and on
    /// every [`McpHandle::apply`] or [`McpHandle::restart`].
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Names of the configured servers, sorted.
    pub fn servers(&self) -> Vec<String> {
        self.current().iter().map(|s| s.name().to_owned()).collect()
    }

    /// Whether `name` is a tool this handle bridges (resource tools included).
    pub fn owns_tool(&self, name: &str) -> bool {
        self.tools().iter().any(|t| t.schema().name == name)
    }

    /// The `mcp-resource-servers` section and one `mcp:<server>` section per
    /// server, in dsh's assembly order (order, then name bytes).
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        self.prompt_sections_where(|_| true)
    }

    /// [`McpHandle::prompt_sections`] of the servers `keep` accepts (the
    /// resource section names only those).
    pub fn prompt_sections_where(&self, keep: impl Fn(&str) -> bool) -> Vec<PromptSection> {
        let servers: Vec<Arc<Server>> = self
            .current()
            .into_iter()
            .filter(|s| keep(s.name()))
            .collect();
        let mut sections = Vec::new();
        if !servers.is_empty() {
            let names: Vec<String> = servers.iter().map(|s| s.name().to_owned()).collect();
            sections.push(PromptSection {
                name: "mcp-resource-servers".into(),
                order: MCP_SECTION_ORDER,
                text: resources::servers_section(&names),
            });
        }
        for server in &servers {
            sections.push(PromptSection {
                name: format!("mcp:{}", server.name()),
                order: MCP_SECTION_ORDER,
                text: server.instructions(),
            });
        }
        sections.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        sections
    }

    /// Status of every enabled server (and of entries that failed validation).
    pub fn status(&self) -> Vec<ServerInfo> {
        let mut out: Vec<ServerInfo> = self
            .current()
            .iter()
            .map(|server| {
                let challenge = server.challenge();
                let (status, error, retrying) = match server.phase() {
                    Phase::Connecting => ("connecting", None, false),
                    Phase::Connected => match &challenge {
                        Some(c) if c.insufficient_scope() => (
                            "needs-login",
                            Some(McpError::Unauthorized(Box::new(c.clone())).to_string()),
                            false,
                        ),
                        _ => ("connected", None, false),
                    },
                    Phase::Retrying { error } => ("error", Some(error), true),
                    Phase::NeedsLogin => (
                        "needs-login",
                        challenge
                            .as_ref()
                            .and_then(|c| c.error_description.clone().or(c.error.clone())),
                        false,
                    ),
                    Phase::Failed { error } => ("error", Some(error), false),
                };
                ServerInfo {
                    name: server.name().to_owned(),
                    status,
                    error,
                    retrying,
                    transport: server.spec.transport_name(),
                    url: match &server.spec.transport {
                        config::TransportSpec::Http { url, .. } => Some(url.to_string()),
                        config::TransportSpec::Stdio { .. } => None,
                    },
                    challenge,
                    tools: server
                        .tools()
                        .into_iter()
                        .map(|t| ToolInfo {
                            name: t.public_name,
                            raw_name: t.raw.name,
                            description: t.raw.description,
                        })
                        .collect(),
                }
            })
            .collect();
        for (name, error) in self.invalid.read().unwrap().iter() {
            out.push(ServerInfo {
                name: name.clone(),
                status: "error",
                error: Some(error.clone()),
                retrying: false,
                transport: "",
                url: None,
                challenge: None,
                tools: Vec::new(),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The last authorization challenge of a server.
    pub fn challenge(&self, name: &str) -> Option<AuthChallenge> {
        self.current()
            .iter()
            .find(|s| s.name() == name)
            .and_then(|s| s.challenge())
    }

    /// Brings the running servers in line with `servers` (disabled entries
    /// count as absent): unchanged servers keep their connection, changed
    /// ones restart, removed ones stop. Waits up to `wait` (`None` = until
    /// done) for the first connection attempt of every started server.
    /// Fails when an entry with `fail_on_startup_error` is invalid or does
    /// not connect; the other servers are applied anyway.
    pub async fn apply(
        &self,
        servers: &BTreeMap<String, McpServerConfig>,
        wait: Option<Duration>,
    ) -> anyhow::Result<()> {
        let _guard = self.reconcile.lock().await;
        let mut specs = Vec::new();
        let mut invalid = BTreeMap::new();
        let mut strict_errors = Vec::new();
        for (name, entry) in servers {
            if !entry.enabled {
                continue;
            }
            match config::resolve(name, entry) {
                Ok(spec) => specs.push(spec),
                Err(e) => {
                    if entry.fail_on_startup_error {
                        strict_errors.push(e.clone());
                    }
                    tracing::error!("{e}; server skipped");
                    self.log_of(name).error(e.clone());
                    invalid.insert(name.clone(), e);
                }
            }
        }
        self.logs
            .lock()
            .unwrap()
            .retain(|name, _| servers.contains_key(name));
        let current = self.current();
        let (keep, stop): (Vec<_>, Vec<_>) = current
            .into_iter()
            .partition(|s| specs.iter().any(|spec| *spec == s.spec));
        let mut next = keep.clone();
        let mut startups = Vec::new();
        for spec in specs {
            if keep.iter().any(|s| s.name() == spec.name) {
                continue;
            }
            let strict = spec.fail_on_startup_error;
            let log = self.log_of(&spec.name);
            let (server, first) =
                Server::start(spec, self.changes.clone(), self.oauth.as_ref(), log);
            startups.push((server.clone(), strict, first));
            next.push(server);
        }
        next.sort_by(|a, b| a.name().cmp(b.name()));
        *self.servers.write().unwrap() = next;
        *self.invalid.write().unwrap() = invalid;
        self.notify();
        futures::future::join_all(stop.iter().map(|s| s.dispose())).await;
        for (server, strict, outcome) in first_attempts(startups, wait).await {
            if let Err(e) = outcome
                && strict
            {
                strict_errors.push(format!(
                    "{}: initial connection or tool synchronization failed: {e}",
                    server.spec.label()
                ));
            }
        }
        match strict_errors.first() {
            Some(e) => anyhow::bail!("{e}"),
            None => Ok(()),
        }
    }

    /// Reconnects one server from scratch (after a login, or after it gave
    /// up), waiting up to `wait` for the first attempt.
    pub async fn restart(&self, name: &str, wait: Option<Duration>) -> anyhow::Result<()> {
        let _guard = self.reconcile.lock().await;
        let Some(old) = self.current().into_iter().find(|s| s.name() == name) else {
            anyhow::bail!("no running MCP server named {name}");
        };
        old.dispose().await;
        old.log.info("restarted");
        let (server, first) = Server::start(
            old.spec.clone(),
            self.changes.clone(),
            self.oauth.as_ref(),
            old.log.clone(),
        );
        {
            let mut servers = self.servers.write().unwrap();
            for slot in servers.iter_mut() {
                if Arc::ptr_eq(slot, &old) {
                    *slot = server.clone();
                }
            }
        }
        self.notify();
        first_attempts(vec![(server, false, first)], wait).await;
        Ok(())
    }

    /// Stops reconnecting and closes every connection; stdio servers get
    /// their stdin closed, then their process group is terminated.
    pub async fn shutdown(&self) {
        let servers = self.current();
        futures::future::join_all(servers.iter().map(|s| s.dispose())).await;
    }
}

/// Validates one `[mcp_servers.<name>]` entry the way a start does.
pub fn check_server(name: &str, config: &McpServerConfig) -> Result<(), String> {
    config::resolve(name, config).map(|_| ())
}

/// Whether logins through [`OAuthManager`] apply to an entry: Streamable
/// HTTP without an `Authorization` header or `bearer_token_env_var`, and
/// OAuth not switched off.
pub fn uses_oauth(name: &str, config: &McpServerConfig) -> bool {
    matches!(
        config::resolve(name, config),
        Ok(config::ServerSpec {
            transport: config::TransportSpec::Http {
                auth: config::AuthSpec::OAuth(_),
                ..
            },
            ..
        })
    )
}

/// A tool a [`probe`] found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeTool {
    /// The server's own name.
    pub name: String,
    pub description: String,
}

/// Outcome of [`probe`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    /// Handshake and `tools/list` succeeded.
    pub ok: bool,
    pub tools: Vec<ProbeTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The server answered with an authorization challenge.
    pub needs_login: bool,
    /// What a stdio server wrote to stderr meanwhile (last lines).
    pub stderr: Vec<String>,
    pub elapsed_ms: u64,
}

/// stderr lines a probe reports.
const PROBE_STDERR_LINES: usize = 40;

/// Connects one server configuration once, as a start would (handshake and
/// `tools/list`), without retries and without keeping the connection: a
/// stdio server is stopped with its process group afterwards. `timeout`
/// bounds the whole attempt.
pub async fn probe(
    name: &str,
    config: &McpServerConfig,
    oauth: Option<&Arc<OAuthManager>>,
    timeout: Duration,
) -> Probe {
    let started = Instant::now();
    let failed = |error: String| Probe {
        ok: false,
        tools: Vec::new(),
        error: Some(error),
        needs_login: false,
        stderr: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    let mut spec = match config::resolve(name, config) {
        Ok(spec) => spec,
        Err(e) => return failed(e),
    };
    spec.reconnect.enabled = false;
    let log = Arc::new(ServerLog::default());
    let changes = Arc::new(watch::channel(0).0);
    let (server, first) = Server::start(spec, changes, oauth, log.clone());
    let outcome = match tokio::time::timeout(timeout, first).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Err("connection supervisor stopped".into()),
        Err(_) => Err(format!(
            "no answer within {} s",
            timeout.as_secs_f32().round()
        )),
    };
    let needs_login = server.phase() == Phase::NeedsLogin;
    let tools = server
        .tools()
        .into_iter()
        .map(|t| ProbeTool {
            name: t.raw.name,
            description: t.raw.description,
        })
        .collect();
    server.dispose().await;
    if outcome.is_err() {
        // A crashing server's last words arrive just after its exit.
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let mut stderr = log.stderr();
    stderr.drain(..stderr.len().saturating_sub(PROBE_STDERR_LINES));
    Probe {
        ok: outcome.is_ok(),
        tools,
        error: outcome.err(),
        needs_login,
        stderr,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

type Startup = (Arc<Server>, bool, oneshot::Receiver<Result<(), String>>);

/// Outcomes of the first connection attempts, as far as they finished
/// within `wait`.
async fn first_attempts(
    startups: Vec<Startup>,
    wait: Option<Duration>,
) -> Vec<(Arc<Server>, bool, Result<(), String>)> {
    let all = futures::future::join_all(startups.into_iter().map(
        |(server, strict, first)| async move {
            let outcome = first
                .await
                .unwrap_or_else(|_| Err("connection supervisor stopped".into()));
            (server, strict, outcome)
        },
    ));
    match wait {
        Some(limit) => tokio::time::timeout(limit, all).await.unwrap_or_default(),
        None => all.await,
    }
}

/// Connects every enabled `[mcp_servers.*]` entry and returns the initial
/// tool set. A server that fails to connect is logged and keeps retrying in
/// the background (its tools appear once connected; watch
/// [`McpHandle::subscribe`]); with `fail_on_startup_error` the failure, or an
/// invalid entry, is returned as an error instead.
pub async fn mcp_tools(config: &Config) -> anyhow::Result<(Vec<Arc<dyn Tool>>, McpHandle)> {
    let handle = McpHandle::empty();
    if let Err(e) = handle.apply(&config.mcp_servers, None).await {
        handle.shutdown().await;
        return Err(e);
    }
    Ok((handle.tools(), handle))
}
