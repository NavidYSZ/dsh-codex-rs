//! Plugin manifests. Detection follows Codex: a root `plugin.json` that
//! declares the Agent Plugins 1.0 `$schema`, then `.codex-plugin/plugin.json`,
//! then `.claude-plugin/plugin.json`; a folder without any is read as a Claude
//! plugin in its standard layout (`skills/`, `.mcp.json`).
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

/// Where a plugin's metadata came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestFormat {
    /// Root `plugin.json` with the Agent Plugins 1.0 `$schema`.
    AgentPlugins,
    /// `.codex-plugin/plugin.json`.
    Codex,
    /// `.claude-plugin/plugin.json`.
    Claude,
    /// No manifest: the Claude standard layout.
    Layout,
}

impl ManifestFormat {
    /// Codex's `skills` field replaces the standard `skills/` folder;
    /// Claude's adds to it.
    pub fn skills_replace_default(self) -> bool {
        matches!(self, ManifestFormat::AgentPlugins | ManifestFormat::Codex)
    }

    /// Codex reads the standard `.mcp.json` only when the manifest names no
    /// MCP servers; Claude reads both.
    pub fn mcp_replaces_default(self) -> bool {
        self.skills_replace_default()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Manifest {
    pub format: ManifestFormat,
    pub path: Option<PathBuf>,
    pub data: Map<String, Value>,
}

/// Whether a `$schema` names the Agent Plugins format.
fn is_agent_plugins_schema(schema: &str) -> bool {
    let s = schema.to_ascii_lowercase().replace(['-', '_'], "");
    s.contains("agentplugin")
}

/// The manifest of the plugin folder `root`; an unreadable manifest is an error.
pub(crate) fn detect(root: &Path) -> Result<Manifest, String> {
    let root_manifest = root.join("plugin.json");
    if root_manifest.is_file() {
        let data = read_object(&root_manifest)?;
        if data
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(is_agent_plugins_schema)
        {
            return Ok(Manifest {
                format: ManifestFormat::AgentPlugins,
                path: Some(root_manifest),
                data,
            });
        }
    }
    for (dir, format) in [
        (".codex-plugin", ManifestFormat::Codex),
        (".claude-plugin", ManifestFormat::Claude),
    ] {
        let path = root.join(dir).join("plugin.json");
        if path.is_file() {
            return Ok(Manifest {
                format,
                data: read_object(&path)?,
                path: Some(path),
            });
        }
    }
    Ok(Manifest {
        format: ManifestFormat::Layout,
        path: None,
        data: Map::new(),
    })
}

pub(crate) fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{}: expected a JSON object", path.display())),
        Err(e) => Err(format!("{}: invalid JSON: {e}", path.display())),
    }
}

/// A manifest path (`./skills/`, `assets/logo.png`) inside `root`; `None`
/// when it is absolute or leaves the plugin folder.
pub(crate) fn resolve_inside(root: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative.trim());
    if rel.as_os_str().is_empty() || rel.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(root.join(out))
}

/// One string or an array of strings.
pub(crate) fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn string(data: &Map<String, Value>, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `author` as a string or `{name}`.
pub(crate) fn author_name(data: &Map<String, Value>) -> Option<String> {
    match data.get("author")? {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_plugin() {
        let root = Path::new("/p");
        assert_eq!(
            resolve_inside(root, "./skills/"),
            Some(PathBuf::from("/p/skills"))
        );
        assert_eq!(resolve_inside(root, "a/../b"), Some(PathBuf::from("/p/b")));
        assert_eq!(resolve_inside(root, "../x"), None);
        assert_eq!(resolve_inside(root, "/etc"), None);
        assert_eq!(resolve_inside(root, ""), None);
    }

    #[test]
    fn agent_plugins_schema() {
        assert!(is_agent_plugins_schema(
            "https://agentplugins.dev/schemas/1.0/plugin.json"
        ));
        assert!(is_agent_plugins_schema(
            "https://example.org/agent-plugins/1.0/plugin.schema.json"
        ));
        assert!(!is_agent_plugins_schema(
            "https://json.schemastore.org/package.json"
        ));
    }
}
