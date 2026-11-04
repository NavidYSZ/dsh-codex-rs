//! `$DSHX_HOME/mobile/clients.json`: the phones paired with this server.
//! Only the SHA-256 of a token is kept. The file is re-read when its mtime or
//! size changed, because `dsh-codex pair` writes it while the server runs.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

/// Client tokens start with this; the rest is 32 random bytes, base64url.
pub const TOKEN_PREFIX: &str = "act_";

/// `lastSeenAt` is written at most this often per client.
const SEEN_INTERVAL_MS: u64 = 60_000;

/// One paired phone, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub token_sha256: String,
    pub created_at: u64,
    pub last_seen_at: Option<u64>,
    /// `ui`, `cli` or `ssh`.
    pub paired_via: String,
}

impl Client {
    /// The row the API shows: without the hash; `current` marks the client
    /// whose token made the request.
    pub fn view(&self, current: bool) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "platform": self.platform,
            "createdAt": self.created_at,
            "lastSeenAt": self.last_seen_at,
            "pairedVia": self.paired_via,
            "current": current,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    clients: Vec<Client>,
}

/// Modification time and size of the file when it was last read or written.
type Stamp = (Option<SystemTime>, u64);

#[derive(Default)]
struct Cache {
    clients: Vec<Client>,
    stamp: Option<Stamp>,
    /// Per client: when `lastSeenAt` was last written.
    seen: HashMap<String, u64>,
}

/// The client list with its file.
pub struct ClientStore {
    path: PathBuf,
    cache: Mutex<Cache>,
}

impl ClientStore {
    pub fn new(home: &Path) -> Self {
        ClientStore {
            path: home.join("mobile").join("clients.json"),
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn stamp(&self) -> Option<Stamp> {
        let meta = std::fs::metadata(&self.path).ok()?;
        Some((meta.modified().ok(), meta.len()))
    }

    /// Reloads the file when another process changed, created or removed it.
    fn refresh(&self, cache: &mut Cache) {
        let stamp = self.stamp();
        if cache.stamp == stamp {
            return;
        }
        cache.clients = match std::fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice::<FileFormat>(&bytes) {
                Ok(file) => file.clients,
                Err(e) => {
                    tracing::warn!("{}: {e}; no client is accepted", self.path.display());
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        cache.stamp = stamp;
    }

    /// Writes the list (folder 0700, file 0600, atomically) and remembers the
    /// file's new stamp so the write does not count as a foreign change.
    fn write(&self, cache: &mut Cache) -> anyhow::Result<()> {
        let dir = self.path.parent().expect("clients.json lives in a folder");
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = FileFormat {
            version: 1,
            clients: cache.clients.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        super::write_private(&tmp, &serde_json::to_vec_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        cache.stamp = self.stamp();
        Ok(())
    }

    pub fn list(&self) -> Vec<Client> {
        let mut cache = self.cache.lock().unwrap();
        self.refresh(&mut cache);
        cache.clients.clone()
    }

    pub fn get(&self, id: &str) -> Option<Client> {
        self.list().into_iter().find(|c| c.id == id)
    }

    /// A new client with a fresh token; the token is returned once and never
    /// stored.
    pub fn create(
        &self,
        name: &str,
        platform: &str,
        paired_via: &str,
    ) -> anyhow::Result<(Client, String)> {
        let (token, token_sha256) = new_token();
        let client = Client {
            id: format!("cli_{}", uuid::Uuid::new_v4().simple()),
            name: name.to_owned(),
            platform: platform.to_owned(),
            token_sha256,
            created_at: dshx_core::now_ms(),
            last_seen_at: None,
            paired_via: paired_via.to_owned(),
        };
        let mut cache = self.cache.lock().unwrap();
        self.refresh(&mut cache);
        cache.clients.push(client.clone());
        self.write(&mut cache)?;
        Ok((client, token))
    }

    /// `None` when there is no such client.
    pub fn rename(&self, id: &str, name: &str) -> anyhow::Result<Option<Client>> {
        let mut cache = self.cache.lock().unwrap();
        self.refresh(&mut cache);
        let Some(client) = cache.clients.iter_mut().find(|c| c.id == id) else {
            return Ok(None);
        };
        client.name = name.to_owned();
        let renamed = client.clone();
        self.write(&mut cache)?;
        Ok(Some(renamed))
    }

    /// Drops the client; `false` when there was none.
    pub fn revoke(&self, id: &str) -> anyhow::Result<bool> {
        let mut cache = self.cache.lock().unwrap();
        self.refresh(&mut cache);
        let before = cache.clients.len();
        cache.clients.retain(|c| c.id != id);
        if cache.clients.len() == before {
            return Ok(false);
        }
        cache.seen.remove(id);
        self.write(&mut cache)?;
        Ok(true)
    }

    /// The id of the client whose token this is. Writes `lastSeenAt` at most
    /// once a minute per client.
    pub fn authenticate(&self, token: &str) -> Option<String> {
        if !token.starts_with(TOKEN_PREFIX) {
            return None;
        }
        let presented = token_sha256(token);
        let mut cache = self.cache.lock().unwrap();
        self.refresh(&mut cache);
        // Every hash is compared so timing does not say which one matched.
        let mut found = None;
        for (i, client) in cache.clients.iter().enumerate() {
            if crate::constant_time_eq(client.token_sha256.as_bytes(), presented.as_bytes()) {
                found = Some(i);
            }
        }
        let i = found?;
        let id = cache.clients[i].id.clone();
        let now = dshx_core::now_ms();
        let due = cache
            .seen
            .get(&id)
            .is_none_or(|last| now.saturating_sub(*last) >= SEEN_INTERVAL_MS);
        if due {
            cache.clients[i].last_seen_at = Some(now);
            cache.seen.insert(id.clone(), now);
            if let Err(e) = self.write(&mut cache) {
                tracing::warn!("{}: {e:#}", self.path.display());
            }
        }
        Some(id)
    }
}

/// `act_` + 32 random bytes (base64url, no padding), and its SHA-256 in hex.
pub fn new_token() -> (String, String) {
    use base64::Engine as _;
    let bytes: [u8; 32] = rand::random();
    let token = format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    );
    let hash = token_sha256(&token);
    (token, hash)
}

/// Lowercase hex SHA-256 of a token: what the file keeps.
pub fn token_sha256(token: &str) -> String {
    hex(&sha2::Sha256::digest(token.as_bytes()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn create_hash_list_and_revoke() {
        let home = tempfile::tempdir().unwrap();
        let store = ClientStore::new(home.path());
        assert!(store.list().is_empty());
        let (client, token) = store.create("iPhone", "ios", "ui").unwrap();
        assert!(token.starts_with("act_"));
        assert_eq!(token.len(), 4 + 43, "32 bytes base64url without padding");
        assert!(!token.contains('='));
        assert_eq!(client.token_sha256, token_sha256(&token));
        assert_eq!(client.token_sha256.len(), 64);
        assert!(client.id.starts_with("cli_"));
        assert_eq!(store.list(), vec![client.clone()]);
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains(&token), "the token itself is never stored");
        assert!(text.contains("\"tokenSha256\""));
        #[cfg(unix)]
        {
            assert_eq!(mode(store.path()), 0o600);
            assert_eq!(mode(store.path().parent().unwrap()), 0o700);
        }
        assert_eq!(
            store.authenticate(&token).as_deref(),
            Some(client.id.as_str())
        );
        assert_eq!(store.authenticate("act_nope"), None);
        assert_eq!(store.authenticate("secret"), None);
        let seen = store.get(&client.id).unwrap().last_seen_at;
        assert!(seen.is_some(), "the first request sets lastSeenAt");
        assert_eq!(
            store
                .rename(&client.id, "Navid's iPhone")
                .unwrap()
                .unwrap()
                .name,
            "Navid's iPhone"
        );
        assert!(store.rename("cli_missing", "x").unwrap().is_none());
        assert!(store.revoke(&client.id).unwrap());
        assert!(!store.revoke(&client.id).unwrap());
        assert!(store.list().is_empty());
        assert_eq!(store.authenticate(&token), None);
    }

    #[test]
    fn reloads_when_another_process_wrote_the_file() {
        let home = tempfile::tempdir().unwrap();
        let server = ClientStore::new(home.path());
        let cli = ClientStore::new(home.path());
        assert!(server.list().is_empty());
        let (a, token_a) = cli.create("A", "ios", "cli").unwrap();
        assert_eq!(server.list(), vec![a.clone()], "a new file is picked up");
        assert_eq!(
            server.authenticate(&token_a).as_deref(),
            Some(a.id.as_str())
        );
        let (b, token_b) = cli.create("B", "ios", "cli").unwrap();
        assert_eq!(server.list().len(), 2, "a changed file is re-read");
        assert_eq!(
            server.authenticate(&token_b).as_deref(),
            Some(b.id.as_str())
        );
        assert!(cli.revoke(&a.id).unwrap());
        assert_eq!(
            server.authenticate(&token_a),
            None,
            "a revocation elsewhere counts"
        );
        std::fs::remove_file(cli.path()).unwrap();
        assert!(server.list().is_empty(), "a missing file means no clients");
    }

    #[test]
    fn last_seen_is_written_once_a_minute() {
        let home = tempfile::tempdir().unwrap();
        let store = ClientStore::new(home.path());
        let (client, token) = store.create("iPhone", "ios", "ui").unwrap();
        store.authenticate(&token).unwrap();
        let first = std::fs::read(store.path()).unwrap();
        let seen = store.get(&client.id).unwrap().last_seen_at.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.authenticate(&token).unwrap();
        assert_eq!(
            std::fs::read(store.path()).unwrap(),
            first,
            "not written again within a minute"
        );
        assert_eq!(store.get(&client.id).unwrap().last_seen_at, Some(seen));
    }
}
