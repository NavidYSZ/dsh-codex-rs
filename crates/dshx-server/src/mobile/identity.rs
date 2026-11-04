//! Who this server is: a stable id in `$DSHX_HOME/server.json`, the name it
//! introduces itself with, and the host name.
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// `server` of `/api/info` and of a pairing payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub version: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    id: String,
    created_at: u64,
}

/// The server's id (`srv_<uuid>`), made on first read (file mode 0600). The
/// app keys its devices by it, so pairing again replaces instead of adding.
pub fn server_id(home: &Path) -> anyhow::Result<String> {
    let path = home.join("server.json");
    if let Ok(bytes) = std::fs::read(&path)
        && let Ok(identity) = serde_json::from_slice::<Identity>(&bytes)
        && identity.id.starts_with("srv_")
    {
        return Ok(identity.id);
    }
    let identity = Identity {
        id: format!("srv_{}", uuid::Uuid::new_v4().simple()),
        created_at: dshx_core::now_ms(),
    };
    std::fs::create_dir_all(home)?;
    let mut bytes = serde_json::to_vec_pretty(&identity)?;
    bytes.push(b'\n');
    super::write_private(&path, &bytes)?;
    Ok(identity.id)
}

/// `[codex.mobile] name`, else the computer's name (macOS), else the host name.
pub fn server_name(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_owned();
    }
    computer_name().unwrap_or_else(hostname)
}

/// The short host name (`mbp-480`), read once.
pub fn hostname() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let name = output_of(Command::new("hostname"))
            .or_else(|| std::env::var("HOSTNAME").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "localhost".into());
        name.split('.')
            .next()
            .unwrap_or("localhost")
            .trim()
            .to_owned()
    })
    .clone()
}

/// macOS: `scutil --get ComputerName` ("MacBook Pro"), read once; `None`
/// elsewhere or when it fails.
fn computer_name() -> Option<String> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let mut cmd = Command::new("scutil");
        cmd.args(["--get", "ComputerName"]);
        output_of(cmd)
    })
    .clone()
}

fn output_of(mut cmd: Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Everything a payload or `/api/info` says about this server.
pub fn server_info(
    home: &Path,
    configured_name: &str,
    version: &str,
) -> anyhow::Result<ServerInfo> {
    Ok(ServerInfo {
        id: server_id(home)?,
        name: server_name(configured_name),
        hostname: hostname(),
        platform: std::env::consts::OS.to_owned(),
        version: version.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_id_is_made_once_and_kept_private() {
        let home = tempfile::tempdir().unwrap();
        let id = server_id(home.path()).unwrap();
        assert!(id.starts_with("srv_"));
        assert_eq!(id.len(), 4 + 32);
        assert_eq!(server_id(home.path()).unwrap(), id, "stable across reads");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(home.path().join("server.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let text = std::fs::read_to_string(home.path().join("server.json")).unwrap();
        assert!(text.contains("\"createdAt\""));
    }

    #[test]
    fn configured_name_wins() {
        assert_eq!(server_name("  Studio  "), "Studio");
        assert!(!server_name("").is_empty());
        assert!(!hostname().contains('.'));
    }
}
