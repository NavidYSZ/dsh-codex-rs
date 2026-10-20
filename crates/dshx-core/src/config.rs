//! User configuration (`$DSHX_HOME/config.toml`, default `~/.dsh-codex/config.toml`).
//!
//! The `[codex]` table carries the settings of the original plugin bundle
//! (`computerUse`, `apps`, `browser`, `mini`), with the same keys and defaults.
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
