//! User configuration (`$DSHX_HOME/config.toml`, default `~/.dsh-codex/config.toml`).
//!
//! The `[codex]` table carries the settings of the original plugin bundle
//! (`computerUse`, `apps`, `browser`, `mini`), with the same keys and defaults.
//! `[mcp_servers.<name>]` tables configure MCP servers (dsh `mcp-client`).
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tool::SandboxMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    pub model: String,
    pub reasoning_effort: Option<String>,
    /// Anthropic-compatible endpoint root; `DEEPSEEK_BASE_URL` overrides it.
    pub base_url: String,
    pub sandbox: SandboxMode,
    /// `ask`: wider access needs the user's OK; `never`: never ask (deny in headless).
    pub approval: String,
    pub max_tokens: u32,
    pub codex: CodexConfig,
    /// MCP servers by name (the `mcp__<name>__<tool>` namespace).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: "deepseek-flash".into(),
            reasoning_effort: None,
            base_url: "https://api.deepseek.com/anthropic".into(),
            sandbox: SandboxMode::WorkspaceWrite,
            approval: "ask".into(),
            max_tokens: 256_000,
            codex: CodexConfig::default(),
            mcp_servers: BTreeMap::new(),
        }
    }
}

/// One `[mcp_servers.<name>]` table (dsh `mcp-client` config). `command`
/// selects the stdio transport, `url` Streamable HTTP; `transport` may name
/// it explicitly (`stdio` / `streamable-http`). dsh's camelCase field names
/// are accepted as aliases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct McpServerConfig {
    /// `false` skips the server without removing its table.
    pub enabled: bool,
    pub transport: Option<String>,
    /// stdio: executable, started without a shell.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// stdio: variables merged over the parent environment (which loses
    /// credential-like names and `DSH_*`).
    pub env: BTreeMap<String, String>,
    /// stdio: working directory; `None` = the process's.
    pub cwd: Option<PathBuf>,
    /// Streamable HTTP endpoint; plain `http://` only for loopback hosts.
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    /// Timeout per tool call or resource request.
    #[serde(alias = "toolCallTimeoutMs")]
    pub tool_call_timeout_ms: u64,
    /// Limit for the server's instructions (with attribution) in the prompt.
    #[serde(alias = "maxInstructionBytes")]
    pub max_instruction_bytes: usize,
    /// Fail startup instead of logging when the first connection fails.
    #[serde(alias = "failOnStartupError")]
    pub fail_on_startup_error: bool,
    pub reconnect: McpReconnectConfig,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        McpServerConfig {
            enabled: true,
            transport: None,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            tool_call_timeout_ms: 60_000,
            max_instruction_bytes: 32_768,
            fail_on_startup_error: false,
            reconnect: McpReconnectConfig::default(),
        }
    }
}

/// Reconnect policy after a lost MCP connection (dsh defaults).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct McpReconnectConfig {
    pub enabled: bool,
    /// First delay; doubles per consecutive failed attempt.
    #[serde(alias = "initialDelayMs")]
    pub initial_delay_ms: u64,
    /// Backoff ceiling, also the uptime after which the attempt budget resets.
    #[serde(alias = "maxDelayMs")]
    pub max_delay_ms: u64,
    /// Consecutive failed attempts per outage before giving up.
    #[serde(alias = "maxAttempts")]
    pub max_attempts: u32,
}

impl Default for McpReconnectConfig {
    fn default() -> Self {
        McpReconnectConfig {
            enabled: true,
            initial_delay_ms: 500,
            max_delay_ms: 30_000,
            max_attempts: 10,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CodexConfig {
    pub computer_use: ComputerUseConfig,
    pub apps: AppsConfig,
    pub browser: BrowserConfig,
    pub mini: MiniConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ComputerUseConfig {
    pub enabled: bool,
    /// `de` or `en`; `None` = system language.
    pub locale: Option<String>,
    pub accent: String,
    pub label: String,
    pub banner: Option<String>,
    pub esc_hint: Option<String>,
    pub show_label: bool,
    pub show_glow: bool,
    pub show_activity: bool,
    pub click_pulse: bool,
    pub max_width: u32,
    pub quality: f32,
    pub blocked_apps: Vec<String>,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        ComputerUseConfig {
            enabled: cfg!(target_os = "macos"),
            locale: None,
            accent: "#339cff".into(),
            label: "DeepSeek".into(),
            banner: None,
            esc_hint: None,
            show_label: false,
            show_glow: false,
            show_activity: true,
            click_pulse: true,
            max_width: 1280,
            quality: 0.8,
            blocked_apps: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppsConfig {
    pub enabled: bool,
}

// Not derivable: the default depends on the target OS.
#[allow(clippy::derivable_impls)]
impl Default for AppsConfig {
    fn default() -> Self {
        AppsConfig {
            enabled: cfg!(target_os = "macos"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserConfig {
    pub enabled: bool,
    /// Chrome/Chromium binary; `None` = auto-detect.
    pub chrome: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub idle_minutes: u32,
    pub quality: f32,
    /// macOS: keep Chrome away from the login keychain (`--use-mock-keychain`).
    /// Meant for throwaway profiles; cookies are then encrypted with a fixed
    /// key, so switching it on for a profile with sign-ins loses them.
    /// `DSHX_BROWSER_MOCK_KEYCHAIN=1` turns it on too.
    pub mock_keychain: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        BrowserConfig {
            enabled: true,
            chrome: None,
            width: 1280,
            height: 800,
            idle_minutes: 15,
            quality: 0.7,
            mock_keychain: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MiniConfig {
    pub done_after: u32,
    pub hide_after: u32,
}

impl Default for MiniConfig {
    fn default() -> Self {
        MiniConfig {
            done_after: 6,
            hide_after: 24,
        }
    }
}

/// `$DSHX_HOME` or `~/.dsh-codex`.
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("DSHX_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".dsh-codex")
}

impl Config {
    /// Reads `config.toml` from the home dir; missing file = defaults.
    pub fn load() -> anyhow::Result<Config> {
        let path = home_dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_servers_table() {
        let config: Config = toml::from_str(
            r#"
            [mcp_servers.github]
            command = "npx"
            args = ["-y", "server-github"]
            env = { GITHUB_TOKEN = "t" }

            [mcp_servers.web]
            url = "http://localhost:3000/mcp"
            headers = { Authorization = "Bearer x" }
            toolCallTimeoutMs = 5000
            reconnect = { enabled = false, max_attempts = 2 }
            "#,
        )
        .unwrap();
        let github = &config.mcp_servers["github"];
        assert!(github.enabled);
        assert_eq!(github.command.as_deref(), Some("npx"));
        assert_eq!(github.tool_call_timeout_ms, 60_000);
        let web = &config.mcp_servers["web"];
        assert_eq!(web.tool_call_timeout_ms, 5000);
        assert!(!web.reconnect.enabled);
        assert_eq!(web.reconnect.max_attempts, 2);
        assert_eq!(web.reconnect.initial_delay_ms, 500);
        assert!(Config::default().mcp_servers.is_empty());
    }
}
