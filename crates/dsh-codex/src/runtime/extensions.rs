//! Plugins, marketplaces, skills, MCP servers and connector logins of a
//! runtime, and the Plugins page routes over them ([`RuntimeExtensions`]).
//!
//! Enabled plugins (`[plugins."<name>@<marketplace>"] enabled = true`) add
//! their skill folders to the skill registry and their MCP servers to the
//! configured ones. Every change re-applies both while the process runs:
//! the skill registry sees new folders at the next step, the MCP handle
//! starts and stops only what changed, and open agents pick up the new
//! tools through [`Runtime::refresh_agent`].
//!
//! Secret values of config servers (environment, headers, bearer tokens
//! entered on the MCP page) live in `credentials.toml` ([`mcp_form`]); the
//! Skills page reads skill folders and creates blank skills
//! ([`skill_files`]).
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::config::{Config, McpServerConfig};
use dshx_mcp::catalog;
use dshx_mcp::{CallbackParams, LoginRequest, ServerInfo};
use dshx_plugins::{InstallRecord, Marketplace, MarketplaceRecord, Plugin, PluginHome, plugin_id};
use dshx_server::{Extensions, NotFound, OAuthCallback};
use serde_json::{Value, json};

use super::Runtime;

mod mcp_form;
mod skill_files;

use mcp_form::{ServerForm, ServerSecrets};

/// How long "Test connection" waits by default, and at most.
const PROBE_WAIT: Duration = Duration::from_secs(30);
const PROBE_WAIT_MAX: Duration = Duration::from_secs(120);

/// How long a change waits for the first connection attempt of the
/// servers it (re)started.
const APPLY_WAIT: Duration = Duration::from_secs(10);

pub(crate) struct ExtensionState {
    plugins: PluginHome,
    /// Serializes installs, config edits and reloads.
    ops: tokio::sync::Mutex<()>,
}

impl ExtensionState {
    pub fn new(home: &Path) -> Self {
        ExtensionState {
            plugins: PluginHome::new(home),
            ops: tokio::sync::Mutex::new(()),
        }
    }

    /// Skill folders of the enabled plugins (blocking).
    pub fn skill_dirs(&self, config: &Config) -> Vec<PathBuf> {
        skill_dirs(config, &self.plugins.list())
    }
}

fn is_enabled(config: &Config, id: &str) -> bool {
    config.plugins.get(id).is_some_and(|p| p.enabled)
}

fn enabled<'a>(config: &'a Config, plugins: &'a [Plugin]) -> impl Iterator<Item = &'a Plugin> {
    plugins
        .iter()
        .filter(|p| p.error.is_none() && is_enabled(config, &p.id))
}

fn skill_dirs(config: &Config, plugins: &[Plugin]) -> Vec<PathBuf> {
    enabled(config, plugins)
        .flat_map(|p| p.skill_dirs.clone())
        .collect()
}

/// One MCP server of the config or of an enabled plugin.
struct ServerEntry {
    name: String,
    config: McpServerConfig,
    plugin: Option<String>,
    original_name: Option<String>,
    /// Why it cannot run (unsupported transport, name taken).
    error: Option<String>,
    warnings: Vec<String>,
    /// Values from `credentials.toml` (config servers only).
    secrets: ServerSecrets,
}

impl ServerEntry {
    /// The config the server runs with: its table plus its secrets.
    fn effective(&self) -> McpServerConfig {
        self.secrets.apply(&self.config)
    }
}

/// Config servers first, then the servers of enabled plugins; a plugin
/// server whose name is already taken is listed with an error.
fn server_entries(config: &Config, plugins: &[Plugin]) -> Vec<ServerEntry> {
    let mut entries: Vec<ServerEntry> = config
        .mcp_servers
        .iter()
        .map(|(name, cfg)| ServerEntry {
            name: name.clone(),
            config: cfg.clone(),
            plugin: None,
            original_name: None,
            error: None,
            warnings: Vec::new(),
            secrets: ServerSecrets::default(),
        })
        .collect();
    for plugin in enabled(config, plugins) {
        for server in &plugin.mcp_servers {
            let taken = entries
                .iter()
                .find(|e| e.name == server.name)
                .map(|e| match &e.plugin {
                    Some(other) => format!("the name {} is already used by {other}", server.name),
                    None => format!(
                        "the name {} is already used by [mcp_servers.{}]",
                        server.name, server.name
                    ),
                });
            entries.push(ServerEntry {
                name: server.name.clone(),
                config: server.config.clone(),
                plugin: Some(plugin.id.clone()),
                original_name: Some(server.original_name.clone()),
                error: server.error.clone().or(taken),
                warnings: server.warnings.clone(),
                secrets: ServerSecrets::default(),
            });
        }
    }
    entries
}

fn runnable(entries: &[ServerEntry]) -> BTreeMap<String, McpServerConfig> {
    entries
        .iter()
        .filter(|e| e.error.is_none())
        .map(|e| (e.name.clone(), e.effective()))
        .collect()
}

/// `dshx_plugins::Unknown` becomes the server's 404.
fn not_found(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<dshx_plugins::Unknown>() {
        Some(u) => NotFound(u.0.clone()).into(),
        None => e,
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await?.map_err(not_found)
}

impl Runtime {
    async fn plugins_now(&self) -> Vec<Plugin> {
        let home = self.ext.plugins.clone();
        tokio::task::spawn_blocking(move || home.list())
            .await
            .unwrap_or_default()
    }

    /// The enabled plugins that load without errors.
    pub(crate) async fn enabled_plugins(&self) -> Vec<Plugin> {
        let config = self.config();
        let plugins = self.plugins_now().await;
        enabled(&config, &plugins).cloned().collect()
    }

    /// The MCP servers each enabled plugin brings, by plugin id (for
    /// `@plugin` mentions).
    pub(crate) async fn plugin_servers(&self) -> Vec<(String, Vec<String>)> {
        self.enabled_plugins()
            .await
            .into_iter()
            .filter(|p| !p.mcp_servers.is_empty())
            .map(|p| {
                let servers = p.mcp_servers.iter().map(|s| s.name.clone()).collect();
                (p.id, servers)
            })
            .collect()
    }

    /// Config and plugin servers with the secrets of the config servers.
    async fn entries_now(&self, config: &Config, plugins: &[Plugin]) -> Vec<ServerEntry> {
        let credentials = self.credentials.clone();
        let mut secrets = tokio::task::spawn_blocking(move || mcp_form::load(&credentials))
            .await
            .unwrap_or_default();
        let mut entries = server_entries(config, plugins);
        for entry in entries.iter_mut().filter(|e| e.plugin.is_none()) {
            entry.secrets = secrets.remove(&entry.name).unwrap_or_default();
        }
        entries
    }

    /// Re-reads the plugins and applies the skill folders and MCP servers
    /// of the config and the enabled plugins. `wait` bounds the wait for
    /// started servers (`None` = until their first attempt finished).
    pub(crate) async fn reload_extensions(&self, wait: Option<Duration>) -> anyhow::Result<()> {
        let config = self.config();
        let plugins = self.plugins_now().await;
        self.skills.set_custom_dirs(skill_dirs(&config, &plugins));
        let entries = self.entries_now(&config, &plugins).await;
        for entry in entries.iter().filter(|e| e.error.is_some()) {
            tracing::warn!(
                "MCP server {} skipped: {}",
                entry.name,
                entry.error.as_deref().unwrap_or_default()
            );
        }
        self.mcp.apply(&runnable(&entries), wait).await
    }
}

/// The Plugins page routes over a runtime.
pub struct RuntimeExtensions {
    rt: Arc<Runtime>,
}

impl RuntimeExtensions {
    pub fn new(rt: Arc<Runtime>) -> Self {
        RuntimeExtensions { rt }
    }

    fn home(&self) -> PluginHome {
        self.rt.ext.plugins.clone()
    }

    fn oauth(&self) -> anyhow::Result<&Arc<dshx_mcp::OAuthManager>> {
        self.rt
            .mcp
            .oauth()
            .ok_or_else(|| anyhow::anyhow!("connector logins are not available"))
    }

    async fn plugin_value(&self, id: &str) -> anyhow::Result<Value> {
        let home = self.home();
        let owned = id.to_owned();
        let (plugin, installed) =
            blocking(move || Ok((home.get(&owned)?, home.installed()))).await?;
        Ok(plugin_json(
            &plugin,
            &self.rt.config(),
            &installed,
            &self.rt.mcp.status(),
        ))
    }

    async fn entry(&self, name: &str) -> anyhow::Result<ServerEntry> {
        let config = self.rt.config();
        let plugins = self.rt.plugins_now().await;
        self.rt
            .entries_now(&config, &plugins)
            .await
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| NotFound(format!("MCP server {name}")).into())
    }

    async fn server_value(&self, name: &str) -> anyhow::Result<Value> {
        let entry = self.entry(name).await?;
        Ok(self.server_json(&entry, &self.rt.mcp.status()))
    }

    fn server_json(&self, entry: &ServerEntry, statuses: &[ServerInfo]) -> Value {
        let cfg = &entry.config;
        let secrets = &entry.secrets;
        let status = statuses.iter().find(|s| s.name == entry.name);
        let url = cfg.url.as_deref().and_then(|u| url::Url::parse(u).ok());
        let login = match (&url, self.rt.mcp.oauth()) {
            (Some(url), Some(oauth)) => oauth.status(url),
            _ => None,
        };
        let oauth = dshx_mcp::uses_oauth(&entry.name, &entry.effective());
        // Per-chat settings (`always`, `disabled_tools`): the config table,
        // for plugin servers the plugin's table.
        let availability = self.rt.config().mcp_availability(&entry.name);
        let connector = catalog::connectors()
            .into_iter()
            .find(|c| cfg.url.as_deref() == Some(c.url.as_str()))
            .map(|c| c.id);
        let state = match (status, &entry.error) {
            (_, Some(_)) => "error",
            (Some(s), None) => s.status,
            (None, None) if !cfg.enabled => "disabled",
            (None, None) => "stopped",
        };
        json!({
            "name": entry.name,
            "origin": if entry.plugin.is_some() { "plugin" } else { "config" },
            "plugin": entry.plugin,
            "originalName": entry.original_name,
            "connector": connector,
            "enabled": cfg.enabled,
            "transport": if cfg.url.is_some() { "streamable-http" } else { "stdio" },
            "url": cfg.url,
            "command": cfg.command,
            "args": cfg.args,
            "cwd": cfg.cwd,
            "envVars": cfg.env_vars,
            "bearerTokenEnvVar": cfg.bearer_token_env_var,
            // Values may be secrets; the page only needs the names.
            "env": mcp_form::names(&cfg.env, &secrets.env),
            "headers": mcp_form::names(&cfg.headers, &secrets.headers),
            "secrets": {
                "env": secrets.env.keys().collect::<Vec<_>>(),
                "headers": secrets.headers.keys().collect::<Vec<_>>(),
                "bearerToken": secrets.bearer_token.is_some(),
            },
            "oauthEnabled": cfg.oauth.as_ref().is_none_or(|o| o.enabled),
            "always": availability.always,
            "disabledTools": availability.disabled_tools,
            "auth": { "oauth": oauth, "login": login },
            "status": state,
            "error": entry.error.clone().or_else(|| status.and_then(|s| s.error.clone())),
            "retrying": status.is_some_and(|s| s.retrying),
            "challenge": status.and_then(|s| s.challenge.clone()),
            "warnings": entry.warnings,
            "tools": status.map(|s| s.tools.clone()).unwrap_or_default(),
        })
    }

    /// `PUT /api/skills/{name}/settings`: `{enabled?, model?, cwd?}` → the
    /// skill's row of `GET /api/skills` (for `cwd`, default the home).
    pub(crate) async fn set_skill_settings(
        &self,
        name: &str,
        body: Value,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            dshx_tools::skill::is_skill_name(name),
            "invalid skill name {name:?}"
        );
        let flag = |key: &str| -> anyhow::Result<Option<bool>> {
            match body.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(*b)),
                Some(_) => anyhow::bail!("{key} must be true or false"),
            }
        };
        let (enabled, model) = (flag("enabled")?, flag("model")?);
        self.rt.update_config(|c| {
            let entry = c.skills.entries.entry(name.to_owned()).or_default();
            if let Some(on) = enabled {
                entry.enabled = on;
            }
            if let Some(on) = model {
                entry.model = on;
            }
            if *entry == Default::default() {
                c.skills.entries.remove(name);
            }
        })?;
        let cwd = body.get("cwd").and_then(Value::as_str).map(str::to_owned);
        let rows = self.skills(cwd).await?;
        let config = self.rt.config();
        let row = rows
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r["name"] == name && r["active"] == true)
                    .or_else(|| rows.iter().find(|r| r["name"] == name))
            })
            .cloned();
        Ok(row.unwrap_or_else(|| {
            json!({
                "name": name,
                "enabled": config.skills.enabled(name),
                "model": config.skills.model_invocable(name, true),
            })
        }))
    }

    /// `PUT /api/mcp/servers/{name}/settings`: `{always?, disabledTools?}` →
    /// the server row. Plugin servers keep theirs in the plugin's table.
    pub(crate) async fn set_mcp_settings(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let always = match body.get("always") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => anyhow::bail!("always must be true or false"),
        };
        let disabled: Option<Vec<String>> = match body.get("disabledTools") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                serde_json::from_value(v.clone())
                    .map_err(|_| anyhow::anyhow!("disabledTools must be a list of tool names"))?,
            ),
        };
        let _ops = self.rt.ext.ops.lock().await;
        let entry = self.entry(name).await?;
        let apply = |a: &mut dshx_core::config::McpAvailability| {
            if let Some(on) = always {
                a.always = on;
            }
            if let Some(list) = &disabled {
                let mut list = list.clone();
                list.sort();
                list.dedup();
                a.disabled_tools = list;
            }
        };
        self.rt.update_config(|c| match &entry.plugin {
            Some(plugin) => {
                let table = c.plugins.entry(plugin.clone()).or_default();
                let a = table.mcp_servers.entry(name.to_owned()).or_default();
                apply(a);
                if a.is_default() {
                    table.mcp_servers.remove(name);
                }
            }
            None => {
                if let Some(server) = c.mcp_servers.get_mut(name) {
                    let mut a = dshx_core::config::McpAvailability {
                        always: server.always,
                        disabled_tools: server.disabled_tools.clone(),
                    };
                    apply(&mut a);
                    server.always = a.always;
                    server.disabled_tools = a.disabled_tools;
                }
            }
        })?;
        self.server_value(name).await
    }

    /// A config change followed by a reload, under the change lock.
    async fn change_config(&self, f: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        self.rt.update_config(f)?;
        self.rt.reload_extensions(Some(APPLY_WAIT)).await
    }

    async fn save_secrets(&self, name: &str, secrets: Option<ServerSecrets>) -> anyhow::Result<()> {
        let credentials = self.rt.credentials.clone();
        let name = name.to_owned();
        blocking(move || mcp_form::save(&credentials, &name, secrets.as_ref())).await
    }

    /// A config server that the page may edit.
    async fn config_entry(&self, name: &str) -> anyhow::Result<ServerEntry> {
        let entry = self.entry(name).await?;
        if let Some(plugin) = &entry.plugin {
            anyhow::bail!("{name} belongs to the plugin {plugin}; change it in the plugin");
        }
        Ok(entry)
    }

    async fn restart(&self, name: &str) {
        if let Err(e) = self.rt.mcp.restart(name, Some(APPLY_WAIT)).await {
            tracing::debug!("restart of {name} skipped: {e}");
        }
    }

    /// The URL of an HTTP server that logs in through OAuth.
    async fn login_target(&self, name: &str) -> anyhow::Result<(ServerEntry, url::Url)> {
        let entry = self.entry(name).await?;
        if let Some(e) = &entry.error {
            anyhow::bail!("{name}: {e}");
        }
        anyhow::ensure!(
            dshx_mcp::uses_oauth(name, &entry.effective()),
            "{name} does not log in through OAuth (stdio, an Authorization header or bearer_token_env_var)"
        );
        let url = url::Url::parse(entry.config.url.as_deref().unwrap_or_default())?;
        Ok((entry, url))
    }
}

fn plugin_json(
    plugin: &Plugin,
    config: &Config,
    installed: &BTreeMap<String, InstallRecord>,
    statuses: &[ServerInfo],
) -> Value {
    let mut v = serde_json::to_value(plugin).unwrap_or_else(|_| json!({}));
    let on = is_enabled(config, &plugin.id);
    v["enabled"] = json!(on);
    v["installed"] = installed
        .get(&plugin.id)
        .and_then(|r| serde_json::to_value(r).ok())
        .unwrap_or(Value::Null);
    let skills: Vec<Value> = plugin
        .skill_dirs
        .iter()
        .flat_map(|d| dshx_tools::skill::discover_dir(d))
        .map(|s| json!({"name": s.name, "description": s.description, "path": s.path}))
        .collect();
    v["skills"] = Value::Array(skills);
    for key in ["logo", "composerIcon"] {
        if let Some(path) = plugin.interface.get(key).and_then(Value::as_str)
            && !path.contains("://")
            && !path.starts_with("data:")
        {
            v[format!("{key}Url")] = json!(format!("/api/plugins/{}/assets/{path}", plugin.id));
        }
    }
    if let Some(Value::Array(servers)) = v.get_mut("mcpServers") {
        for server in servers {
            let name = server["name"].as_str().unwrap_or_default().to_owned();
            let status = statuses.iter().find(|s| s.name == name);
            server["status"] = json!(match (status, on) {
                (Some(s), true) => s.status,
                (None, true) => "stopped",
                (_, false) => "disabled",
            });
        }
    }
    v
}

fn marketplace_json(
    record: &MarketplaceRecord,
    market: Result<Marketplace, String>,
    config: &Config,
    installed_ids: &HashSet<String>,
) -> Value {
    let mut v = serde_json::to_value(record).unwrap_or_else(|_| json!({}));
    match market {
        Ok(m) => {
            v["displayName"] = json!(m.display_name);
            v["description"] = json!(m.description);
            v["owner"] = m.owner.clone().unwrap_or(Value::Null);
            v["format"] = json!(m.format);
            let plugins: Vec<Value> = m
                .plugins
                .iter()
                .map(|e| {
                    let id = plugin_id(&e.name, &record.name);
                    let mut ev = serde_json::to_value(e).unwrap_or_else(|_| json!({}));
                    ev["installed"] = json!(installed_ids.contains(&id));
                    ev["enabled"] = json!(is_enabled(config, &id));
                    ev["id"] = json!(id);
                    ev
                })
                .collect();
            v["plugins"] = Value::Array(plugins);
        }
        Err(e) => {
            v["error"] = json!(e);
            v["plugins"] = json!([]);
        }
    }
    v
}

#[async_trait]
impl Extensions for RuntimeExtensions {
    async fn summary(&self) -> anyhow::Result<Value> {
        let config = self.rt.config();
        let plugins = self.rt.plugins_now().await;
        let skills = self.rt.skills.clone();
        let home = self.rt.home.clone();
        let markets = self.home();
        let (skill_count, market_count) = blocking(move || {
            let names: HashSet<String> = skills
                .candidates(&home)
                .into_iter()
                .map(|s| s.name)
                .collect();
            Ok((names.len(), markets.marketplaces().len()))
        })
        .await?;
        let statuses = self.rt.mcp.status();
        let count = |status: &str| statuses.iter().filter(|s| s.status == status).count();
        Ok(json!({
            "plugins": plugins.len(),
            "pluginsEnabled": enabled(&config, &plugins).count(),
            "marketplaces": market_count,
            "skills": skill_count,
            "mcpServers": statuses.len(),
            "mcpConnected": count("connected"),
            "mcpNeedsLogin": count("needs-login"),
        }))
    }

    async fn plugins(&self) -> anyhow::Result<Value> {
        let home = self.home();
        let (plugins, installed) = blocking(move || Ok((home.list(), home.installed()))).await?;
        let config = self.rt.config();
        let statuses = self.rt.mcp.status();
        Ok(Value::Array(
            plugins
                .iter()
                .map(|p| plugin_json(p, &config, &installed, &statuses))
                .collect(),
        ))
    }

    async fn plugin(&self, id: &str) -> anyhow::Result<Value> {
        self.plugin_value(id).await
    }

    async fn install_plugin(
        &self,
        marketplace: &str,
        name: &str,
        enable: bool,
    ) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let (market, plugin_name) = (marketplace.to_owned(), name.to_owned());
        let plugin = blocking(move || home.install(&market, &plugin_name)).await?;
        if enable {
            let id = plugin.id.clone();
            self.change_config(|c| {
                c.plugins.entry(id).or_default().enabled = true;
            })
            .await?;
        }
        self.plugin_value(&plugin.id).await
    }

    async fn update_plugin(&self, id: &str) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let owned = id.to_owned();
        blocking(move || home.update(&owned)).await?;
        self.rt.reload_extensions(Some(APPLY_WAIT)).await?;
        self.plugin_value(id).await
    }

    async fn remove_plugin(&self, id: &str) -> anyhow::Result<()> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let owned = id.to_owned();
        blocking(move || home.remove(&owned)).await?;
        let id = id.to_owned();
        self.change_config(|c| {
            c.plugins.remove(&id);
        })
        .await
    }

    async fn set_plugin_enabled(&self, id: &str, on: bool) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let owned = id.to_owned();
        let plugin = blocking(move || home.get(&owned)).await?;
        if let Some(e) = plugin.error.filter(|_| on) {
            anyhow::bail!("{id} cannot be enabled: {e}");
        }
        let key = id.to_owned();
        self.change_config(|c| {
            c.plugins.entry(key).or_default().enabled = on;
        })
        .await?;
        self.plugin_value(id).await
    }

    async fn plugin_asset(&self, id: &str, path: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let home = self.home();
        let (id, path) = (id.to_owned(), path.to_owned());
        blocking(move || {
            let plugin = home.get(&id)?;
            let (file, mime) = plugin
                .asset(&path)
                .ok_or_else(|| NotFound(format!("{id}: {path}")))?;
            Ok((std::fs::read(file)?, mime.to_owned()))
        })
        .await
    }

    async fn marketplaces(&self) -> anyhow::Result<Value> {
        let home = self.home();
        let config = self.rt.config();
        blocking(move || {
            let installed: HashSet<String> = home.list().into_iter().map(|p| p.id).collect();
            Ok(Value::Array(
                home.marketplaces()
                    .iter()
                    .map(|r| {
                        let market = home.marketplace(&r.name).map_err(|e| format!("{e:#}"));
                        marketplace_json(r, market, &config, &installed)
                    })
                    .collect(),
            ))
        })
        .await
    }

    async fn add_marketplace(&self, source: &str, git_ref: Option<&str>) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let config = self.rt.config();
        let (source, git_ref) = (source.to_owned(), git_ref.map(str::to_owned));
        blocking(move || {
            let (record, market) = home.add_marketplace(&source, git_ref.as_deref())?;
            let installed: HashSet<String> = home.list().into_iter().map(|p| p.id).collect();
            Ok(marketplace_json(&record, Ok(market), &config, &installed))
        })
        .await
    }

    async fn update_marketplace(&self, name: &str) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let config = self.rt.config();
        let name = name.to_owned();
        blocking(move || {
            let (record, market) = home.update_marketplace(&name)?;
            let installed: HashSet<String> = home.list().into_iter().map(|p| p.id).collect();
            Ok(marketplace_json(&record, Ok(market), &config, &installed))
        })
        .await
    }

    async fn remove_marketplace(&self, name: &str) -> anyhow::Result<()> {
        let _ops = self.rt.ext.ops.lock().await;
        let home = self.home();
        let name = name.to_owned();
        blocking(move || home.remove_marketplace(&name)).await
    }

    async fn skills(&self, cwd: Option<String>) -> anyhow::Result<Value> {
        let cwd = cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.rt.home.clone());
        let config = self.rt.config();
        let plugins = self.rt.plugins_now().await;
        let skills = self.rt.skills.clone();
        blocking(move || {
            let canonical = |d: &PathBuf| d.canonicalize().unwrap_or_else(|_| d.clone());
            let owners: Vec<(PathBuf, String)> = enabled(&config, &plugins)
                .flat_map(|p| p.skill_dirs.iter().map(|d| (canonical(d), p.id.clone())))
                .collect();
            let logos: BTreeMap<&str, String> = plugins
                .iter()
                .filter_map(|p| {
                    let path = p.interface.get("logo").and_then(Value::as_str)?;
                    let local = !path.contains("://") && !path.starts_with("data:");
                    local.then(|| {
                        (
                            p.id.as_str(),
                            format!("/api/plugins/{}/assets/{path}", p.id),
                        )
                    })
                })
                .collect();
            let title = |s: &dshx_tools::skill::SkillSummary| {
                skill_files::frontmatter_title(&s.path)
                    .unwrap_or_else(|| skill_files::title(None, &s.name))
            };
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for s in skills.candidates(&cwd) {
                let plugin = owners
                    .iter()
                    .find(|(dir, _)| s.path.starts_with(dir))
                    .map(|(_, id)| id.clone());
                out.push(json!({
                    "name": s.name,
                    "title": title(&s),
                    "logoUrl": plugin.as_deref().and_then(|id| logos.get(id)),
                    "description": s.description,
                    "whenToUse": s.when_to_use,
                    "source": if plugin.is_some() { "plugin" } else { s.source.as_str() },
                    "plugin": plugin,
                    "path": s.path,
                    "active": seen.insert(s.name.clone()),
                    "modelInvocable": s.model_invocable,
                    "userInvocable": s.user_invocable,
                    "enabled": config.skills.enabled(&s.name),
                    "model": config.skills.model_invocable(&s.name, s.model_invocable),
                }));
            }
            // Skills of disabled plugins, so the page can offer them.
            for p in plugins
                .iter()
                .filter(|p| p.error.is_none() && !is_enabled(&config, &p.id))
            {
                for s in p
                    .skill_dirs
                    .iter()
                    .flat_map(|d| dshx_tools::skill::discover_dir(d))
                {
                    out.push(json!({
                        "name": s.name,
                        "title": title(&s),
                        "logoUrl": logos.get(p.id.as_str()),
                        "description": s.description,
                        "whenToUse": s.when_to_use,
                        "source": "plugin",
                        "plugin": p.id,
                        "path": s.path,
                        "active": false,
                        "modelInvocable": s.model_invocable,
                        "userInvocable": s.user_invocable,
                        "enabled": config.skills.enabled(&s.name),
                        "model": config.skills.model_invocable(&s.name, s.model_invocable),
                    }));
                }
            }
            // Skill files that are no skill (yet): shown with their problem,
            // never invocable.
            for r in skills.rejected(&cwd) {
                let folder = match r.path.file_name().and_then(|f| f.to_str()) {
                    Some("SKILL.md") => r.path.parent().and_then(Path::file_name),
                    _ => r.path.file_stem(),
                };
                let name = folder
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let plugin = owners
                    .iter()
                    .find(|(dir, _)| r.path.starts_with(dir))
                    .map(|(_, id)| id.clone());
                out.push(json!({
                    "name": name,
                    "title": skill_files::title(None, &name),
                    "description": "",
                    "source": if plugin.is_some() { "plugin" } else { r.source.as_str() },
                    "plugin": plugin,
                    "path": r.path,
                    "active": false,
                    "invalid": true,
                    "error": r.reason,
                    "modelInvocable": false,
                    "userInvocable": false,
                    "enabled": config.skills.enabled(&name),
                    "model": false,
                }));
            }
            Ok(Value::Array(out))
        })
        .await
    }

    async fn mcp_servers(&self) -> anyhow::Result<Value> {
        let config = self.rt.config();
        let plugins = self.rt.plugins_now().await;
        let statuses = self.rt.mcp.status();
        Ok(Value::Array(
            self.rt
                .entries_now(&config, &plugins)
                .await
                .iter()
                .map(|e| self.server_json(e, &statuses))
                .collect(),
        ))
    }

    async fn add_mcp_server(&self, body: Value) -> anyhow::Result<Value> {
        let connector = match body.get("connector").and_then(Value::as_str) {
            Some(id) => {
                Some(catalog::connector(id).ok_or_else(|| NotFound(format!("connector {id}")))?)
            }
            None => None,
        };
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .or_else(|| connector.as_ref().map(|c| c.id.clone()))
            .ok_or_else(|| anyhow::anyhow!("name is required"))?;
        let mut body = body.clone();
        if let Some(c) = &connector
            && let Some(fields) = body
                .as_object_mut()
                .map(|b| b.entry("config").or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut)
        {
            fields
                .entry("url")
                .or_insert_with(|| Value::String(c.url.clone()));
        }
        let (cfg, secrets) = ServerForm::parse(&body)?.merge(None)?;
        dshx_mcp::check_server(&name, &secrets.apply(&cfg)).map_err(|e| anyhow::anyhow!(e))?;
        let _ops = self.rt.ext.ops.lock().await;
        if self.entry(&name).await.is_ok() {
            anyhow::bail!("an MCP server named {name} already exists");
        }
        self.save_secrets(&name, Some(secrets)).await?;
        let key = name.clone();
        self.change_config(|c| {
            c.mcp_servers.insert(key, cfg);
        })
        .await?;
        self.server_value(&name).await
    }

    async fn remove_mcp_server(&self, name: &str) -> anyhow::Result<()> {
        let _ops = self.rt.ext.ops.lock().await;
        let entry = self.entry(name).await?;
        if let Some(plugin) = &entry.plugin {
            anyhow::bail!("{name} belongs to the plugin {plugin}; disable the plugin instead");
        }
        let key = name.to_owned();
        self.change_config(|c| {
            c.mcp_servers.remove(&key);
        })
        .await?;
        self.save_secrets(name, None).await?;
        // Its tokens go with it.
        if let (Some(url), Ok(oauth)) = (
            entry
                .config
                .url
                .as_deref()
                .and_then(|u| url::Url::parse(u).ok()),
            self.oauth(),
        ) {
            let _ = oauth.logout(&url).await;
        }
        Ok(())
    }

    async fn set_mcp_server_enabled(&self, name: &str, on: bool) -> anyhow::Result<Value> {
        let _ops = self.rt.ext.ops.lock().await;
        let entry = self.entry(name).await?;
        if let Some(plugin) = &entry.plugin {
            anyhow::bail!(
                "{name} belongs to the plugin {plugin}; enable or disable the plugin instead"
            );
        }
        let key = name.to_owned();
        self.change_config(|c| {
            if let Some(server) = c.mcp_servers.get_mut(&key) {
                server.enabled = on;
            }
        })
        .await?;
        self.server_value(name).await
    }

    async fn restart_mcp_server(&self, name: &str) -> anyhow::Result<Value> {
        let entry = self.entry(name).await?;
        anyhow::ensure!(
            entry.config.enabled && entry.error.is_none(),
            "{name} is not running"
        );
        self.rt.mcp.restart(name, Some(APPLY_WAIT)).await?;
        self.server_value(name).await
    }

    async fn connectors(&self) -> anyhow::Result<Value> {
        let config = self.rt.config();
        let statuses = self.rt.mcp.status();
        Ok(Value::Array(
            catalog::connectors()
                .into_iter()
                .map(|c| {
                    let servers: Vec<&String> = config
                        .mcp_servers
                        .iter()
                        .filter(|(_, s)| s.url.as_deref() == Some(c.url.as_str()))
                        .map(|(name, _)| name)
                        .collect();
                    let status = servers
                        .first()
                        .and_then(|n| statuses.iter().find(|s| &&s.name == n))
                        .map(|s| s.status);
                    let mut v = serde_json::to_value(&c).unwrap_or_else(|_| json!({}));
                    v["added"] = json!(!servers.is_empty());
                    v["servers"] = json!(servers);
                    v["status"] = json!(status);
                    v
                })
                .collect(),
        ))
    }

    async fn start_login(&self, server: &str, redirect_uri: &str) -> anyhow::Result<Value> {
        let (entry, url) = self.login_target(server).await?;
        let oauth_config = entry.config.oauth.clone().unwrap_or_default();
        let headers = entry.effective().headers;
        let challenge = self.rt.mcp.challenge(server);
        let authorization_url = self
            .oauth()?
            .start(LoginRequest {
                server,
                url: &url,
                oauth: &oauth_config,
                headers: &headers,
                challenge: challenge.as_ref(),
                redirect_uri,
            })
            .await?;
        Ok(json!({
            "server": server,
            "authorizationUrl": authorization_url,
            "redirectUri": redirect_uri,
        }))
    }

    async fn finish_login(&self, params: OAuthCallback) -> anyhow::Result<String> {
        let done = self
            .oauth()?
            .complete(CallbackParams {
                code: params.code,
                state: params.state,
                iss: params.iss,
                error: params.error,
                error_description: params.error_description,
            })
            .await?;
        self.restart(&done.server).await;
        Ok(done.server)
    }

    async fn logout(&self, server: &str) -> anyhow::Result<Value> {
        let (_, url) = self.login_target(server).await?;
        self.oauth()?.logout(&url).await?;
        self.restart(server).await;
        self.server_value(server).await
    }

    async fn set_token(&self, server: &str, token: Option<String>) -> anyhow::Result<Value> {
        let (_, url) = self.login_target(server).await?;
        self.oauth()?.set_token(&url, token.as_deref())?;
        self.restart(server).await;
        self.server_value(server).await
    }

    async fn mcp_server(&self, name: &str) -> anyhow::Result<Value> {
        let entry = self.entry(name).await?;
        let mut value = self.server_json(&entry, &self.rt.mcp.status());
        let schemas: BTreeMap<String, Value> = self.rt.mcp.tool_schemas(name).into_iter().collect();
        if let Some(Value::Array(tools)) = value.get_mut("tools") {
            for tool in tools {
                let raw = tool["rawName"].as_str().unwrap_or_default();
                if let Some(schema) = schemas.get(raw) {
                    tool["inputSchema"] = schema.clone();
                }
            }
        }
        Ok(value)
    }

    async fn update_mcp_server(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let form = ServerForm::parse(&body)?;
        let _ops = self.rt.ext.ops.lock().await;
        let entry = self.config_entry(name).await?;
        let (cfg, secrets) = form.merge(Some((&entry.config, &entry.secrets)))?;
        dshx_mcp::check_server(name, &secrets.apply(&cfg)).map_err(|e| anyhow::anyhow!(e))?;
        self.save_secrets(name, Some(secrets)).await?;
        let key = name.to_owned();
        self.change_config(|c| {
            c.mcp_servers.insert(key, cfg);
        })
        .await?;
        self.server_value(name).await
    }

    async fn test_mcp_server(&self, body: Value) -> anyhow::Result<Value> {
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("test")
            .to_owned();
        let form = ServerForm::parse(&body)?;
        // Testing an edited server: masked values come from its secrets.
        let current = self.entry(&name).await.ok().filter(|e| e.plugin.is_none());
        let (cfg, secrets) = form.merge(current.as_ref().map(|e| (&e.config, &e.secrets)))?;
        let wait = body
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .map_or(PROBE_WAIT, Duration::from_millis)
            .clamp(Duration::from_secs(1), PROBE_WAIT_MAX);
        let probe = dshx_mcp::probe(&name, &secrets.apply(&cfg), self.rt.mcp.oauth(), wait).await;
        Ok(serde_json::to_value(probe)?)
    }

    async fn mcp_server_log(&self, name: &str) -> anyhow::Result<Value> {
        self.entry(name).await?;
        Ok(json!({ "name": name, "lines": self.rt.mcp.log(name) }))
    }

    async fn skill(
        &self,
        name: &str,
        cwd: Option<String>,
        path: Option<String>,
    ) -> anyhow::Result<Value> {
        // Only skills the page lists can be read.
        let rows = self.skills(cwd).await?;
        let mut row = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| {
                r["name"] == name
                    && path
                        .as_deref()
                        .is_none_or(|p| r["path"].as_str() == Some(p))
            })
            .cloned()
            .ok_or_else(|| NotFound(format!("skill {name}")))?;
        let file = PathBuf::from(row["path"].as_str().unwrap_or_default());
        let detail = blocking(move || skill_files::detail(&file)).await?;
        if let (Some(row), Value::Object(detail)) = (row.as_object_mut(), detail) {
            row.extend(detail);
        }
        Ok(row)
    }

    async fn create_skill(&self, body: Value) -> anyhow::Result<Value> {
        let text = |key: &str| {
            body.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_owned()
        };
        let (name, description, cwd) = (text("name"), text("description"), text("cwd"));
        let cwd = (!cwd.is_empty()).then_some(cwd);
        let dir = match body
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("personal")
        {
            "personal" => self
                .rt
                .skills
                .config()
                .agents_home
                .ok_or_else(|| anyhow::anyhow!("no home folder for personal skills"))?
                .join("skills"),
            "project" => {
                let cwd = cwd
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("a project skill needs the project folder"))?;
                skill_files::project_root(Path::new(cwd)).join(".agents/skills")
            }
            other => anyhow::bail!("unknown scope {other:?} (personal or project)"),
        };
        let rows = self.skills(cwd.clone()).await?;
        if let Some(other) = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["name"] == name.as_str() && r["invalid"] != true)
        {
            anyhow::bail!(
                "a skill named {name} already exists ({})",
                other["path"].as_str().unwrap_or_default()
            );
        }
        let (folder, skill) = (dir, name.clone());
        let file = blocking(move || {
            let file = skill_files::create(&folder, &skill, &description)?;
            Ok(file.canonicalize().unwrap_or(file))
        })
        .await?;
        self.skill(&name, cwd, Some(file.display().to_string()))
            .await
    }
}
