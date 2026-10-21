//! Read-only file access for the UI's side panel: previews of files in a
//! chat's folder and a folder listing. Pages, images and PDFs load through
//! `/preview/<grant>/<path>`, a short-lived capability scoped to one folder,
//! so the API token never reaches a previewed page.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

const TTL: Duration = Duration::from_secs(2 * 3600);
/// Larger files are not served (videos and PDFs stay well below).
pub(crate) const MAX_FILE: u64 = 64 << 20;
const MAX_ENTRIES: usize = 2000;

struct Grant {
    root: PathBuf,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct Grants {
    map: Mutex<HashMap<String, Grant>>,
}

impl Grants {
    /// A grant for `root`; a live one is reused so preview URLs stay stable.
    pub(crate) fn grant(&self, root: &Path) -> String {
        let now = Instant::now();
        let mut map = self.map.lock().unwrap();
        map.retain(|_, g| g.expires > now);
        if let Some((key, g)) = map.iter_mut().find(|(_, g)| g.root == root) {
            g.expires = now + TTL;
            return key.clone();
        }
        let key = uuid::Uuid::new_v4().simple().to_string();
        map.insert(
            key.clone(),
            Grant {
                root: root.to_path_buf(),
                expires: now + TTL,
            },
        );
        key
    }

    pub(crate) fn root(&self, key: &str) -> Option<PathBuf> {
        let map = self.map.lock().unwrap();
        map.get(key)
            .filter(|g| g.expires > Instant::now())
            .map(|g| g.root.clone())
    }
}

/// `rel` inside `root` with symlinks resolved, or `None` when it leaves the
/// folder (`..`, absolute paths, links pointing outside) or does not exist.
pub(crate) fn resolve_inside(root: &Path, rel: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let rel = rel.trim_start_matches('/');
    if Path::new(rel)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let full = root.join(rel).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

/// Media type to serve, and whether the document can run code (HTML, SVG,
/// XML): those are sandboxed into an opaque origin.
pub(crate) fn content_type(path: &Path) -> (String, bool) {
    let mime = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    let active = matches!(
        mime,
        "text/html" | "application/xhtml+xml" | "image/svg+xml" | "text/xml" | "application/xml"
    );
    let textual = mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/javascript" | "application/xml" | "image/svg+xml"
        );
    let mime = if textual {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    };
    (mime, active)
}

#[derive(Serialize)]
pub(crate) struct Entry {
    name: String,
    /// Relative to the chat's folder, `/`-separated.
    path: String,
    kind: &'static str,
    size: u64,
}

/// Entries of `dir` (folders first, then by name), at most [`MAX_ENTRIES`].
pub(crate) fn list(root: &Path, dir: &Path) -> std::io::Result<(Vec<Entry>, bool)> {
    let root = root.canonicalize()?;
    let mut out = Vec::new();
    let mut truncated = false;
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        if out.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let meta = std::fs::metadata(item.path()).ok();
        let kind = match &meta {
            Some(m) if m.is_dir() => "dir",
            Some(_) => "file",
            None => "broken",
        };
        let rel = item.path();
        let rel = rel.strip_prefix(&root).unwrap_or(&rel);
        out.push(Entry {
            name,
            path: rel.to_string_lossy().replace('\\', "/"),
            kind,
            size: meta.map_or(0, |m| m.len()),
        });
    }
    out.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((out, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_inside_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "a").unwrap();
        std::fs::write(tmp.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("secret"), root.join("link")).unwrap();
        assert!(resolve_inside(&root, "sub/a.txt").is_some());
        assert!(
            resolve_inside(&root, "/sub/a.txt").is_some(),
            "leading slash is relative"
        );
        assert!(resolve_inside(&root, "../secret").is_none());
        assert!(resolve_inside(&root, "sub/../../secret").is_none());
        assert!(
            resolve_inside(&root, "link").is_none(),
            "links may not point outside"
        );
        assert!(resolve_inside(&root, "missing").is_none());
    }

    #[test]
    fn grants_are_reused_per_folder() {
        let g = Grants::default();
        let a = g.grant(Path::new("/x"));
        assert_eq!(g.grant(Path::new("/x")), a);
        assert_ne!(g.grant(Path::new("/y")), a);
        assert_eq!(g.root(&a).as_deref(), Some(Path::new("/x")));
        assert!(g.root("nope").is_none());
    }

    #[test]
    fn active_documents_are_marked() {
        assert!(content_type(Path::new("a.html")).1);
        assert!(content_type(Path::new("a.svg")).1);
        assert!(!content_type(Path::new("a.png")).1);
        assert_eq!(
            content_type(Path::new("a.css")).0,
            "text/css; charset=utf-8"
        );
    }
}
