//! Everything a front end (headless, web, SDK) needs to run agents: config,
//! API key, DeepSeek client, session store, attachments and the tool set
//! (built-ins, Codex tools, the `skill` tool and MCP servers). Skills and
//! MCP servers reach a chat only on demand (see [`ondemand`]).
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use anyhow::Context as _;
use dshx_agent::{
    Agent, AgentConfig, AgentDeps, AgentObserver, ExtraSection, PreStep, PreStepHook, Preset,
    RouteCapabilities, TurnEndCompaction,
};
use dshx_core::config::{CompactionMode, CompactionSettings, Config};
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ImageAttachmentRef, LlmClient, Message,
    SessionEvent, SessionLog, Tool, ToolRegistry,
};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, ProviderRouter, SharedApiKey};
use dshx_session::{Credentials, FileAttachments, FileLog, SessionStore};
use dshx_tools::skill::{CatalogHistory, SkillConfig, SkillRegistry, SkillTool};

mod browser;
mod extensions;
pub(crate) mod ondemand;
mod providers;
mod suggest;

pub(crate) use browser::parse_settings as parse_browser_settings;
pub use extensions::RuntimeExtensions;
pub use providers::RuntimeProviders;
pub use suggest::SuggestContext;

use crate::automation::Automation;
use crate::live::{LiveHub, LiveMini, Watched};

pub struct Runtime {
    pub home: PathBuf,
    config: RwLock<Config>,
    pub store: Arc<SessionStore>,
    pub attachments: Arc<FileAttachments>,
    pub credentials: Credentials,
    pub api_key: SharedApiKey,
    /// The DeepSeek client this runtime started with, for DeepSeek model
    /// metadata; requests go through `router`.
    #[allow(dead_code)]
    pub llm: Arc<DeepSeekClient>,
    /// DeepSeek plus every configured provider route; what agents talk to.
    pub router: Arc<ProviderRouter>,
    provider_keys: providers::ProviderKeys,
    /// One background Chrome per process, shared by all sessions.
    browser: Option<Arc<dshx_browser::BrowserTool>>,
    /// Saved launch settings at process start; edits take effect on restart.
    browser_at_start: dshx_core::config::BrowserConfig,
    macos: Vec<Arc<dyn Tool>>,
    /// Browser frames and Mac tool screenshots for the web UI's live view.
    pub live: Arc<LiveHub>,
    /// In-process subagents of every agent this runtime starts.
    pub subagents: Arc<dshx_subagent::SubagentRuntime>,
    /// Local skills behind the `skill` tool and the session skill catalog.
    skills: Arc<SkillRegistry>,
    /// MCP servers of the config and of enabled plugins; empty until
    /// [`Runtime::start_mcp`] ran, re-applied when extensions change.
    mcp: dshx_mcp::McpHandle,
    /// Plugins, marketplaces and the lock of their changes.
    ext: extensions::ExtensionState,
    /// Goals, schedules, time context and Agent Teams.
    pub automation: Arc<Automation>,
    /// Per session: MCP servers switched on (`true`) or off through the API
    /// that no step has recorded in the log yet.
    mcp_pending: Mutex<HashMap<String, BTreeMap<String, bool>>>,
    /// Bumped by every config change (skill and MCP settings among them).
    settings_version: AtomicU64,
    /// This runtime, for hooks that outlive a call.
    me: OnceLock<Weak<Runtime>>,
}

impl Runtime {
    /// Loads `~/.dsh-codex` (or `$DSHX_HOME`): config, stored API key, sessions.
    pub fn load() -> anyhow::Result<Arc<Runtime>> {
        let home = dshx_core::config::home_dir();
        let config = Config::load()
            .with_context(|| format!("reading {}", home.join("config.toml").display()))?;
        let store = SessionStore::new(home.join("sessions"));
        let attachments = FileAttachments::new(&home);
        let credentials = Credentials::new(home.join("credentials.toml"));
        Self::build(home, config, store, attachments, credentials)
    }

    pub fn build(
        home: PathBuf,
        config: Config,
        store: SessionStore,
        attachments: FileAttachments,
        credentials: Credentials,
    ) -> anyhow::Result<Arc<Runtime>> {
        // The environment wins over the stored key, like dsh.
        let key = dshx_session::resolve_api_key(&credentials).ok().flatten();
        let api_key = SharedApiKey::new(key);
        let attachments = Arc::new(attachments);
        let resolver_store = attachments.clone();
        let llm = DeepSeekClient::builder(DeepSeekConfig::from_core(&config))
            .api_key(Arc::new(api_key.clone()))
            .attachments(attachments.clone() as Arc<dyn AttachmentStore>)
            .image_path(Arc::new(move |image: &ImageAttachmentRef| {
                resolver_store
                    .object_path(&image.attachment_id)
                    .ok()
                    .map(|p| p.display().to_string())
            }))
            .build()
            .map_err(|e| anyhow::anyhow!("DeepSeek client: {e}"))?;
        let live = LiveHub::new();
        let mini = Arc::new(LiveMini {
            inner: dshx_macos::browser_mini(&config.codex),
            hub: live.clone(),
        });
        let browser = if config.codex.browser.enabled {
            let locale = config
                .codex
                .computer_use
                .locale
                .clone()
                .unwrap_or_else(dshx_browser::system_locale);
            let opts = dshx_browser::BrowserOptions::from_config(&config.codex.browser, &locale);
            opts.chrome.is_some().then(|| {
                Arc::new(dshx_browser::BrowserTool::new(dshx_browser::Browser::new(
                    opts, mini,
                )))
            })
        } else {
            None
        };
        let macos = dshx_macos::macos_tools(&config.codex);
        let store = Arc::new(store);
        let llm = Arc::new(llm);
        let router = Arc::new(ProviderRouter::new(llm.clone()));
        let sessions = Arc::new(dshx_subagent::StoreSessions::new(store.clone()));
        let subagents = dshx_subagent::SubagentRuntime::new(dshx_subagent::RuntimeDeps {
            llm: router.clone() as Arc<dyn LlmClient>,
            sessions: sessions.clone(),
            attachments: attachments.clone() as Arc<dyn AttachmentStore>,
            approver: Arc::new(NoApprover),
        });
        let automation = Arc::new(Automation::new(&config, &subagents, sessions));
        let skills = Arc::new(SkillRegistry::new(SkillConfig {
            harness_home: Some(home.clone()),
            ..SkillConfig::default()
        }));
        let ext = extensions::ExtensionState::new(&home);
        // Skill folders of enabled plugins join the catalog right away.
        skills.set_custom_dirs(ext.skill_dirs(&config));
        skills.set_policy(Some(config.skills.clone()));
        let mcp = dshx_mcp::McpHandle::new(Some(dshx_mcp::OAuthManager::new(home.join("oauth"))));
        // Children see the skill catalog too, like in dsh.
        let child_skills = skills.clone();
        let child_automation = automation.clone();
        subagents.set_child_agent(Arc::new(move |child: &Agent| {
            child.add_pre_step_hook(Arc::new(SkillsHook {
                skills: child_skills.clone(),
            }));
            child_automation.attach_child(child);
        }));
        subagents.set_child_tools(Arc::new(Automation::child_tools));
        let runtime = Arc::new(Runtime {
            home,
            browser_at_start: config.codex.browser.clone(),
            config: RwLock::new(config),
            store,
            attachments,
            credentials,
            api_key,
            llm,
            router,
            provider_keys: Default::default(),
            browser,
            macos,
            live,
            subagents,
            skills,
            mcp,
            ext,
            automation,
            mcp_pending: Mutex::default(),
            settings_version: AtomicU64::new(0),
            me: OnceLock::new(),
        });
        let _ = runtime.me.set(Arc::downgrade(&runtime));
        runtime.apply_providers();
        Ok(runtime)
    }

    /// Connects the MCP servers of the config and of enabled plugins
    /// (concurrently, each bounded by its handshake timeout, like dsh before
    /// its first turn). `wait` bounds the wait: servers that take longer keep
    /// connecting in the background (the web UI shows them as connecting),
    /// and only failures within it count. Without an enabled server this
    /// does nothing.
    pub async fn start_mcp(&self, wait: Option<std::time::Duration>) -> anyhow::Result<()> {
        if let Err(e) = self.reload_extensions(wait).await {
            self.mcp.shutdown().await;
            return Err(e);
        }
        Ok(())
    }

    /// Changes whenever the MCP tool set or server instructions change
    /// (servers added, removed or restarted at runtime) and with every
    /// config change (skill and MCP settings).
    pub fn tools_version(&self) -> u64 {
        let mcp = *self.mcp.subscribe().borrow();
        mcp.wrapping_add(self.settings_version.load(Ordering::SeqCst))
    }

    /// dsh's file-reference guidance (order 900) and the instructions of
    /// `always` MCP servers (dsh order 3100). Servers a chat activates bring
    /// their instructions with the activation note instead.
    pub fn prompt_sections(&self) -> Vec<ExtraSection> {
        let config = self.config();
        let mut sections = vec![ExtraSection {
            name: "context:file-reference".into(),
            order: 900,
            text: ondemand::FILE_REFERENCE_PROMPT.into(),
            literal: true,
        }];
        sections.push(ExtraSection {
            name: "tools:browser-routing".into(),
            order: 950,
            text: browser::ROUTING.into(),
            literal: true,
        });
        sections.extend(
            self.mcp
                .prompt_sections_where(|server| config.mcp_availability(server).always)
                .into_iter()
                .map(|s| ExtraSection {
                    name: s.name,
                    order: s.order,
                    text: s.text,
                    literal: true,
                }),
        );
        sections
    }

    /// Brings an open agent's tools and prompt sections up to date when they
    /// changed since `seen` (call before each turn). A Minimal agent
    /// composes neither.
    pub fn refresh_agent(&self, agent: &Agent, seen: &AtomicU64) {
        if agent.config().preset == Preset::Minimal {
            return;
        }
        let version = self.tools_version();
        if seen.swap(version, Ordering::SeqCst) == version {
            return;
        }
        let config = agent.config();
        let update = config.route.as_ref().and_then(|r| r.tool_update);
        agent.set_tools(self.session_tools(
            &agent.session_id(),
            &config.cwd,
            &agent.events(),
            update,
        ));
        let sections = self.prompt_sections();
        agent.update_config(|c| c.prompt.extra_sections = sections);
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// Applies a change, writes it into `config.toml` (only the keys it
    /// touched; comments and everything else stay) and returns the new
    /// config. Open agents pick up skill and MCP settings before their next
    /// turn.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> anyhow::Result<Config> {
        let mut config = self.config.write().unwrap();
        let before = config.clone();
        f(&mut config);
        std::fs::create_dir_all(&self.home)?;
        let path = self.home.join("config.toml");
        let old = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let text = crate::config_file::render(&old, &before, &config)?;
        // Provider headers can hold secrets too. Create the atomic write
        // owner-only instead of widening an existing 0600 config to 0644.
        let tmp = self
            .home
            .join(format!(".config.{}.tmp", dshx_core::new_id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options
            .open(&tmp)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(error) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }
        self.skills.set_policy(Some(config.skills.clone()));
        self.settings_version.fetch_add(1, Ordering::SeqCst);
        Ok(config.clone())
    }

    pub fn api_key_configured(&self) -> bool {
        self.api_key.get().is_some()
    }

    /// Stores (or with `None` removes) the API key and uses it right away.
    pub fn set_api_key(&self, key: Option<&str>) -> anyhow::Result<()> {
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => {
                self.credentials.set_api_key(k)?;
                self.api_key.set(Some(k.to_owned()));
            }
            None => {
                self.credentials.clear_api_key()?;
                self.api_key.set(
                    std::env::var(dshx_llm::API_KEY_ENV)
                        .ok()
                        .filter(|k| !k.is_empty()),
                );
            }
        }
        Ok(())
    }

    pub fn tool_flags(&self) -> (bool, bool, bool) {
        let names: Vec<String> = self.macos.iter().map(|t| t.schema().name).collect();
        (
            self.browser.is_some(),
            names.iter().any(|n| n == "app"),
            names.iter().any(|n| n == "computer"),
        )
    }

    /// The tools every Standard session has: dsh's built-ins plus the Codex
    /// tools (no `skill` tool, no MCP).
    fn base_tools(&self) -> ToolRegistry {
        let config = self.config();
        let mut tools = ToolRegistry::new();
        for tool in dshx_tools::default_tools(&config) {
            // Not wired to a search provider yet; offering it would only produce errors.
            if tool.schema().name == "web_search" {
                continue;
            }
            tools.register(tool);
        }
        if let Some(browser) = &self.browser {
            tools.register(Arc::new(Watched {
                inner: browser.clone(),
                hub: self.live.clone(),
            }));
        }
        for tool in &self.macos {
            tools.register(Arc::new(Watched {
                inner: tool.clone(),
                hub: self.live.clone(),
            }));
        }
        self.automation.register_tools(&mut tools, &self.subagents);
        tools
    }

    /// Whether any enabled skill of `cwd` is model-invocable.
    pub fn model_skills(&self, cwd: &Path) -> bool {
        self.skills
            .snapshot(cwd)
            .skills
            .iter()
            .any(|s| s.model_invocable)
    }

    /// The tools of one Standard session: the base set, the `skill` tool
    /// while a skill is model-invocable, and the MCP tools the session's log
    /// and pending choices select. On the addition-only route, tools the
    /// session already declared stay declared.
    pub fn session_tools(
        &self,
        session: &str,
        cwd: &Path,
        events: &[SessionEvent],
        update: Option<dshx_agent::ToolUpdate>,
    ) -> ToolRegistry {
        let mut tools = self.base_tools();
        let history = self.mcp_choices(session, events);
        let keep = update == Some(dshx_agent::ToolUpdate::AdditionOnly);
        if self.model_skills(cwd) || (keep && history.declared.contains("skill")) {
            tools.register(Arc::new(SkillTool::new(self.skills.clone())));
        }
        let config = self.config();
        let selection = ondemand::McpSelection {
            config: &config,
            choices: &history.choices,
            declared: &history.declared,
            tool_update: update,
        };
        for tool in self.mcp_tools(&selection) {
            tools.register(tool);
        }
        tools
    }

    /// The tools of one session with the given preset: Minimal has dsh's
    /// persistent shell and nothing else.
    fn tools_for(
        &self,
        preset: Preset,
        log: &dyn SessionLog,
        config: &AgentConfig,
    ) -> ToolRegistry {
        match preset {
            Preset::Standard | Preset::Ptc => self.session_tools(
                log.id(),
                &config.cwd,
                &log.events(),
                config.route.as_ref().and_then(|r| r.tool_update),
            ),
            Preset::Minimal => {
                let mut tools = ToolRegistry::new();
                tools.register(Arc::new(
                    dshx_tools::persistent_bash::PersistentBashTool::new(),
                ));
                tools
            }
        }
    }

    /// The configured preset of new chats.
    pub fn default_preset(&self) -> Preset {
        crate::presets::default_of(&self.config())
    }

    /// A new session for `cwd`; a chosen `preset` is recorded in the header
    /// like dsh does (`agentPreset`) and stays for the session's life.
    pub fn create_session(&self, cwd: &str, preset: Option<Preset>) -> anyhow::Result<FileLog> {
        let mut header = SessionStore::new_header(cwd);
        if let Some(preset) = preset {
            header
                .extra
                .insert("agentPreset".into(), preset.id().into());
        }
        Ok(self.store.create_with_header(header)?)
    }

    /// Agent settings for `cwd` from the config, with per-call overrides.
    pub fn agent_config(&self, cwd: &Path, overrides: &Overrides) -> AgentConfig {
        let mut core = self.config();
        if let Some(m) = &overrides.model {
            core.model = m.clone();
        }
        if let Some(e) = &overrides.effort {
            core.reasoning_effort = Some(e.clone());
        }
        if let Some(s) = overrides.sandbox {
            core.sandbox = s;
        }
        // `core.model` is a reference; the agent gets its route and bare id.
        let reference = core.model.clone();
        let (provider, model) = self.split_model(&reference);
        core.model = model;
        let mut config = AgentConfig::from_core(&core, cwd);
        config.compaction.turn_end = turn_end_compaction(&core.codex.compaction);
        config.route = Some(self.route(&reference));
        config.title.model = self
            .router
            .route(&provider)
            .and_then(|c| c.spec().title_model.clone());
        config.provider = Some(provider);
        config.prompt.extra_sections = self.prompt_sections();
        config
    }

    /// Route capabilities of a model reference from its provider's catalog.
    pub fn route(&self, model: &str) -> RouteCapabilities {
        let resolved = self.router.resolve(model).model;
        let mut route = RouteCapabilities::for_model(&resolved.info.id);
        route.system_prompt_in_history = resolved.system_prompt_update.is_some();
        route.tool_update = resolved.tool_update.map(|u| match u {
            dshx_llm::ToolUpdate::InHistory => dshx_agent::ToolUpdate::InHistory,
            dshx_llm::ToolUpdate::AdditionOnly => dshx_agent::ToolUpdate::AdditionOnly,
        });
        route.default_max_tokens = resolved.default_max_tokens;
        route.context_window = Some(resolved.info.context_window);
        if let Some(effort) = resolved.info.default_reasoning_effort {
            route.default_effort = effort;
        }
        route
    }

    /// An agent for `log`, composed like the preset the session recorded.
    pub fn new_agent(
        &self,
        mut config: AgentConfig,
        log: Arc<dyn SessionLog>,
        approver: Arc<dyn Approver>,
        observer: Option<Arc<dyn AgentObserver>>,
    ) -> Agent {
        let recorded = dshx_session::agent_preset(&log.header(), &log.events());
        let preset = Preset::of_session(recorded.as_deref());
        config.apply_preset(preset);
        let tools = self.tools_for(preset, log.as_ref(), &config);
        let agent = Agent::new(
            config,
            AgentDeps {
                llm: self.router.clone() as Arc<dyn LlmClient>,
                tools,
                log,
                attachments: self.attachments.clone() as Arc<dyn AttachmentStore>,
                approver,
                observer,
                tool_state: None,
            },
        );
        // Skills, delegation, goals, Teams and the time context belong to
        // Standard's composition.
        if preset != Preset::Minimal {
            agent.add_pre_step_hook(Arc::new(SkillsHook {
                skills: self.skills.clone(),
            }));
            if let Some(me) = self.me.get() {
                agent.add_pre_step_hook(Arc::new(ondemand::McpHook {
                    runtime: me.clone(),
                    session: agent.session_id(),
                }));
            }
            self.subagents.attach(&agent);
            self.automation.attach_root(&agent);
        }
        agent
    }

    /// Stops an agent and everything it delegated.
    pub async fn close_agent(&self, agent: &Agent) {
        let id = agent.session_id();
        self.automation.close(&id).await;
        self.subagents.drain_descendants(&id).await;
        agent.dispose().await;
        self.subagents.detach(&id);
    }

    /// Closes the browser so its profile is saved cleanly, and stops the MCP
    /// servers (stdio servers with their whole process group).
    pub async fn shutdown(&self) {
        self.automation.shutdown().await;
        self.subagents.shutdown().await;
        if let Some(browser) = &self.browser {
            browser.browser().close().await;
        }
        self.mcp.shutdown().await;
    }
}

/// dsh `tool-skill`'s two pre-step listeners: the session skill catalog
/// (first publication, replacements, withdrawal) after the runtime context,
/// then one `skill-invocation` message per `/name` gesture. The `skill`
/// tool comes and goes with the model-invocable skills (`[skills]`); the
/// addition-only route keeps one it declared.
struct SkillsHook {
    skills: Arc<SkillRegistry>,
}

#[async_trait::async_trait]
impl PreStepHook for SkillsHook {
    async fn pre_step(
        &self,
        step: &PreStep<'_>,
        entering: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        let history =
            step.read_history(|events, visible| CatalogHistory::from_events(events, visible));
        let offered = step.tools.iter().any(|t| t == "skill");
        let skills = self.skills.clone();
        let cwd = step.cwd.to_path_buf();
        let claimed = step.claimed.to_vec();
        // Discovery reads skill files; keep it off the async workers.
        let (snapshot, invocations) = tokio::task::spawn_blocking(move || {
            let snapshot = skills.snapshot(&cwd);
            let invocations = dshx_tools::skill::invocation_messages(&skills, &cwd, &claimed);
            (snapshot, invocations)
        })
        .await?;
        let wanted = snapshot.skills.iter().any(|s| s.model_invocable);
        if wanted && !offered {
            let tool = Arc::new(SkillTool::new(self.skills.clone()));
            step.update_tools(|tools| tools.register(tool));
        } else if !wanted && offered {
            let declared = step.tool_update == Some(dshx_agent::ToolUpdate::AdditionOnly)
                && step.read_history(|events, _| {
                    events.iter().any(|e| {
                        e.kind == dshx_core::kinds::REQUEST_HEADER
                            && e.data["header"]["tools"]
                                .as_array()
                                .is_some_and(|t| t.iter().any(|t| t["name"] == "skill"))
                    })
                });
            if !declared {
                step.update_tools(|tools| {
                    let mut next = ToolRegistry::new();
                    for tool in tools.iter().filter(|t| t.schema().name != "skill") {
                        next.register(tool.clone());
                    }
                    *tools = next;
                });
            }
        }
        let max = self.skills.config().catalog_description_max_length;
        let batch = std::mem::take(entering);
        let snapshot = wanted.then_some(&snapshot);
        *entering = dshx_tools::skill::apply_catalog(batch, snapshot, max, &history);
        entering.extend(invocations);
        Ok(())
    }
}

/// The agent's turn-end compaction for `[codex.compaction]`.
pub fn turn_end_compaction(settings: &CompactionSettings) -> TurnEndCompaction {
    match settings.mode {
        CompactionMode::Threshold => TurnEndCompaction::Threshold(settings.threshold_tokens),
        CompactionMode::EveryTurn => TurnEndCompaction::EveryTurn,
        CompactionMode::Off => TurnEndCompaction::Off,
    }
}

/// Per-prompt choices that override the config (composer pickers, CLI flags).
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub sandbox: Option<dshx_core::SandboxMode>,
}

/// Headless runs have nobody to ask: dsh answers "unavailable".
pub struct NoApprover;

#[async_trait::async_trait]
impl Approver for NoApprover {
    async fn approve(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> ApprovalDecision {
        ApprovalDecision::Unavailable
    }
}
