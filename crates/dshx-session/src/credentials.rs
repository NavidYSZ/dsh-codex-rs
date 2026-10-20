//! The DeepSeek API key file, `$DSHX_HOME/credentials.toml` (mode 0600):
//!
//! ```toml
//! [deepseek]
//! api_key = "sk-…"
//! ```
//!
//! Writes are atomic (temp file + rename) and owner-only; other tables in the
//! file are preserved.
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result, SessionError};
use crate::lock::create_private_dir;

/// Handle on one credentials file.
#[derive(Debug, Clone)]
pub struct Credentials {
    path: PathBuf,
}

impl Credentials {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Credentials { path: path.into() }
    }

    /// `$DSHX_HOME/credentials.toml`, default `~/.dsh-codex/credentials.toml`.
    pub fn default_path() -> PathBuf {
        dshx_core::config::home_dir().join("credentials.toml")
    }

    pub fn open_default() -> Self {
        Self::new(Self::default_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_table(&self) -> Result<toml::Table> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                tighten_mode(&self.path);
                text.parse::<toml::Table>()
                    .map_err(|e| SessionError::Corrupt(format!("{}: {e}", self.path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
            Err(e) => Err(SessionError::io(&self.path, e)),
        }
    }

    /// The stored DeepSeek API key, if any (empty strings count as unset).
    pub fn api_key(&self) -> Result<Option<String>> {
        let table = self.read_table()?;
        Ok(table
            .get("deepseek")
            .and_then(|d| d.get("api_key"))
            .and_then(toml::Value::as_str)
            .filter(|k| !k.trim().is_empty())
            .map(|k| k.trim().to_owned()))
    }

    /// Stores the key (trimmed); an empty key clears it.
    pub fn set_api_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.is_empty() {
            return self.clear_api_key();
        }
        let mut table = self.read_table()?;
        let section = table
            .entry("deepseek")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(section) = section.as_table_mut() else {
            return Err(SessionError::Corrupt(format!(
                "{}: [deepseek] is not a table",
                self.path.display()
            )));
        };
        section.insert("api_key".into(), toml::Value::String(key.to_owned()));
        self.write_table(&table)
    }

    /// Removes the key; deletes the file when nothing else is left in it.
    pub fn clear_api_key(&self) -> Result<()> {
        let mut table = self.read_table()?;
        if let Some(section) = table
            .get_mut("deepseek")
            .and_then(toml::Value::as_table_mut)
        {
            section.remove("api_key");
            if section.is_empty() {
                table.remove("deepseek");
            }
        }
        if table.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(SessionError::io(&self.path, e))
                }
                _ => Ok(()),
            };
        }
        self.write_table(&table)
    }

    fn write_table(&self, table: &toml::Table) -> Result<()> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        create_private_dir(dir)?;
        let text = toml::to_string(table).map_err(|e| SessionError::Corrupt(e.to_string()))?;
        let tmp = dir.join(format!(".credentials.{}.tmp", dshx_core::new_id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&tmp).and_then(|mut f| {
            f.write_all(text.as_bytes())?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(SessionError::io(&tmp, e));
        }
        std::fs::rename(&tmp, &self.path)
            .at(&self.path)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
    }
}

/// A key file readable by others is narrowed to 0600 (with a warning).
fn tighten_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(path = %path.display(), "credentials file was accessible to others; restricting to 0600");
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `DEEPSEEK_API_KEY` from the environment, else the credentials file.
pub fn resolve_api_key(credentials: &Credentials) -> Result<Option<String>> {
    if let Some(key) = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        return Ok(Some(key.trim().to_owned()));
    }
    credentials.api_key()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear_with_private_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let creds = Credentials::new(tmp.path().join("home/credentials.toml"));
        assert_eq!(creds.api_key().unwrap(), None);
        creds.set_api_key("  sk-test-123 ").unwrap();
        assert_eq!(creds.api_key().unwrap().as_deref(), Some("sk-test-123"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(creds.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(tmp.path().join("home"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        // Other tables survive edits.
        std::fs::write(
            creds.path(),
            "[other]\nx = 1\n[deepseek]\napi_key = \"sk-a\"\n",
        )
        .unwrap();
        creds.set_api_key("sk-b").unwrap();
        let text = std::fs::read_to_string(creds.path()).unwrap();
        assert!(text.contains("x = 1") && text.contains("sk-b"));
        creds.clear_api_key().unwrap();
        assert_eq!(creds.api_key().unwrap(), None);
        assert!(creds.path().exists());
        std::fs::write(creds.path(), "[deepseek]\napi_key = \"sk-a\"\n").unwrap();
        creds.set_api_key("").unwrap();
        assert!(!creds.path().exists());
        creds.clear_api_key().unwrap();
    }
}
