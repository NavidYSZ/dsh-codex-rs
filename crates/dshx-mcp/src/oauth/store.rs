//! Token storage under `$DSHX_HOME/oauth/` (folder 0700, files 0600): one
//! file per authorization server issuer with its registered clients and the
//! tokens per MCP server URL, an index from server URL to issuer, and
//! manually entered bearer tokens (personal access tokens).
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A client registered at (or configured for) an authorization server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientRecord {
    pub redirect_uri: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// `none`, `client_secret_basic` or `client_secret_post`.
    pub auth_method: String,
    pub registered_at: u64,
}

/// Tokens of one MCP server plus what a refresh needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenRecord {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub token_type: String,
    /// Milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// RFC 8707 resource the tokens are bound to.
    pub resource: String,
    pub issuer: String,
    pub token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_endpoint: Option<String>,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub auth_method: String,
    pub obtained_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IssuerFile {
    pub issuer: String,
    #[serde(default)]
    pub clients: Vec<ClientRecord>,
    /// By MCP server URL.
    #[serde(default)]
    pub tokens: BTreeMap<String, TokenRecord>,
}

/// Distinct temp file names for concurrent writes.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Store {
    dir: PathBuf,
    /// Serializes read-modify-write cycles of this process.
    lock: Mutex<()>,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store {
            dir,
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn issuer_path(&self, issuer: &str) -> PathBuf {
        let host: String = url::Url::parse(issuer)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let hash: String = Sha256::digest(issuer.trim_end_matches('/').as_bytes())
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(format!("{host}-{hash}.json"))
    }

    fn issuer(&self, issuer: &str) -> IssuerFile {
        let mut file: IssuerFile = self.read(&self.issuer_path(issuer)).unwrap_or_default();
        if file.issuer.is_empty() {
            file.issuer = issuer.to_owned();
        }
        file
    }

    fn save_issuer(&self, file: &IssuerFile) -> std::io::Result<()> {
        self.write(&self.issuer_path(&file.issuer), file)
    }

    /// Server URL → issuer of its tokens.
    fn index(&self) -> BTreeMap<String, String> {
        self.read(&self.dir.join("index.json")).unwrap_or_default()
    }

    fn save_index(&self, index: &BTreeMap<String, String>) -> std::io::Result<()> {
        self.write(&self.dir.join("index.json"), index)
    }

    /// The manually entered bearer token of a server (personal access token).
    pub fn manual_token(&self, server_url: &str) -> Option<String> {
        let tokens: BTreeMap<String, String> =
            self.read(&self.dir.join("manual.json")).unwrap_or_default();
        tokens.get(server_url).cloned()
    }

    pub fn set_manual_token(&self, server_url: &str, token: Option<&str>) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        let path = self.dir.join("manual.json");
        let mut tokens: BTreeMap<String, String> = self.read(&path).unwrap_or_default();
        match token {
            Some(t) => tokens.insert(server_url.to_owned(), t.to_owned()),
            None => tokens.remove(server_url),
        };
        self.write(&path, &tokens)
    }

    /// A client registered for `redirect_uri` at `issuer`.
    pub fn client(&self, issuer: &str, redirect_uri: &str) -> Option<ClientRecord> {
        self.issuer(issuer)
            .clients
            .into_iter()
            .find(|c| c.redirect_uri == redirect_uri)
    }

    pub fn put_client(&self, issuer: &str, client: ClientRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.issuer(issuer);
        file.clients
            .retain(|c| c.redirect_uri != client.redirect_uri);
        file.clients.push(client);
        self.save_issuer(&file)
    }

    /// The stored tokens of a server.
    pub fn tokens(&self, server_url: &str) -> Option<TokenRecord> {
        let issuer = self.index().get(server_url)?.clone();
        self.issuer(&issuer).tokens.get(server_url).cloned()
    }

    pub fn put_tokens(&self, server_url: &str, record: TokenRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.issuer(&record.issuer);
        let mut index = self.index();
        index.insert(server_url.to_owned(), record.issuer.clone());
        file.tokens.insert(server_url.to_owned(), record);
        self.save_issuer(&file)?;
        self.save_index(&index)
    }

    pub fn remove_tokens(&self, server_url: &str) -> std::io::Result<Option<TokenRecord>> {
        let _guard = self.lock.lock().unwrap();
        let mut index = self.index();
        let Some(issuer) = index.remove(server_url) else {
            return Ok(None);
        };
        let mut file = self.issuer(&issuer);
        let removed = file.tokens.remove(server_url);
        self.save_issuer(&file)?;
        self.save_index(&index)?;
        Ok(removed)
    }

    fn read<T: DeserializeOwned>(&self, path: &Path) -> Option<T> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("ignoring unreadable {}: {e}", path.display());
                None
            }
        }
    }

    /// Writes owner-only: the folder 0700, the file 0600, atomically.
    fn write<T: Serialize>(&self, path: &Path, value: &T) -> std::io::Result<()> {
        create_private_dir(&self.dir)?;
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp-{}-{seq}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
