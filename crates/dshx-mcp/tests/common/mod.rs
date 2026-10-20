//! Shared helpers: the Python fixture server (`tests/fixtures/mcp_server.py`)
//! over stdio or Streamable HTTP, configs and tool invocation.
#![allow(dead_code)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dshx_core::config::{Config, McpServerConfig};
use dshx_core::{Tool, ToolContext, ToolError, ToolResultPart};
use serde_json::Value;

pub fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_server.py")
}

pub fn stdio_server(env: &[(&str, &str)]) -> McpServerConfig {
    McpServerConfig {
        command: Some("python3".into()),
        args: vec![fixture().to_string_lossy().into_owned()],
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        ..Default::default()
    }
}

pub fn config(servers: Vec<(&str, McpServerConfig)>) -> Config {
    Config {
        mcp_servers: servers
            .into_iter()
            .map(|(name, server)| (name.to_owned(), server))
            .collect(),
        ..Default::default()
    }
}

/// Fast reconnects for tests.
pub fn quick_reconnect(mut server: McpServerConfig, max_attempts: u32) -> McpServerConfig {
    server.reconnect.initial_delay_ms = 20;
    server.reconnect.max_delay_ms = 100;
    server.reconnect.max_attempts = max_attempts;
    server
}

pub fn find(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.schema().name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
        .clone()
}

pub fn ctx() -> ToolContext {
    dshx_tools::testing::context(Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// Runs a tool and returns what the model would see (`Error: ` like the agent).
pub async fn call(
    tools: &[Arc<dyn Tool>],
    name: &str,
    args: Value,
    ctx: ToolContext,
) -> (bool, Vec<ToolResultPart>) {
    match find(tools, name).execute(args, ctx).await {
        Ok(out) => (out.is_error, out.content),
        Err(ToolError::Cancelled) => (true, vec![text("Error: tool call aborted")]),
        Err(e) => (true, vec![text(&format!("Error: {e}"))]),
    }
}

pub fn text(t: &str) -> ToolResultPart {
    ToolResultPart::Text { text: t.to_owned() }
}

pub fn only_text(parts: &[ToolResultPart]) -> String {
    match parts {
        [ToolResultPart::Text { text }] => text.clone(),
        other => panic!("expected one text part, got {other:?}"),
    }
}

/// Lifecycle events the fixture server wrote (`MCP_TEST_EVENTS`).
pub fn events(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A fixture server in HTTP mode; killed on drop.
pub struct HttpFixture {
    child: std::process::Child,
    pub url: String,
}

impl HttpFixture {
    pub async fn start(sse: bool, events: &Path) -> HttpFixture {
        let dir = events.parent().unwrap();
        let port_file = dir.join(format!("port-{}", std::process::id()));
        let _ = std::fs::remove_file(&port_file);
        let mut cmd = std::process::Command::new("python3");
        cmd.arg(fixture())
            .arg("--http")
            .arg(&port_file)
            .env("MCP_TEST_EVENTS", events)
            .env("MCP_TEST_INSTRUCTIONS", "HTTP fixture.");
        if sse {
            cmd.arg("--sse");
        }
        let child = cmd.spawn().expect("python3 is required for the MCP tests");
        wait_for("the HTTP fixture port", || {
            std::fs::read_to_string(&port_file).is_ok_and(|p| !p.is_empty())
        })
        .await;
        let port = std::fs::read_to_string(&port_file).unwrap();
        HttpFixture {
            child,
            url: format!("http://127.0.0.1:{}/mcp", port.trim()),
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
