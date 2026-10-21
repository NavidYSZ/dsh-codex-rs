//! Plugins and marketplaces of dsh-codex.
//!
//! A plugin is a folder `$DSHX_HOME/plugins/<marketplace>/<name>/` in one of
//! three formats (see [`ManifestFormat`]); it contributes skill folders and
//! MCP servers once `[plugins."<name>@<marketplace>"] enabled = true` is set
//! in `config.toml` (the runtime applies that). Marketplaces are catalogs
//! (`.agents/plugins/marketplace.json` or `.claude-plugin/marketplace.json`)
//! in a git repository or a local folder; installing copies or clones a
//! plugin from one into the plugins folder.
//!
//! Everything here is blocking file and `git` work; call it off the async
//! workers.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod install;
mod manifest;
mod marketplace;
mod mcp;

pub use manifest::ManifestFormat;
pub use marketplace::{Marketplace, MarketplaceEntry, MarketplaceRecord, PluginSource, SourceKind};
pub use mcp::{PluginMcpServer, server_name};

use manifest::{author_name, resolve_inside, string, strings};

/// An unknown plugin or marketplace (the server answers 404).
#[derive(Debug)]
pub struct Unknown(pub String);

impl std::error::Error for Unknown {}

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not found: {}", self.0)
    }
}

/// One plugin folder as loaded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    /// `<name>@<marketplace>`, the key of its `[plugins]` table.
    pub id: String,
    /// Folder name.
    pub name: String,
    pub marketplace: String,
    pub root: PathBuf,
    pub format: ManifestFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<Value>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    /// Display metadata (Codex `interface`: `displayName`, `shortDescription`,
    /// `brandColor`, `logo`, ...). Asset paths are relative to `root`;
    /// `displayName` and `shortDescription` are always filled.
    pub interface: Map<String, Value>,
    /// Skill roots, in precedence order.
    pub skill_dirs: Vec<PathBuf>,
    pub mcp_servers: Vec<PluginMcpServer>,
    /// Ignored parts (missing folders, unsupported components).
    pub warnings: Vec<String>,
    /// Set when the manifest cannot be read; the plugin then contributes nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `<name>@<marketplace>`.
pub fn plugin_id(name: &str, marketplace: &str) -> String {
    format!("{name}@{marketplace}")
}

/// Splits a plugin id into name and marketplace.
pub fn parse_id(id: &str) -> Option<(&str, &str)> {
    let (name, market) = id.rsplit_once('@')?;
    (is_component_name(name) && is_component_name(market)).then_some((name, market))
}

/// Plugin and marketplace names double as folder names:
/// `[A-Za-z0-9._-]{1,64}`, not starting with a dot.
pub fn is_component_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Loads the plugin in `root`. Never fails: problems end up in `error` and
/// `warnings`. `data_dir` backs `${CLAUDE_PLUGIN_DATA}`.
pub fn load_plugin(root: &Path, name: &str, marketplace: &str, data_dir: &Path) -> Plugin {
    let mut plugin = Plugin {
        id: plugin_id(name, marketplace),
        name: name.to_owned(),
        marketplace: marketplace.to_owned(),
        root: root.to_path_buf(),
        format: ManifestFormat::Layout,
        manifest_path: None,
        version: None,
        description: None,
        author: None,
        homepage: None,
        repository: None,
        license: None,
        keywords: Vec::new(),
        interface: Map::new(),
        skill_dirs: Vec::new(),
        mcp_servers: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };
    let manifest = match manifest::detect(root) {
        Ok(m) => m,
        Err(e) => {
            plugin.error = Some(e);
            plugin.interface = fallback_interface(&Map::new(), name);
            return plugin;
        }
    };
    let data = &manifest.data;
    plugin.format = manifest.format;
    plugin.manifest_path = manifest.path.clone();
    if let Some(declared) = string(data, "name")
        && declared != name
    {
        plugin
            .warnings
            .push(format!("the manifest names the plugin \"{declared}\""));
    }
    plugin.version = string(data, "version");
    plugin.description = string(data, "description");
    plugin.author = data.get("author").cloned();
    plugin.homepage = string(data, "homepage");
    plugin.repository = match data.get("repository") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => o.get("url").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    };
    plugin.license = string(data, "license");
    plugin.keywords = strings(data.get("keywords"));
    plugin.interface = interface(data, name, root, &mut plugin.warnings);
    plugin.skill_dirs = skill_dirs(&manifest, root, &mut plugin.warnings);
    plugin.mcp_servers = mcp_servers(&manifest, name, root, data_dir, &mut plugin.warnings);
    for (dir, what) in [
        ("commands", "slash commands"),
        ("agents", "subagent definitions"),
        ("hooks", "hooks"),
    ] {
        if root.join(dir).is_dir() || data.contains_key(dir) {
            plugin
                .warnings
                .push(format!("{what} ({dir}) are not supported and ignored"));
        }
    }
    plugin
}

fn fallback_interface(data: &Map<String, Value>, name: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(
        "displayName".into(),
        Value::String(string(data, "name").unwrap_or_else(|| name.to_owned())),
    );
    if let Some(d) = string(data, "description") {
        out.insert("shortDescription".into(), Value::String(d));
    }
    out
}

/// Codex `interface` plus fallbacks from the Claude fields; asset paths are
/// checked to stay inside the plugin.
fn interface(
    data: &Map<String, Value>,
    name: &str,
    root: &Path,
    warnings: &mut Vec<String>,
) -> Map<String, Value> {
    let mut out = match data.get("interface") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    for (key, value) in fallback_interface(data, name) {
        out.entry(key).or_insert(value);
    }
    if let Some(dev) = author_name(data) {
        out.entry("developerName").or_insert(Value::String(dev));
    }
    if let Some(home) = string(data, "homepage") {
        out.entry("websiteURL").or_insert(Value::String(home));
    }
    let mut asset = |value: &str| -> Option<Value> {
        if value.starts_with("https://") || value.starts_with("data:") {
            return Some(Value::String(value.to_owned()));
        }
        match resolve_inside(root, value) {
            Some(path) if path.is_file() => Some(Value::String(relative(root, &path))),
            _ => {
                warnings.push(format!("asset {value:?} not found in the plugin"));
                None
            }
        }
    };
    for key in ["logo", "composerIcon"] {
        if let Some(v) = out.get(key).and_then(Value::as_str).map(str::to_owned) {
            match asset(&v) {
                Some(v) => out.insert(key.into(), v),
                None => out.remove(key),
            };
        }
    }
    if let Some(Value::Array(shots)) = out.get("screenshots").cloned() {
        let kept: Vec<Value> = shots
            .iter()
            .filter_map(Value::as_str)
            .filter_map(&mut asset)
            .collect();
        out.insert("screenshots".into(), Value::Array(kept));
    }
    if !out.contains_key("logo") {
        let conventional = [
            "logo.svg",
            "logo.png",
            "assets/logo.svg",
            "assets/logo.png",
            "assets/icon.png",
            "icon.png",
        ];
        if let Some(found) = conventional.iter().find(|p| root.join(p).is_file()) {
            out.insert("logo".into(), Value::String((*found).to_owned()));
        }
    }
    out
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn skill_dirs(
    manifest: &manifest::Manifest,
    root: &Path,
    warnings: &mut Vec<String>,
) -> Vec<PathBuf> {
    let declared = strings(manifest.data.get("skills"));
    let mut dirs: Vec<PathBuf> = Vec::new();
    let default = root.join("skills");
    if (declared.is_empty() || !manifest.format.skills_replace_default()) && default.is_dir() {
        dirs.push(default);
    }
    for d in declared {
        match resolve_inside(root, &d) {
            Some(path) if path.is_dir() => {
                if !dirs.contains(&path) {
                    dirs.push(path);
                }
            }
            _ => warnings.push(format!("skill folder {d:?} not found in the plugin")),
        }
    }
    dirs
}

fn mcp_servers(
    manifest: &manifest::Manifest,
    name: &str,
    root: &Path,
    data_dir: &Path,
    warnings: &mut Vec<String>,
) -> Vec<PluginMcpServer> {
    let mut maps: Vec<Map<String, Value>> = Vec::new();
    let read_file = |rel: &str, warnings: &mut Vec<String>, maps: &mut Vec<_>| match resolve_inside(
        root, rel,
    ) {
        Some(path) if path.is_file() => match manifest::read_object(&path) {
            Ok(data) => maps.push(mcp::server_map(&data)),
            Err(e) => warnings.push(e),
        },
        _ => warnings.push(format!("MCP file {rel:?} not found in the plugin")),
    };
    let declared = manifest.data.get("mcpServers");
    if declared.is_none() || !manifest.format.mcp_replaces_default() {
        if let Some(default) = [".mcp.json", "mcp.json"]
            .into_iter()
            .find(|f| root.join(f).is_file())
        {
            read_file(default, warnings, &mut maps);
        }
    }
    match declared {
        Some(Value::String(path)) => read_file(path, warnings, &mut maps),
        Some(Value::Array(paths)) => {
            for path in paths.iter().filter_map(Value::as_str) {
                read_file(path, warnings, &mut maps);
            }
        }
        Some(Value::Object(inline)) => maps.push(inline.clone()),
        Some(_) => warnings.push("mcpServers must be a path, a list of paths or an object".into()),
        None => {}
    }
    let vars = mcp::Vars {
        root,
        data: data_dir,
    };
    // Later sources (the manifest) win over the standard file.
    let mut by_name: BTreeMap<String, Value> = BTreeMap::new();
    for map in maps {
        for (server, raw) in map {
            by_name.insert(server, raw);
        }
    }
    let servers: Vec<PluginMcpServer> = by_name
        .iter()
        .map(|(server, raw)| mcp::normalize(name, server, raw, &vars))
        .collect();
    let mut seen = std::collections::HashSet::new();
    servers
        .into_iter()
        .map(|mut s| {
            if !seen.insert(s.name.clone()) {
                s.error = Some(format!("the server name {} is used twice", s.name));
            }
            s
        })
        .collect()
}

/// Bookkeeping of an installed plugin (`plugins/installed.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRecord {
    pub marketplace: String,
    pub name: String,
    pub version: Option<String>,
    /// Where it came from (folder or git URL, ref, subfolder).
    pub source: String,
    /// Git commit of the installed copy.
    pub commit: Option<String>,
    pub installed_at: u64,
    pub updated_at: u64,
}

/// The plugin and marketplace folders under the dsh-codex home.
#[derive(Debug, Clone)]
pub struct PluginHome {
    home: PathBuf,
}

impl PluginHome {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        PluginHome { home: home.into() }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// `$DSHX_HOME/plugins`.
    pub fn plugins_dir(&self) -> PathBuf {
        self.home.join("plugins")
    }

    pub fn plugin_dir(&self, name: &str, marketplace: &str) -> PathBuf {
        self.plugins_dir().join(marketplace).join(name)
    }

    /// `${CLAUDE_PLUGIN_DATA}`: `$DSHX_HOME/plugin-data/<marketplace>/<name>`.
    pub fn data_dir(&self, name: &str, marketplace: &str) -> PathBuf {
        self.home.join("plugin-data").join(marketplace).join(name)
    }

    /// Every plugin folder, sorted by id.
    pub fn list(&self) -> Vec<Plugin> {
        let mut out = Vec::new();
        for market in subdirs(&self.plugins_dir()) {
            for name in subdirs(&self.plugins_dir().join(&market)) {
                let root = self.plugin_dir(&name, &market);
                out.push(load_plugin(
                    &root,
                    &name,
                    &market,
                    &self.data_dir(&name, &market),
                ));
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// One installed plugin by id.
    pub fn get(&self, id: &str) -> anyhow::Result<Plugin> {
        let (name, market) = parse_id(id).ok_or_else(|| Unknown(id.to_owned()))?;
        let root = self.plugin_dir(name, market);
        if !root.is_dir() {
            return Err(Unknown(id.to_owned()).into());
        }
        Ok(load_plugin(
            &root,
            name,
            market,
            &self.data_dir(name, market),
        ))
    }

    /// Install bookkeeping by plugin id.
    pub fn installed(&self) -> BTreeMap<String, InstallRecord> {
        std::fs::read(self.plugins_dir().join("installed.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_installed(&self, records: &BTreeMap<String, InstallRecord>) -> anyhow::Result<()> {
        write_json(&self.plugins_dir().join("installed.json"), records)
    }
}

/// Visible subfolders of `dir`, sorted.
fn subdirs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_component_name(n))
        .collect();
    names.sort();
    names
}

/// Writes JSON atomically (temp file + rename).
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn now_ms() -> u64 {
    dshx_core::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        assert_eq!(
            parse_id("linear@openai-curated"),
            Some(("linear", "openai-curated"))
        );
        assert_eq!(parse_id("a@b@c"), None);
        assert_eq!(parse_id("../x@m"), None);
        assert!(is_component_name("my_plugin.v2"));
        assert!(!is_component_name(".hidden"));
        assert!(!is_component_name("a/b"));
    }
}
