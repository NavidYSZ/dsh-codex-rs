//! MCP client of dsh-codex, a port of dsh `mcp-client` and `mcp-resources`.
//!
//! Every `[mcp_servers.<name>]` table of the config becomes one supervised
//! connection (stdio child process or Streamable HTTP). The server's tools
//! are offered as `mcp__<name>__<tool>`, plus the shared resource tools
//! `list_mcp_resources`, `list_mcp_resource_templates` and
//! `read_mcp_resource`. Server instructions and the resource-server list
//! become system prompt sections (dsh order `MCP_SERVERS` = 3100).
//!
//! ```no_run
//! # async fn demo(config: &dshx_core::config::Config) -> anyhow::Result<()> {
//! let (tools, mcp) = dshx_mcp::mcp_tools(config).await?;
//! let mut registry = dshx_core::ToolRegistry::new();
//! for tool in tools {
//!     registry.register(tool);
//! }
//! // Later: rebuild the registry from `mcp.tools()` whenever
//! // `mcp.subscribe()` changes (tool-list changes, reconnects, give-ups),
//! // and add `mcp.prompt_sections()` to the system prompt.
//! mcp.shutdown().await;
//! # Ok(()) }
//! ```
use std::sync::Arc;

use dshx_core::Tool;
use dshx_core::config::Config;
use tokio::sync::watch;

mod config;
mod error;
mod http;
mod names;
mod peer;
mod protocol;
mod resources;
mod server;
mod stdio;
mod tools;

pub use error::McpError;
pub use names::{is_server_name, public_tool_name};
pub use resources::ResourceTool;
pub use tools::McpTool;

use server::Server;

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

/// Owner of all MCP connections. Dropping it does not stop the servers; call
/// [`McpHandle::shutdown`].
pub struct McpHandle {
    servers: Arc<Vec<Arc<Server>>>,
    resource_tools: Vec<Arc<dyn Tool>>,
    changes: Arc<watch::Sender<u64>>,
}

impl McpHandle {
    /// A handle without servers.
    pub fn empty() -> Self {
        McpHandle {
            servers: Arc::new(Vec::new()),
            resource_tools: Vec::new(),
            changes: Arc::new(watch::channel(0).0),
        }
    }

    /// The current tool set: resource tools (when any server is configured)
    /// and every server's current tools.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.resource_tools.clone();
        for server in self.servers.iter() {
            for entry in server.tools() {
                tools.push(Arc::new(McpTool::new(server.clone(), entry)));
            }
        }
        tools
    }

    /// Changes whenever a server's tools or instructions change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Names of the configured servers, sorted.
    pub fn servers(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.name().to_owned()).collect()
    }

    /// Whether `name` is a tool this handle bridges (resource tools included).
    pub fn owns_tool(&self, name: &str) -> bool {
        self.tools().iter().any(|t| t.schema().name == name)
    }

    /// The `mcp-resource-servers` section and one `mcp:<server>` section per
    /// server, in dsh's assembly order (order, then name bytes).
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        let mut sections = Vec::new();
        if !self.servers.is_empty() {
            sections.push(PromptSection {
                name: "mcp-resource-servers".into(),
                order: MCP_SECTION_ORDER,
                text: resources::servers_section(&self.servers()),
            });
        }
        for server in self.servers.iter() {
            sections.push(PromptSection {
                name: format!("mcp:{}", server.name()),
                order: MCP_SECTION_ORDER,
                text: server.instructions(),
            });
        }
        sections.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        sections
    }

    /// Stops reconnecting and closes every connection; stdio servers get
    /// their stdin closed, then their process group is terminated.
    pub async fn shutdown(&self) {
        futures::future::join_all(self.servers.iter().map(|s| s.dispose())).await;
    }
}

/// Connects every enabled `[mcp_servers.*]` entry and returns the initial
/// tool set. A server that fails to connect is logged and keeps retrying in
/// the background (its tools appear once connected; watch
/// [`McpHandle::subscribe`]); with `fail_on_startup_error` the failure, or an
/// invalid entry, is returned as an error instead.
pub async fn mcp_tools(config: &Config) -> anyhow::Result<(Vec<Arc<dyn Tool>>, McpHandle)> {
    let changes = Arc::new(watch::channel(0).0);
    let mut servers = Vec::new();
    let mut startups = Vec::new();
    for (name, entry) in &config.mcp_servers {
        if !entry.enabled {
            continue;
        }
        let spec = match config::resolve(name, entry) {
            Ok(spec) => spec,
            Err(e) if entry.fail_on_startup_error => {
                shutdown_all(&servers).await;
                anyhow::bail!(e);
            }
            Err(e) => {
                tracing::error!("{e}; server skipped");
                continue;
            }
        };
        let strict = spec.fail_on_startup_error;
        let (server, first) = Server::start(spec, changes.clone());
        startups.push((server.clone(), strict, first));
        servers.push(server);
    }
    let outcomes = futures::future::join_all(startups.into_iter().map(
        |(server, strict, first)| async move {
            let outcome = first
                .await
                .unwrap_or_else(|_| Err("connection supervisor stopped".into()));
            (server, strict, outcome)
        },
    ))
    .await;
    for (server, strict, outcome) in outcomes {
        if let Err(e) = outcome
            && strict
        {
            shutdown_all(&servers).await;
            anyhow::bail!(
                "{}: initial connection or tool synchronization failed: {e}",
                server.spec.label()
            );
        }
    }
    let servers = Arc::new(servers);
    let resource_tools = if servers.is_empty() {
        Vec::new()
    } else {
        resources::resource_tools(servers.clone())
    };
    let handle = McpHandle {
        servers,
        resource_tools,
        changes,
    };
    Ok((handle.tools(), handle))
}

async fn shutdown_all(servers: &[Arc<Server>]) {
    futures::future::join_all(servers.iter().map(|s| s.dispose())).await;
}
