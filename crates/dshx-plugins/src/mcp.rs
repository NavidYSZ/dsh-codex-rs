//! MCP servers of a plugin (`.mcp.json`, `mcp.json` or inline `mcpServers`),
//! normalized to [`McpServerConfig`] under a plugin-prefixed name.
//!
//! Accepted entry fields are the union of Claude's `.mcp.json` and Codex's
//! `[mcp_servers]`: `type`/`transport`, `command`, `args`, `env`, `cwd`,
//! `url`, `headers`/`http_headers`, `env_http_headers`,
//! `bearer_token_env_var`, `enabled`, `tool_timeout_sec`, `oauth`.
//! `${CLAUDE_PLUGIN_ROOT}`/`${PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`/
//! `${PLUGIN_DATA}` and `${VAR}`/`${VAR:-default}` from the environment are
//! expanded in strings.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dshx_core::config::{McpOAuthConfig, McpServerConfig};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Longest MCP server name (it becomes the `mcp__<name>__` tool namespace).
const MAX_SERVER_NAME: usize = 32;

/// One MCP server a plugin brings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMcpServer {
    /// Name in the MCP namespace (`<plugin>_<server>`, at most 32 characters).
    pub name: String,
    /// Name inside the plugin's MCP file.
    pub original_name: String,
    #[serde(skip)]
    pub config: McpServerConfig,
    /// Why the server cannot run (unsupported transport, missing fields).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `<plugin>_<server>` (just `<plugin>` when both are equal) with every
/// character outside `[A-Za-z0-9_-]` replaced; names over 32 characters are
/// cut and end in a hash of both parts so they stay distinct.
pub fn server_name(plugin: &str, server: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let (p, s) = (clean(plugin), clean(server));
    let full = if p == s || s.is_empty() {
        p
    } else {
        format!("{p}_{s}")
    };
    if !full.is_empty() && full.len() <= MAX_SERVER_NAME {
        return full;
    }
    let hash: String = Sha256::digest(format!("{plugin}\0{server}").as_bytes())
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect();
    let keep = MAX_SERVER_NAME - hash.len() - 1;
    format!("{}_{hash}", &full[..keep.min(full.len())])
}

/// Values for `${...}` expansion.
pub(crate) struct Vars<'a> {
    pub root: &'a Path,
    pub data: &'a Path,
}

impl Vars<'_> {
    /// Expands the variables of `text`; unknown environment variables
    /// without a default become empty and are reported in `missing`.
    pub fn expand(&self, text: &str, missing: &mut Vec<String>) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                return out;
            };
            let inner = &after[..end];
            let (name, default) = match inner.split_once(":-") {
                Some((n, d)) => (n, Some(d)),
                None => (inner, None),
            };
            let value = match name {
                "CLAUDE_PLUGIN_ROOT" | "PLUGIN_ROOT" | "CODEX_PLUGIN_ROOT" => {
                    Some(self.root.display().to_string())
                }
                "CLAUDE_PLUGIN_DATA" | "PLUGIN_DATA" => Some(self.data.display().to_string()),
                _ => std::env::var(name).ok().filter(|v| !v.is_empty()),
            };
            match (value, default) {
                (Some(v), _) => out.push_str(&v),
                (None, Some(d)) => out.push_str(d),
                (None, None) => {
                    if !missing.iter().any(|m| m == name) {
                        missing.push(name.to_owned());
                    }
                }
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Plugin-relative command or folder (`./bin/server`) made absolute.
    fn path(&self, value: String) -> String {
        if value.starts_with("./") || value.starts_with("../") {
            self.root.join(value).display().to_string()
        } else {
            value
        }
    }
}

/// The server map of an MCP file: `{"mcpServers": {...}}` or the bare map.
pub(crate) fn server_map(data: &Map<String, Value>) -> Map<String, Value> {
    match data.get("mcpServers") {
        Some(Value::Object(servers)) => servers.clone(),
        _ => data
            .iter()
            .filter(|(k, v)| !k.starts_with('$') && v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

fn string_map(
    value: Option<&Value>,
    vars: &Vars,
    missing: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let Some(Value::Object(map)) = value else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            Some((k.clone(), vars.expand(&text, missing)))
        })
        .collect()
}

fn field<'a>(raw: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|n| raw.get(*n))
}

fn text<'a>(raw: &'a Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    field(raw, names).and_then(Value::as_str)
}

/// Normalizes one entry of a plugin's MCP file.
pub(crate) fn normalize(plugin: &str, original: &str, raw: &Value, vars: &Vars) -> PluginMcpServer {
    let mut server = PluginMcpServer {
        name: server_name(plugin, original),
        original_name: original.to_owned(),
        config: McpServerConfig::default(),
        error: None,
        warnings: Vec::new(),
    };
    let Some(raw) = raw.as_object() else {
        server.error = Some("the server entry is not an object".into());
        return server;
    };
    let mut missing = Vec::new();
    let c = &mut server.config;
    let kind = text(raw, &["type", "transport"]).map(str::to_ascii_lowercase);
    let has_command = raw.get("command").is_some_and(Value::is_string);
    let has_url = raw.get("url").is_some_and(Value::is_string);
    let transport = match kind.as_deref() {
        Some("stdio") => Ok("stdio"),
        Some("http" | "streamable-http" | "streamable_http" | "streamablehttp") => {
            Ok("streamable-http")
        }
        Some("sse") => Err(
            "uses the legacy SSE transport; dsh-codex speaks stdio and Streamable HTTP only"
                .to_owned(),
        ),
        Some(other) => Err(format!("unsupported transport \"{other}\"")),
        None if has_command => Ok("stdio"),
        None if has_url => Ok("streamable-http"),
        None => Err("needs a command (stdio) or a url (Streamable HTTP)".to_owned()),
    };
    match transport {
        Ok(t) => c.transport = Some(t.to_owned()),
        Err(e) => server.error = Some(e),
    }
    if let Some(enabled) = raw.get("enabled").and_then(Value::as_bool) {
        c.enabled = enabled;
    }
    if c.transport.as_deref() == Some("stdio") {
        match raw.get("command").and_then(Value::as_str) {
            Some(cmd) => c.command = Some(vars.path(vars.expand(cmd, &mut missing))),
            None => server.error = Some("stdio servers need a command".into()),
        }
        if let Some(Value::Array(args)) = raw.get("args") {
            c.args = args
                .iter()
                .map(|a| match a {
                    Value::String(s) => vars.expand(s, &mut missing),
                    other => other.to_string(),
                })
                .collect();
        }
        c.env = string_map(raw.get("env"), vars, &mut missing);
        for key in ["CLAUDE_PLUGIN_ROOT", "PLUGIN_ROOT"] {
            c.env
                .entry(key.to_owned())
                .or_insert_with(|| vars.root.display().to_string());
        }
        // Relative script paths in args resolve against the plugin folder.
        let cwd = raw
            .get("cwd")
            .and_then(Value::as_str)
            .map(|d| vars.expand(d, &mut missing))
            .map(|d| {
                let p = PathBuf::from(&d);
                if p.is_absolute() {
                    p
                } else {
                    vars.root.join(p)
                }
            })
            .unwrap_or_else(|| vars.root.to_path_buf());
        c.cwd = Some(cwd);
    } else if c.transport.as_deref() == Some("streamable-http") {
        match raw.get("url").and_then(Value::as_str) {
            Some(url) => c.url = Some(vars.expand(url, &mut missing)),
            None => server.error = Some("Streamable HTTP servers need a url".into()),
        }
        c.headers = string_map(
            field(raw, &["headers", "http_headers", "httpHeaders"]),
            vars,
            &mut missing,
        );
        if let Some(Value::Object(env_headers)) =
            field(raw, &["env_http_headers", "envHttpHeaders"])
        {
            for (header, var) in env_headers {
                let Some(var) = var.as_str() else { continue };
                match std::env::var(var).ok().filter(|v| !v.is_empty()) {
                    Some(v) => {
                        c.headers.insert(header.clone(), v);
                    }
                    None => missing.push(var.to_owned()),
                }
            }
        }
        c.bearer_token_env_var =
            text(raw, &["bearer_token_env_var", "bearerTokenEnvVar"]).map(str::to_owned);
        if let Some(Value::Object(oauth)) = raw.get("oauth") {
            let scopes = match field(oauth, &["scopes", "scope"]) {
                Some(Value::String(s)) => s.split_whitespace().map(str::to_owned).collect(),
                other => crate::manifest::strings(other),
            };
            c.oauth = Some(McpOAuthConfig {
                enabled: oauth
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                client_id: text(oauth, &["client_id", "clientId"]).map(str::to_owned),
                client_secret: text(oauth, &["client_secret", "clientSecret"]).map(str::to_owned),
                scopes,
            });
        }
    }
    let timeout_ms = field(raw, &["tool_call_timeout_ms", "toolCallTimeoutMs"])
        .and_then(Value::as_u64)
        .or_else(|| {
            field(raw, &["tool_timeout_sec", "toolTimeoutSec"])
                .and_then(Value::as_f64)
                .map(|s| (s * 1000.0) as u64)
        });
    if let Some(ms) = timeout_ms.filter(|ms| *ms > 0) {
        c.tool_call_timeout_ms = ms;
    }
    if !missing.is_empty() {
        server.warnings.push(format!(
            "environment variables not set: {}",
            missing.join(", ")
        ));
    }
    server
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_prefixed_and_bounded() {
        assert_eq!(server_name("linear", "linear"), "linear");
        assert_eq!(server_name("notes", "files"), "notes_files");
        assert_eq!(server_name("my plugin", "srv.v2"), "my-plugin_srv-v2");
        let long = server_name("a-very-long-plugin-name", "with-a-long-server-name");
        assert_eq!(long.len(), 32);
        assert!(long.starts_with("a-very-long-plugin-name_w"), "{long}");
        assert_ne!(
            long,
            server_name("a-very-long-plugin-name", "with-a-long-server-name-2")
        );
    }

    #[test]
    fn expands_plugin_and_environment_variables() {
        let vars = Vars {
            root: Path::new("/plug"),
            data: Path::new("/data"),
        };
        let mut missing = Vec::new();
        assert_eq!(
            vars.expand(
                "${CLAUDE_PLUGIN_ROOT}/bin:${PLUGIN_DATA}:${DSHX_SURELY_UNSET_VAR:-dflt}:${DSHX_SURELY_UNSET_2}",
                &mut missing
            ),
            "/plug/bin:/data:dflt:"
        );
        assert_eq!(missing, ["DSHX_SURELY_UNSET_2"]);
    }

    #[test]
    fn sse_is_reported() {
        let vars = Vars {
            root: Path::new("/plug"),
            data: Path::new("/data"),
        };
        let s = normalize(
            "p",
            "s",
            &json!({"type": "sse", "url": "https://x/sse"}),
            &vars,
        );
        assert!(s.error.unwrap().contains("SSE"));
    }
}
