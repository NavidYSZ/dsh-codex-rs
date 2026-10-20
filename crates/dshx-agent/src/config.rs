//! Agent configuration. Defaults equal the dsh 0.2.0-rc.2 `headless` profile
//! (dsh-base rows for title, instructions, compaction, pruner, retry, reminder).
use std::path::PathBuf;
use std::time::Duration;

use dshx_core::SandboxMode;
use serde::{Deserialize, Serialize};

use crate::retry::RetryPolicy;

/// dsh approval policy: `ask` routes approvals to the [`dshx_core::Approver`];
/// `never` rejects every approval request without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalPolicy {
    #[default]
    Ask,
    Never,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalPolicy::Ask => "ask",
            ApprovalPolicy::Never => "never",
        }
    }

    /// `"ask"`/`"never"`; anything else is `ask`.
    pub fn parse(s: &str) -> Self {
        if s == "never" {
            ApprovalPolicy::Never
        } else {
            ApprovalPolicy::Ask
        }
    }
}

pub(crate) fn sandbox_str(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
        SandboxMode::DangerFullAccess => "danger-full-access",
    }
}

/// How a route accepts tool-set changes in history (dsh `toolUpdate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolUpdate {
    InHistory,
    AdditionOnly,
}

/// Prepared-call facts dsh's adapter reports for a route (`prepareCall()`).
#[derive(Debug, Clone, PartialEq)]
pub struct RouteCapabilities {
    /// Context window in tokens; `None` falls back to the client's model catalog.
    pub context_window: Option<u64>,
    /// `systemPromptUpdate: "in-history"`.
    pub system_prompt_in_history: bool,
    pub tool_update: Option<ToolUpdate>,
    /// Adapter default output cap (`maxTokens` when not configured).
    pub default_max_tokens: u32,
    /// Adapter default reasoning effort.
    pub default_effort: String,
}

impl RouteCapabilities {
    /// The DeepSeek adapter catalog: `deepseek-flash` supports in-history prompt
    /// and addition-only tool updates; other models support neither.
    pub fn for_model(model: &str) -> Self {
        let flash = model == "deepseek-flash";
        RouteCapabilities {
            context_window: None,
            system_prompt_in_history: flash,
            tool_update: flash.then_some(ToolUpdate::AdditionOnly),
            default_max_tokens: 256_000,
            default_effort: "high".into(),
        }
    }
}

/// An extra prompt section contributed by an embedding product.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtraSection {
    pub name: String,
    pub order: i64,
    pub text: String,
    /// Insert `text` verbatim, without `{{variable}}` interpolation (dsh
    /// `interpolate: false`, e.g. MCP server instructions).
    pub literal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptConfig {
    pub include_harness_identity: bool,
    /// `deployment:persona-prefix` template (order 0).
    pub persona_prefix: String,
    /// `deployment:persona-suffix` template (order 10200).
    pub persona_suffix: String,
    pub include_runtime_context: bool,
    pub extra_sections: Vec<ExtraSection>,
    /// Runtime-context contributions after `sandbox:policy` and
    /// `approval:policy`, in `order` (e.g. a subagent's `subagent:delegation`).
    pub extra_context: Vec<ExtraSection>,
}

impl Default for PromptConfig {
    fn default() -> Self {
        PromptConfig {
            include_harness_identity: true,
            persona_prefix: "You are a coding agent powered by the {{model}} model.".into(),
            persona_suffix: "Your working directory is {{cwd}}.".into(),
            include_runtime_context: true,
            extra_sections: Vec::new(),
            extra_context: Vec::new(),
        }
    }
}

/// AGENTS.md / CLAUDE.md loading (dsh `agent-instructions`).
#[derive(Debug, Clone, PartialEq)]
pub struct InstructionsConfig {
    pub enabled: bool,
    pub max_bytes: usize,
    pub max_source_bytes: usize,
    pub project_root_markers: Vec<String>,
    pub candidates: Vec<String>,
    pub local_candidates: Vec<String>,
    /// User-global harness home holding `AGENTS.md` (dsh: `~/.dsh`).
    pub harness_home: PathBuf,
    /// Model-facing label of `harness_home`.
    pub harness_home_display: String,
    /// Shared agents root (`~/.agents`, `$DSH_AGENTS_HOME`).
    pub agents_home: PathBuf,
    pub agents_home_display: String,
}

impl Default for InstructionsConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let (harness_home, harness_home_display) =
            match std::env::var_os("DSHX_HOME").filter(|v| !v.is_empty()) {
                Some(h) => (PathBuf::from(h), "$DSHX_HOME".to_owned()),
                None => (home.join(".dsh-codex"), "~/.dsh-codex".to_owned()),
            };
        let (agents_home, agents_home_display) =
            match std::env::var_os("DSH_AGENTS_HOME").filter(|v| !v.is_empty()) {
                Some(h) => (PathBuf::from(h), "$DSH_AGENTS_HOME".to_owned()),
                None => (home.join(".agents"), "~/.agents".to_owned()),
            };
        InstructionsConfig {
            enabled: true,
            max_bytes: 65_536,
            max_source_bytes: 1_048_576,
            project_root_markers: vec![".git".into()],
            candidates: vec!["AGENTS.md".into(), "CLAUDE.md".into()],
            local_candidates: vec!["AGENTS.local.md".into(), "CLAUDE.local.md".into()],
            harness_home,
            harness_home_display,
            agents_home,
            agents_home_display,
        }
    }
}

/// Pressure compaction and overflow recovery (dsh `compaction-basic`).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub auto: bool,
    pub threshold_ratio: f64,
    pub headroom_tokens: u64,
    pub retain_ratio: f64,
    pub retain_tokens: Option<u64>,
    /// Summarizer model; `None` = the latest routed request model.
    pub summarization_model: Option<String>,
    /// Summary output cap; `None` = `headroom_tokens`.
    pub max_tokens: Option<u32>,
    pub compaction_retries: u32,
    pub max_overflow_retries: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        CompactionConfig {
            auto: true,
            threshold_ratio: 0.8,
            headroom_tokens: 65_536,
            retain_ratio: 0.16,
            retain_tokens: None,
            summarization_model: None,
            max_tokens: None,
            compaction_retries: 1,
            max_overflow_retries: 1,
        }
    }
}

/// Model-free head/tail pruning of oversized tool results (dsh `compaction-tool-result-pruner`).
#[derive(Debug, Clone, PartialEq)]
pub struct PrunerConfig {
    pub enabled: bool,
    pub threshold_chars: usize,
    pub head_chars: usize,
    pub tail_chars: usize,
}

impl Default for PrunerConfig {
    fn default() -> Self {
        PrunerConfig {
            enabled: true,
            threshold_chars: 8192,
            head_chars: 4096,
            tail_chars: 1024,
        }
    }
}

/// Session titles (dsh `session-title` + `session-title-first-prompt-llm`).
#[derive(Debug, Clone, PartialEq)]
pub struct TitleConfig {
    pub enabled: bool,
    /// Ask the model for a title (otherwise only the fallback is written).
    pub llm: bool,
    pub fallback_max_words: usize,
    pub fallback_max_bytes: usize,
    pub max_title_bytes: usize,
    pub target_words: u32,
    pub target_cjk_characters: u32,
    pub max_input_bytes: usize,
    pub max_output_tokens: u32,
    pub timeout: Duration,
}

impl Default for TitleConfig {
    fn default() -> Self {
        TitleConfig {
            enabled: true,
            llm: true,
            fallback_max_words: 5,
            fallback_max_bytes: 40,
            max_title_bytes: 80,
            target_words: 5,
            target_cjk_characters: 10,
            max_input_bytes: 4096,
            max_output_tokens: 64,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Repeated identical tool-call reminders (dsh `repeat-tool-reminder`).
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatReminderConfig {
    pub enabled: bool,
    pub thresholds: Vec<u32>,
    pub arguments_preview_chars: usize,
}

impl Default for RepeatReminderConfig {
    fn default() -> Self {
        RepeatReminderConfig {
            enabled: true,
            thresholds: vec![3, 5, 8],
            arguments_preview_chars: 500,
        }
    }
}

/// Everything that shapes one agent's requests and behaviour.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Reasoning effort; `None` = adapter default (recorded as `adapterDefaults`).
    pub effort: Option<String>,
    /// Output cap; `None` = adapter default (recorded as `adapterDefaults`).
    pub max_tokens: Option<u32>,
    pub sandbox: SandboxMode,
    pub approval: ApprovalPolicy,
    /// Workspace; also the `{{cwd}}` of the system prompt.
    pub cwd: PathBuf,
    /// Parallel-safe tool calls in flight per step (dsh `maxParallelToolCalls`).
    pub max_parallel_tool_calls: usize,
    /// How long a cancelled or timed-out tool may take to settle before it is dropped.
    pub tool_grace: Duration,
    /// Route capabilities; `None` = [`RouteCapabilities::for_model`].
    pub route: Option<RouteCapabilities>,
    pub prompt: PromptConfig,
    pub instructions: InstructionsConfig,
    pub compaction: CompactionConfig,
    pub pruner: PrunerConfig,
    pub title: TitleConfig,
    pub retry: RetryPolicy,
    pub repeat_reminder: RepeatReminderConfig,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        AgentConfig {
            model: model.into(),
            effort: None,
            max_tokens: None,
            sandbox: SandboxMode::WorkspaceWrite,
            approval: ApprovalPolicy::Ask,
            cwd: cwd.into(),
            max_parallel_tool_calls: 10,
            tool_grace: Duration::from_secs(5),
            route: None,
            prompt: PromptConfig::default(),
            instructions: InstructionsConfig::default(),
            compaction: CompactionConfig::default(),
            pruner: PrunerConfig::default(),
            title: TitleConfig::default(),
            retry: RetryPolicy::default(),
            repeat_reminder: RepeatReminderConfig::default(),
        }
    }

    /// From the user's `config.toml`. `max_tokens` equal to the adapter default
    /// counts as unset so the header records it as an adapter default like dsh.
    pub fn from_core(core: &dshx_core::config::Config, cwd: impl Into<PathBuf>) -> Self {
        let mut c = AgentConfig::new(core.model.clone(), cwd);
        c.effort = core.reasoning_effort.clone().filter(|e| !e.is_empty());
        c.max_tokens = (core.max_tokens != 256_000).then_some(core.max_tokens);
        c.sandbox = core.sandbox;
        c.approval = ApprovalPolicy::parse(&core.approval);
        c
    }

    pub(crate) fn route(&self) -> RouteCapabilities {
        self.route
            .clone()
            .unwrap_or_else(|| RouteCapabilities::for_model(&self.model))
    }
}
