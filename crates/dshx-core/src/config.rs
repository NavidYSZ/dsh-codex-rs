//! User configuration (`$DSHX_HOME/config.toml`, default `~/.dsh-codex/config.toml`).
//!
//! The `[codex]` table carries the settings of the original plugin bundle
//! (`computerUse`, `apps`, `browser`, `mini`), with the same keys and defaults,
//! plus the dsh automation features (`goal`, `schedule`, `time_context`,
//! `team`) with dsh 0.2.0-rc.2's defaults.
//! `[mcp_servers.<name>]` tables configure MCP servers (dsh `mcp-client`),
//! `[plugins."<name>@<marketplace>"]` tables switch installed plugins on.
//! `[folders]` widens what the web UI's folder browser may list.
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
    /// Installed plugins by id (`<name>@<marketplace>`); only enabled ones
    /// contribute skills and MCP servers.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Extra roots of the project folder browser (`/api/fs/dirs`).
    #[serde(skip_serializing_if = "FoldersConfig::is_empty")]
    pub folders: FoldersConfig,
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
            plugins: BTreeMap::new(),
            folders: FoldersConfig::default(),
        }
    }
}

/// `[folders]`: where the web UI's folder browser may look besides the home
/// folder and the parents of known projects, e.g. `roots =
/// ["/srv/deepseek-workspaces"]` on a server. `~/` is the home folder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FoldersConfig {
    pub roots: Vec<PathBuf>,
}

impl FoldersConfig {
    fn is_empty(&self) -> bool {
        self.roots.is_empty()
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
    /// Codex calls them `http_headers`.
    #[serde(alias = "http_headers")]
    pub headers: BTreeMap<String, String>,
    /// Streamable HTTP: bearer token read from this environment variable.
    #[serde(alias = "bearerTokenEnvVar", skip_serializing_if = "Option::is_none")]
    pub bearer_token_env_var: Option<String>,
    /// Streamable HTTP: OAuth settings. Without them a 401 still starts
    /// discovery and dynamic client registration; `enabled = false` turns
    /// OAuth off for the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
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
            bearer_token_env_var: None,
            oauth: None,
            tool_call_timeout_ms: 60_000,
            max_instruction_bytes: 32_768,
            fail_on_startup_error: false,
            reconnect: McpReconnectConfig::default(),
        }
    }
}

/// `[mcp_servers.<name>.oauth]`: a client registered in advance (for
/// authorization servers without dynamic registration, e.g. GitHub) and the
/// scopes to request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct McpOAuthConfig {
    pub enabled: bool,
    #[serde(alias = "clientId", skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(alias = "clientSecret", skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Empty = the scopes the server asks for (`WWW-Authenticate`) or advertises.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

impl Default for McpOAuthConfig {
    fn default() -> Self {
        McpOAuthConfig {
            enabled: true,
            client_id: None,
            client_secret: None,
            scopes: Vec::new(),
        }
    }
}

/// One `[plugins."<name>@<marketplace>"]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct PluginConfig {
    pub enabled: bool,
}

impl Default for PluginConfig {
    fn default() -> Self {
        PluginConfig { enabled: true }
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
    pub goal: GoalConfig,
    pub schedule: ScheduleConfig,
    #[serde(rename = "time_context", alias = "timeContext")]
    pub time_context: TimeContextConfig,
    pub team: TeamConfig,
}

/// Same-session goals (dsh `goal`, `tool-goal`, `goal-round-driver`; on in dsh-base).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct GoalConfig {
    pub enabled: bool,
    /// Round cap of a goal created without `max_goal_rounds`.
    #[serde(alias = "defaultMaxGoalRounds")]
    pub default_max_goal_rounds: u32,
    /// Rounds a goal round must reach before the model may report `blocked`.
    #[serde(alias = "blockedAfterConsecutiveRounds")]
    pub blocked_after_consecutive_rounds: u32,
}

impl Default for GoalConfig {
    fn default() -> Self {
        GoalConfig {
            enabled: true,
            default_max_goal_rounds: 256,
            blocked_after_consecutive_rounds: 3,
        }
    }
}

/// Host reminders (dsh `dsh-experimental-schedule-bundle`, opt-in). Tasks
/// live in `$DSHX_HOME/storages/schedule.json`; only `dsh-codex web`
/// delivers them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ScheduleConfig {
    pub enabled: bool,
    /// Days of delivery history kept per task.
    #[serde(alias = "deliveryHistoryDays")]
    pub delivery_history_days: u32,
    /// Delivery records kept per task.
    #[serde(alias = "deliveryHistoryRecords")]
    pub delivery_history_records: u32,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        ScheduleConfig {
            enabled: false,
            delivery_history_days: 30,
            delivery_history_records: 200,
        }
    }
}

/// Request clock readings (dsh `time-context`, part of the schedule bundle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TimeContextConfig {
    /// `None` follows `schedule.enabled`, like the dsh bundle.
    pub enabled: Option<bool>,
    /// IANA zone of the readings; `None` = the system zone.
    #[serde(alias = "timeZone")]
    pub time_zone: Option<String>,
    /// Minimum time between two readings (0 = every step).
    #[serde(alias = "refreshIntervalMs")]
    pub refresh_interval_ms: u64,
}

impl Default for TimeContextConfig {
    fn default() -> Self {
        TimeContextConfig {
            enabled: None,
            time_zone: None,
            refresh_interval_ms: 600_000,
        }
    }
}

/// Agent Teams (dsh `dsh-experimental-agent-team-profile`, opt-in): replaces
/// the subagent tools with the team tools. Limits are the profile's.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TeamConfig {
    pub enabled: bool,
    #[serde(alias = "maxMembers")]
    pub max_members: u32,
    #[serde(alias = "maxTasks")]
    pub max_tasks: u32,
    #[serde(alias = "maxPendingMessagesPerMember")]
    pub max_pending_messages_per_member: u32,
    #[serde(alias = "maxMessageBytes")]
    pub max_message_bytes: u32,
    #[serde(alias = "freshProvider")]
    pub fresh_provider: String,
    #[serde(alias = "forkProvider")]
    pub fork_provider: String,
}

impl Default for TeamConfig {
    fn default() -> Self {
        TeamConfig {
            enabled: false,
            max_members: 8,
            max_tasks: 256,
            max_pending_messages_per_member: 64,
            max_message_bytes: 65_536,
            fresh_provider: "spawn".into(),
            fork_provider: "fork".into(),
        }
    }
}

impl CodexConfig {
    /// Whether request clock readings are on (`time_context.enabled`, else `schedule.enabled`).
    pub fn time_context_enabled(&self) -> bool {
        self.time_context.enabled.unwrap_or(self.schedule.enabled)
    }
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

    #[test]
    fn plugins_and_oauth_tables() {
        let config: Config = toml::from_str(
            r#"
            [plugins."linear@openai-curated"]
            enabled = true

            [plugins."notes@local"]
            enabled = false

            [mcp_servers.gh]
            url = "https://api.githubcopilot.com/mcp/"
            http_headers = { X-Extra = "1" }
            oauth = { client_id = "Iv1.abc", scopes = ["repo"] }
            "#,
        )
        .unwrap();
        assert!(config.plugins["linear@openai-curated"].enabled);
        assert!(!config.plugins["notes@local"].enabled);
        let gh = &config.mcp_servers["gh"];
        assert_eq!(gh.headers["X-Extra"], "1");
        let oauth = gh.oauth.as_ref().unwrap();
        assert!(oauth.enabled);
        assert_eq!(oauth.client_id.as_deref(), Some("Iv1.abc"));
        // Unset optional tables stay out of a rewritten config.toml.
        let text = toml::to_string(&Config::default()).unwrap();
        assert!(
            !text.contains("plugins") && !text.contains("oauth") && !text.contains("folders"),
            "{text}"
        );
    }

    #[test]
    fn folders_table() {
        let config: Config = toml::from_str(
            r#"
            [folders]
            roots = ["/srv/deepseek-workspaces", "~/code"]
            "#,
        )
        .unwrap();
        assert_eq!(
            config.folders.roots,
            [
                PathBuf::from("/srv/deepseek-workspaces"),
                PathBuf::from("~/code")
            ]
        );
        assert!(Config::default().folders.roots.is_empty());
    }

    #[test]
    fn automation_tables() {
        let config: Config = toml::from_str(
            r#"
            [codex.goal]
            blocked_after_consecutive_rounds = 5

            [codex.schedule]
            enabled = true
            deliveryHistoryDays = 7

            [codex.time_context]
            time_zone = "Europe/Berlin"

            [codex.team]
            enabled = true
            max_members = 4
            "#,
        )
        .unwrap();
        let codex = &config.codex;
        assert!(codex.goal.enabled);
        assert_eq!(codex.goal.default_max_goal_rounds, 256);
        assert_eq!(codex.goal.blocked_after_consecutive_rounds, 5);
        assert!(codex.schedule.enabled);
        assert_eq!(codex.schedule.delivery_history_days, 7);
        assert_eq!(codex.schedule.delivery_history_records, 200);
        assert!(codex.time_context_enabled());
        assert_eq!(
            codex.time_context.time_zone.as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(codex.time_context.refresh_interval_ms, 600_000);
        assert!(codex.team.enabled);
        assert_eq!(codex.team.max_members, 4);
        assert_eq!(codex.team.max_tasks, 256);
        let defaults = CodexConfig::default();
        assert!(!defaults.schedule.enabled && !defaults.team.enabled);
        assert!(!defaults.time_context_enabled());
    }
}
