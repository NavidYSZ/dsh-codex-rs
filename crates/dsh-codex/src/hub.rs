//! The Aether accounts hub (docs/aether/SPEC.md §5): `dsh-codex hub enroll`
//! registers this server with an enrol code from the app, keeps the server
//! key in `credentials.toml` (`[hub] server_key`) and the hub's address in
//! `[codex.hub] url`; `dsh-codex web` then keeps the relay connection that
//! lets the app reach this server through the hub.
use std::path::Path;
use std::sync::Arc;

use dshx_core::config::Config;
use dshx_hub::relay_client::{self, RelayConfig, RelayStatus, ServerInfo};
use dshx_server::mobile::identity;
use dshx_session::Credentials;
use serde_json::{Value, json};

/// The table in `credentials.toml` that holds the server key.
const TABLE: &str = "hub";

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Register this server with a hub, using the enrol code the Aether app
    /// shows; stores the key and switches the relay on (`dsh-codex web`
    /// picks it up at its next start).
    Enroll {
        /// The hub, e.g. https://hub.example (or http://127.0.0.1:8787).
        #[arg(long)]
        url: String,
        /// The enrol code from the app (ABCD-1234, valid 10 minutes).
        #[arg(long)]
        code: String,
        /// How the server appears in the app (default: the computer name).
        #[arg(long)]
        name: Option<String>,
    },
}

pub async fn run(command: Command) -> anyhow::Result<i32> {
    match command {
        Command::Enroll { url, code, name } => enroll(&url, &code, name.as_deref()).await,
    }
}

async fn enroll(url: &str, code: &str, name: Option<&str>) -> anyhow::Result<i32> {
    let home = dshx_core::config::home_dir();
    let before = Config::load()?;
    let url = url.trim().trim_end_matches('/').to_owned();
    let mut server = server_info(&home, &before)?;
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        server.name = name.to_owned();
    }
    let (enrolled, key) = relay_client::enroll(&url, code, &server).await?;
    let credentials = Credentials::new(home.join("credentials.toml"));
    set_server_key(&credentials, &key)?;
    let mut after = before.clone();
    after.codex.hub.url = url.clone();
    crate::config_file::write(&home, &before, &after)?;
    eprintln!(
        "dsh-codex: enrolled as {:?} ({}) with {url}; the key is in {}. \
         Restart `dsh-codex web` to connect the relay.",
        enrolled.name,
        enrolled.id,
        credentials.path().display()
    );
    Ok(0)
}

/// The stored server key, if this server is enrolled.
pub fn server_key(credentials: &Credentials) -> Option<String> {
    credentials
        .table(TABLE)
        .ok()
        .flatten()?
        .get("server_key")?
        .as_str()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
}

fn set_server_key(credentials: &Credentials, key: &str) -> anyhow::Result<()> {
    let mut table = credentials.table(TABLE)?.unwrap_or_default();
    table.insert("server_key".into(), toml::Value::String(key.to_owned()));
    credentials.set_table(TABLE, Some(table))?;
    Ok(())
}

/// The identity the app knows from pairing (`server.json`, the computer
/// name), as the hub wants it.
fn server_info(home: &Path, config: &Config) -> anyhow::Result<ServerInfo> {
    let s = identity::server_info(home, &config.codex.mobile.name, env!("CARGO_PKG_VERSION"))?;
    Ok(ServerInfo {
        id: Some(s.id),
        name: s.name,
        hostname: s.hostname,
        platform: s.platform,
        version: s.version,
    })
}

/// The relay task of `dsh-codex web`, when `[codex.hub] url` and the key are
/// set: it answers the hub's requests from the loopback listener on `port`
/// with the start token. Aborted when dropped.
pub struct Relay {
    pub status: Arc<RelayStatus>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Relay {
    pub fn start(
        home: &Path,
        config: &Config,
        credentials: &Credentials,
        port: u16,
        token: &str,
    ) -> Option<Relay> {
        let url = config.codex.hub.url.trim();
        if url.is_empty() {
            return None;
        }
        let Some(server_key) = server_key(credentials) else {
            eprintln!(
                "aether: [codex.hub] url is set but no server key; run `dsh-codex hub enroll`"
            );
            return None;
        };
        let server = match server_info(home, config) {
            Ok(server) => server,
            Err(e) => {
                eprintln!("aether: the hub relay did not start: {e:#}");
                return None;
            }
        };
        let status = Arc::new(RelayStatus::default());
        let config = RelayConfig {
            hub_url: url.trim_end_matches('/').to_owned(),
            server_key,
            local_base: format!("http://127.0.0.1:{port}"),
            local_token: token.to_owned(),
            server,
        };
        eprintln!("aether: relay to {}", config.hub_url);
        let task = tokio::spawn(relay_client::run_with_status(config, status.clone()));
        Some(Relay { status, task })
    }
}

/// `/api/info.hub`: `{url, enrolled, connected}` (and `error` while the
/// relay is down for a reason).
pub fn info(config: &Config, credentials: &Credentials, relay: Option<&Relay>) -> Value {
    let mut out = json!({
        "url": config.codex.hub.url,
        "enrolled": server_key(credentials).is_some(),
        "connected": relay.is_some_and(|r| r.status.connected()),
    });
    if let Some(error) = relay.and_then(|r| r.status.last_error()) {
        out["error"] = json!(error);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_key_lives_beside_the_api_key() {
        let tmp = tempfile::tempdir().unwrap();
        let credentials = Credentials::new(tmp.path().join("credentials.toml"));
        credentials.set_api_key("sk-test").unwrap();
        let mut config = Config::default();
        assert_eq!(
            info(&config, &credentials, None),
            json!({"url": "", "enrolled": false, "connected": false})
        );
        assert!(Relay::start(tmp.path(), &config, &credentials, 1, "t").is_none());

        set_server_key(&credentials, "ask_abc").unwrap();
        assert_eq!(server_key(&credentials).as_deref(), Some("ask_abc"));
        assert_eq!(credentials.api_key().unwrap().as_deref(), Some("sk-test"));
        config.codex.hub.url = "http://127.0.0.1:9".into();
        assert_eq!(
            info(&config, &credentials, None),
            json!({"url": "http://127.0.0.1:9", "enrolled": true, "connected": false})
        );
    }
}
