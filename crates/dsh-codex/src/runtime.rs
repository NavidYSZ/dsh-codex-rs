//! Everything a front end (headless, web, SDK) needs to run agents: config,
//! API key, DeepSeek client, session store, attachments and the tool set.
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use dshx_agent::{Agent, AgentConfig, AgentDeps, AgentObserver, RouteCapabilities};
use dshx_core::config::Config;
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ImageAttachmentRef, LlmClient, SessionLog, Tool,
    ToolRegistry,
};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, SharedApiKey};
use dshx_session::{Credentials, FileAttachments, SessionStore};

pub struct Runtime {
    pub home: PathBuf,
    config: RwLock<Config>,
    pub store: Arc<SessionStore>,
    pub attachments: Arc<FileAttachments>,
    pub credentials: Credentials,
    pub api_key: SharedApiKey,
    pub llm: Arc<DeepSeekClient>,
    /// One background Chrome per process, shared by all sessions.
    browser: Option<Arc<dshx_browser::BrowserTool>>,
    macos: Vec<Arc<dyn Tool>>,
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
        let mini = dshx_macos::browser_mini(&config.codex);
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
        Ok(Arc::new(Runtime {
            home,
            config: RwLock::new(config),
            store: Arc::new(store),
            attachments,
            credentials,
            api_key,
            llm: Arc::new(llm),
            browser,
            macos,
        }))
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// Applies a change, writes `config.toml` and returns the new config.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> anyhow::Result<Config> {
        let mut config = self.config.write().unwrap();
        f(&mut config);
        std::fs::create_dir_all(&self.home)?;
        let text = toml::to_string_pretty(&*config)?;
        let path = self.home.join("config.toml");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
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

    /// The tools of one session: dsh's built-ins plus the Codex tools.
    pub fn tools(&self) -> ToolRegistry {
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
            tools.register(browser.clone() as Arc<dyn Tool>);
        }
        for tool in &self.macos {
            tools.register(tool.clone());
        }
        tools
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
        let mut config = AgentConfig::from_core(&core, cwd);
        config.route = Some(self.route(&config.model));
        config
    }

    /// Route capabilities from the DeepSeek model catalog.
    pub fn route(&self, model: &str) -> RouteCapabilities {
        let resolved = self.llm.resolve_model(model);
        let mut route = RouteCapabilities::for_model(model);
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

    pub fn new_agent(
        &self,
        config: AgentConfig,
        log: Arc<dyn SessionLog>,
        approver: Arc<dyn Approver>,
        observer: Option<Arc<dyn AgentObserver>>,
    ) -> Agent {
        Agent::new(
            config,
            AgentDeps {
                llm: self.llm.clone() as Arc<dyn LlmClient>,
                tools: self.tools(),
                log,
                attachments: self.attachments.clone() as Arc<dyn AttachmentStore>,
                approver,
                observer,
                tool_state: None,
            },
        )
    }

    /// Closes the browser so its profile is saved cleanly.
    pub async fn shutdown(&self) {
        if let Some(browser) = &self.browser {
            browser.browser().close().await;
        }
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
