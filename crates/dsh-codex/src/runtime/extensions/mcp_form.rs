//! The MCP page's server form: secret values of config servers and the
//! merge of a form into a server's `[mcp_servers.<name>]` table.
//!
//! Environment values, header values and bearer tokens entered on the page
//! live in `credentials.toml` (mode 0600), never in `config.toml`:
//!
//! ```toml
//! [mcp_servers.github]
//! bearer_token = "ghp_…"
//! [mcp_servers.github.env]
//! GITHUB_TOKEN = "…"
//! [mcp_servers.github.headers]
//! X-Api-Key = "…"
//! ```
//!
//! A server runs with its config table plus these values
//! ([`ServerSecrets::apply`]); the page only ever sees their names.
use std::collections::{BTreeMap, BTreeSet};

use dshx_core::config::McpServerConfig;
use dshx_session::Credentials;
use serde_json::{Map, Value};

/// The table of `credentials.toml` that holds them.
const TABLE: &str = "mcp_servers";

/// Secret values of one config server.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ServerSecrets {
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub bearer_token: Option<String>,
}

fn strings(value: Option<&toml::Value>) -> BTreeMap<String, String> {
    value
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn table_of(map: &BTreeMap<String, String>) -> toml::Value {
    toml::Value::Table(
        map.iter()
            .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
            .collect(),
    )
}

impl ServerSecrets {
    fn from_toml(table: &toml::Table) -> Self {
        ServerSecrets {
            env: strings(table.get("env")),
            headers: strings(table.get("headers")),
            bearer_token: table
                .get("bearer_token")
                .and_then(toml::Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
        }
    }

    fn to_toml(&self) -> toml::Table {
        let mut table = toml::Table::new();
        if let Some(token) = &self.bearer_token {
            table.insert("bearer_token".into(), toml::Value::String(token.clone()));
        }
        if !self.env.is_empty() {
            table.insert("env".into(), table_of(&self.env));
        }
        if !self.headers.is_empty() {
            table.insert("headers".into(), table_of(&self.headers));
        }
        table
    }

    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.headers.is_empty() && self.bearer_token.is_none()
    }

    /// The config a server runs with: the secret values over its table; a
    /// bearer token becomes the `Authorization` header.
    pub fn apply(&self, config: &McpServerConfig) -> McpServerConfig {
        let mut config = config.clone();
        config
            .env
            .extend(self.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        config
            .headers
            .extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
        if let Some(token) = &self.bearer_token {
            config
                .headers
                .retain(|k, _| !k.eq_ignore_ascii_case("authorization"));
            config
                .headers
                .insert("Authorization".into(), format!("Bearer {token}"));
        }
        config
    }
}

/// Every server's secrets; an unreadable file counts as empty (logged).
pub(crate) fn load(credentials: &Credentials) -> BTreeMap<String, ServerSecrets> {
    match credentials.table(TABLE) {
        Ok(table) => table
            .unwrap_or_default()
            .iter()
            .filter_map(|(name, v)| Some((name.clone(), ServerSecrets::from_toml(v.as_table()?))))
            .collect(),
        Err(e) => {
            tracing::warn!("MCP secrets unreadable: {e}");
            BTreeMap::new()
        }
    }
}

/// Stores (or with `None`/empty removes) one server's secrets.
pub(crate) fn save(
    credentials: &Credentials,
    name: &str,
    secrets: Option<&ServerSecrets>,
) -> anyhow::Result<()> {
    let mut table = credentials.table(TABLE)?.unwrap_or_default();
    match secrets.filter(|s| !s.is_empty()) {
        Some(s) => {
            table.insert(name.to_owned(), toml::Value::Table(s.to_toml()));
        }
        None => {
            table.remove(name);
        }
    }
    credentials.set_table(TABLE, Some(table))?;
    Ok(())
}

/// Names and values of a form table: a string sets the value, `null`
/// keeps the stored one.
type Patch = BTreeMap<String, Option<String>>;

/// The body of the add, update and test routes:
/// `{config, secrets?: {env?, headers?, bearerToken?}}`. `env` and
/// `headers` name every variable or header the server keeps; a missing
/// table leaves them as they are. `bearerToken`: a string sets it, `""`
/// removes it, `null` or missing keeps it.
#[derive(Debug, Default)]
pub(crate) struct ServerForm {
    config: Map<String, Value>,
    env: Option<Patch>,
    headers: Option<Patch>,
    bearer_token: Option<String>,
}

/// Keys of a server table that the form owns: the transport and how to
/// reach and authenticate the server. Everything else (timeouts,
/// reconnect, OAuth client, per-chat settings) stays as configured.
const FORM_KEYS: [&str; 7] = [
    "transport",
    "command",
    "args",
    "cwd",
    "env_vars",
    "url",
    "bearer_token_env_var",
];

fn patch(value: Option<&Value>, what: &str) -> anyhow::Result<Option<Patch>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                anyhow::ensure!(!k.trim().is_empty(), "{what}: empty name");
                let value = match v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => anyhow::bail!("{what}.{k}: expected a string"),
                };
                Ok((k.trim().to_owned(), value))
            })
            .collect::<anyhow::Result<Patch>>()
            .map(Some),
        Some(_) => anyhow::bail!("{what} must be an object"),
    }
}

impl ServerForm {
    pub fn parse(body: &Value) -> anyhow::Result<ServerForm> {
        let mut config = match body.get("config") {
            Some(Value::Object(map)) => map.clone(),
            Some(Value::Null) | None => Map::new(),
            Some(_) => anyhow::bail!("config must be an object"),
        };
        let secrets = match body.get("secrets") {
            Some(Value::Object(map)) => map.clone(),
            Some(Value::Null) | None => Map::new(),
            Some(_) => anyhow::bail!("secrets must be an object"),
        };
        let mut env = patch(secrets.get("env"), "secrets.env")?;
        let mut headers = patch(secrets.get("headers"), "secrets.headers")?;
        // Values written into the table itself (pasted JSON) become secrets too.
        for (key, target) in [("env", &mut env), ("headers", &mut headers)] {
            if let Some(given) = patch(config.remove(key).as_ref(), key)? {
                let target = target.get_or_insert_with(Patch::new);
                for (k, v) in given {
                    target.entry(k).or_insert(v);
                }
            }
        }
        if let Some(alias) = config.remove("http_headers") {
            let target = headers.get_or_insert_with(Patch::new);
            for (k, v) in patch(Some(&alias), "http_headers")?.unwrap_or_default() {
                target.entry(k).or_insert(v);
            }
        }
        let bearer_token = match secrets.get("bearerToken") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.trim().to_owned()),
            Some(_) => anyhow::bail!("secrets.bearerToken must be a string"),
        };
        Ok(ServerForm {
            config,
            env,
            headers,
            bearer_token,
        })
    }

    /// The new table and secrets of a server: this form over the current
    /// ones (`None` for a new server).
    pub fn merge(
        &self,
        current: Option<(&McpServerConfig, &ServerSecrets)>,
    ) -> anyhow::Result<(McpServerConfig, ServerSecrets)> {
        let empty = (McpServerConfig::default(), ServerSecrets::default());
        let (old, old_secrets) = current.unwrap_or((&empty.0, &empty.1));
        let mut table = match serde_json::to_value(old)? {
            Value::Object(map) if current.is_some() => map,
            _ => Map::new(),
        };
        let reachable = self.config.contains_key("command") || self.config.contains_key("url");
        if reachable {
            for key in FORM_KEYS {
                table.remove(key);
            }
        }
        for (key, value) in &self.config {
            if key == "oauth" {
                // Keep a registered client and scopes; the form only switches it.
                let mut oauth = match table.remove("oauth") {
                    Some(Value::Object(map)) => map,
                    _ => Map::new(),
                };
                if let Value::Object(given) = value {
                    oauth.extend(given.clone());
                }
                table.insert("oauth".into(), Value::Object(oauth));
            } else if !value.is_null() {
                table.insert(key.clone(), value.clone());
            }
        }
        table.remove("env");
        table.remove("headers");
        let mut config: McpServerConfig = serde_json::from_value(Value::Object(table))
            .map_err(|e| anyhow::anyhow!("invalid server config: {e}"))?;
        config.env = old.env.clone();
        config.headers = old.headers.clone();
        let mut secrets = old_secrets.clone();
        merge_values(
            &mut config.env,
            &mut secrets.env,
            self.env.as_ref(),
            "environment variable",
        )?;
        merge_values(
            &mut config.headers,
            &mut secrets.headers,
            self.headers.as_ref(),
            "header",
        )?;
        match self.bearer_token.as_deref() {
            Some("") => secrets.bearer_token = None,
            Some(token) => secrets.bearer_token = Some(token.to_owned()),
            None => {}
        }
        // What the other transport needed goes with the switch.
        if config.url.is_some() {
            config.env.clear();
            config.env_vars.clear();
            config.cwd = None;
            secrets.env.clear();
        } else {
            config.headers.clear();
            config.bearer_token_env_var = None;
            secrets.headers.clear();
            secrets.bearer_token = None;
        }
        Ok((config, secrets))
    }
}

/// Applies a form table to the plain values of the config and the secret
/// ones: new values become secrets (and leave the config), `null` keeps a
/// stored value, names the form no longer lists go from both.
fn merge_values(
    plain: &mut BTreeMap<String, String>,
    secret: &mut BTreeMap<String, String>,
    patch: Option<&Patch>,
    what: &str,
) -> anyhow::Result<()> {
    let Some(patch) = patch else {
        return Ok(());
    };
    let known: BTreeSet<String> = plain.keys().chain(secret.keys()).cloned().collect();
    for (name, value) in patch {
        match value {
            Some(v) => {
                plain.remove(name);
                secret.insert(name.clone(), v.clone());
            }
            None => anyhow::ensure!(known.contains(name), "the {what} {name} needs a value"),
        }
    }
    plain.retain(|k, _| patch.contains_key(k));
    secret.retain(|k, _| patch.contains_key(k));
    Ok(())
}

/// Names of a server's variables or headers, plain and secret, sorted.
pub(crate) fn names<'a>(
    plain: &'a BTreeMap<String, String>,
    secret: &'a BTreeMap<String, String>,
) -> Vec<&'a String> {
    let all: BTreeSet<&String> = plain.keys().chain(secret.keys()).collect();
    all.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secrets_round_trip_through_credentials_toml() {
        let dir = tempfile::tempdir().unwrap();
        let creds = Credentials::new(dir.path().join("credentials.toml"));
        creds.set_api_key("sk-deepseek").unwrap();
        let github = ServerSecrets {
            env: BTreeMap::from([("GITHUB_TOKEN".into(), "ghp_1".into())]),
            headers: BTreeMap::new(),
            bearer_token: None,
        };
        let remote = ServerSecrets {
            headers: BTreeMap::from([("X-Api-Key".into(), "k".into())]),
            bearer_token: Some("tok".into()),
            ..Default::default()
        };
        save(&creds, "github", Some(&github)).unwrap();
        save(&creds, "remote", Some(&remote)).unwrap();
        let all = load(&creds);
        assert_eq!(all["github"], github);
        assert_eq!(all["remote"], remote);
        let text = std::fs::read_to_string(creds.path()).unwrap();
        assert!(text.contains("[mcp_servers.github.env]"), "{text}");
        assert!(text.contains("sk-deepseek"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(creds.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        save(&creds, "github", None).unwrap();
        save(&creds, "remote", Some(&ServerSecrets::default())).unwrap();
        assert!(load(&creds).is_empty());
        // The DeepSeek key is untouched.
        assert_eq!(creds.api_key().unwrap().as_deref(), Some("sk-deepseek"));
    }

    #[test]
    fn apply_puts_the_bearer_token_into_the_authorization_header() {
        let config = McpServerConfig {
            url: Some("https://mcp.example/mcp".into()),
            headers: BTreeMap::from([("authorization".into(), "old".into())]),
            ..Default::default()
        };
        let secrets = ServerSecrets {
            bearer_token: Some("tok".into()),
            ..Default::default()
        };
        let run = secrets.apply(&config);
        assert_eq!(
            run.headers,
            BTreeMap::from([("Authorization".into(), "Bearer tok".into())])
        );
        assert!(!dshx_mcp::uses_oauth("x", &run));
    }

    #[test]
    fn a_new_stdio_server_keeps_its_values_out_of_the_config() {
        let form = ServerForm::parse(&json!({
            "config": {"command": "npx", "args": ["-y", "server-github"], "env_vars": ["HOME_TOKEN"],
                       "cwd": "/tmp/w"},
            "secrets": {"env": {"GITHUB_TOKEN": "ghp_secret"}, "headers": {}, "bearerToken": ""},
        }))
        .unwrap();
        let (config, secrets) = form.merge(None).unwrap();
        assert_eq!(config.command.as_deref(), Some("npx"));
        assert_eq!(config.args, ["-y", "server-github"]);
        assert_eq!(config.env_vars, ["HOME_TOKEN"]);
        assert!(config.env.is_empty());
        assert_eq!(secrets.env["GITHUB_TOKEN"], "ghp_secret");
        let toml = toml::to_string(&config).unwrap();
        assert!(!toml.contains("ghp_secret"), "{toml}");
        // Pasted JSON with values in the table itself.
        let pasted = ServerForm::parse(&json!({
            "config": {"command": "uvx", "env": {"API_KEY": "abc"}},
        }))
        .unwrap();
        let (config, secrets) = pasted.merge(None).unwrap();
        assert!(config.env.is_empty());
        assert_eq!(secrets.env["API_KEY"], "abc");
    }

    #[test]
    fn an_update_keeps_masked_values_and_other_settings() {
        let current = McpServerConfig {
            command: Some("old".into()),
            env: BTreeMap::from([("PLAIN".into(), "p".into()), ("GONE".into(), "g".into())]),
            tool_call_timeout_ms: 5_000,
            ..Default::default()
        };
        let stored = ServerSecrets {
            env: BTreeMap::from([("TOKEN".into(), "t1".into()), ("OLD".into(), "o".into())]),
            ..Default::default()
        };
        let form = ServerForm::parse(&json!({
            "config": {"command": "new", "args": []},
            "secrets": {"env": {"PLAIN": null, "TOKEN": null, "FRESH": "f"}},
        }))
        .unwrap();
        let (config, secrets) = form.merge(Some((&current, &stored))).unwrap();
        assert_eq!(config.command.as_deref(), Some("new"));
        assert_eq!(config.tool_call_timeout_ms, 5_000);
        assert_eq!(config.env, BTreeMap::from([("PLAIN".into(), "p".into())]));
        assert_eq!(
            secrets.env,
            BTreeMap::from([("FRESH".into(), "f".into()), ("TOKEN".into(), "t1".into())])
        );
        // A typed value replaces a plain one and moves it to the secrets.
        let typed = ServerForm::parse(&json!({"config": {"command": "new"},
            "secrets": {"env": {"PLAIN": "p2"}}}))
        .unwrap();
        let (config, secrets) = typed.merge(Some((&current, &stored))).unwrap();
        assert!(config.env.is_empty());
        assert_eq!(secrets.env, BTreeMap::from([("PLAIN".into(), "p2".into())]));
        // Keeping a name nobody stored is an error.
        let unknown = ServerForm::parse(&json!({"config": {"command": "x"},
            "secrets": {"env": {"NEW": null}}}))
        .unwrap();
        let err = unknown
            .merge(Some((&current, &stored)))
            .unwrap_err()
            .to_string();
        assert!(err.contains("NEW needs a value"), "{err}");
    }

    #[test]
    fn switching_to_http_drops_the_stdio_values_and_keeps_the_oauth_client() {
        let current = McpServerConfig {
            command: Some("srv".into()),
            env_vars: vec!["X".into()],
            oauth: Some(dshx_core::config::McpOAuthConfig {
                client_id: Some("client".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let stored = ServerSecrets {
            env: BTreeMap::from([("TOKEN".into(), "t".into())]),
            ..Default::default()
        };
        let form = ServerForm::parse(&json!({
            "config": {"url": "https://mcp.example/mcp", "oauth": {"enabled": false}},
            "secrets": {"headers": {"X-Team": "core"}, "bearerToken": "tok"},
        }))
        .unwrap();
        let (config, secrets) = form.merge(Some((&current, &stored))).unwrap();
        assert_eq!(config.command, None);
        assert!(config.env_vars.is_empty());
        let oauth = config.oauth.unwrap();
        assert!(!oauth.enabled);
        assert_eq!(oauth.client_id.as_deref(), Some("client"));
        assert!(secrets.env.is_empty());
        assert_eq!(secrets.headers["X-Team"], "core");
        assert_eq!(secrets.bearer_token.as_deref(), Some("tok"));
        // `null` keeps the token, "" removes it.
        let keep = ServerForm::parse(&json!({"config": {"url": "https://mcp.example/mcp"},
            "secrets": {"bearerToken": null}}))
        .unwrap();
        let config = McpServerConfig {
            url: Some("https://mcp.example/mcp".into()),
            ..Default::default()
        };
        assert_eq!(
            keep.merge(Some((&config, &secrets)))
                .unwrap()
                .1
                .bearer_token
                .as_deref(),
            Some("tok")
        );
        let drop = ServerForm::parse(&json!({"config": {"url": "https://mcp.example/mcp"},
            "secrets": {"bearerToken": ""}}))
        .unwrap();
        assert_eq!(
            drop.merge(Some((&config, &secrets)))
                .unwrap()
                .1
                .bearer_token,
            None
        );
    }
}
