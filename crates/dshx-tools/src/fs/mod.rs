//! File tools (dsh `tool-fs` over `fs-local`, `fs-sandbox` and the
//! `fs-observation-policy`): `read`, `write`, `edit`, `read_image`.
//!
//! Paths resolve like dsh: a lexical absolute *display path* used in error
//! messages, and a realpath *target key* that identifies the file (and is the
//! path the model sees in successful results).
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use dshx_core::{SandboxMode, ToolContext, ToolError};

use crate::args::fail;
use crate::sandbox::{self, denial_marker, escalation_hint, writable_roots};

pub mod image;
pub mod lang;
pub mod mutate;
pub mod read;

/// A resolved file target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Absolute lexical spelling (used in error messages).
    pub display: String,
    /// Canonical identity: realpath, or the realpath of the nearest existing
    /// ancestor plus the missing suffix.
    pub key: PathBuf,
}

impl Target {
    /// The path successful results report (dsh `processPath`).
    pub fn process_path(&self) -> String {
        self.key.to_string_lossy().into_owned()
    }
}

/// Node `path.posix.basename` (trailing slashes ignored).
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "";
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Node `path.posix.dirname`.
fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(i) => {
            let parent = trimmed[..i].trim_end_matches('/');
            if parent.is_empty() {
                "/".into()
            } else {
                parent.to_owned()
            }
        }
    }
}

/// Lexical normalisation of an absolute path without `..` (Node `resolve`).
fn normalize(raw: &str) -> String {
    let parts: Vec<&str> = raw
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    format!("/{}", parts.join("/"))
}

fn has_parent_segment(raw: &str) -> bool {
    raw.split('/').any(|segment| segment == "..")
}

/// dsh `localDisplayPath`.
pub fn display_path(cwd: &Path, path: &str) -> String {
    let raw = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", cwd.to_string_lossy())
    };
    if has_parent_segment(&raw) {
        raw
    } else {
        normalize(&raw)
    }
}

fn is_not_dir(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOTDIR)
}

/// dsh `resolveLocalTarget`.
pub fn resolve(cwd: &Path, path: &str) -> Result<Target, ToolError> {
    if path.trim().is_empty() {
        return Err(fail("file_path must be a non-empty string"));
    }
    let display = display_path(cwd, path);
    let not_a_dir = || {
        fail(format!(
            "cannot resolve \"{display}\": a parent path segment is not a directory"
        ))
    };
    match std::fs::canonicalize(&display) {
        Ok(key) => return Ok(Target { display, key }),
        Err(error) if is_not_dir(&error) => return Err(not_a_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail(format!("cannot resolve \"{display}\": {error}"))),
    }
    let mut missing = vec![basename(&display).to_owned()];
    let mut ancestor = dirname(&display);
    loop {
        match std::fs::canonicalize(&ancestor) {
            Ok(real) => {
                if missing.iter().any(|m| m == "..") {
                    return Err(fail(format!(
                        "cannot resolve \"{display}\": parent traversal crosses a missing directory"
                    )));
                }
                let mut key = real;
                for segment in missing.iter().filter(|s| !s.is_empty() && *s != ".") {
                    key.push(segment);
                }
                return Ok(Target { display, key });
            }
            Err(error) if is_not_dir(&error) => return Err(not_a_dir()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = dirname(&ancestor);
                if parent == ancestor {
                    let key = PathBuf::from(&display);
                    return Ok(Target { display, key });
                }
                missing.insert(0, basename(&ancestor).to_owned());
                ancestor = parent;
            }
            Err(error) => return Err(fail(format!("cannot resolve \"{display}\": {error}"))),
        }
    }
}

/// What a stat found.
#[derive(Debug, Clone)]
pub struct Info {
    pub version: String,
    pub is_file: bool,
    pub size: u64,
    pub mode: u32,
}

/// `stat` following symlinks; `None` when absent (dsh `probe`).
pub fn probe(path: &Path) -> Result<Option<Info>, ToolError> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
            let ctime = i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec());
            Ok(Some(Info {
                version: format!(
                    "{}:{}:{}:{mtime}:{ctime}",
                    meta.dev(),
                    meta.ino(),
                    meta.size()
                ),
                is_file: meta.is_file(),
                size: meta.size(),
                mode: meta.mode() & 0o777,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound || is_not_dir(&error) => {
            Ok(None)
        }
        Err(error) => Err(fail(format!("cannot stat \"{}\": {error}", path.display()))),
    }
}

/// Serializes mutations per target (dsh `fs-local` `withLock`).
pub(crate) async fn lock_target(key: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let lock = {
        let mut locks = LOCKS.lock().unwrap();
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(key.to_path_buf()).or_default().clone()
    };
    lock.lock_owned().await
}

/// dsh `isPathUnder`: lexical containment, else an ancestor with the root's identity.
fn is_path_under(path: &Path, root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if path == root || path.starts_with(root) {
        return true;
    }
    let Ok(root_meta) = std::fs::metadata(root) else {
        return false;
    };
    let mut ancestor = Some(path);
    while let Some(current) = ancestor {
        if let Ok(meta) = std::fs::metadata(current)
            && meta.dev() == root_meta.dev()
            && meta.ino() == root_meta.ino()
        {
            return true;
        }
        ancestor = current.parent();
    }
    false
}

/// The in-process sandbox fence for mutations (dsh `fs-sandbox`): read-only
/// refuses, workspace-write requires the fresh target under a writable root.
/// Returns the fresh target to mutate.
pub(crate) fn checked_target(
    target: &Target,
    mode: SandboxMode,
    workspace_root: &Path,
) -> Result<Target, ToolError> {
    let denied = || {
        fail(format!(
            "{}\n{}",
            denial_marker(mode),
            escalation_hint("operation")
        ))
    };
    match mode {
        SandboxMode::DangerFullAccess => Ok(target.clone()),
        SandboxMode::ReadOnly => Err(denied()),
        SandboxMode::WorkspaceWrite => {
            let fresh = resolve(Path::new("/"), &target.display)?;
            let root = sandbox::canonical_path(workspace_root);
            if writable_roots(mode, &root)
                .iter()
                .any(|r| is_path_under(&fresh.key, r))
            {
                Ok(fresh)
            } else {
                Err(denied())
            }
        }
    }
}

/// The per-call sandbox mode for write/edit (dsh `FsSandboxController.resolvePolicy`).
pub(crate) async fn resolve_mode(
    ctx: &ToolContext,
    tool: &str,
    permissions: Option<&str>,
    justification: Option<&str>,
    file_path: &str,
) -> Result<SandboxMode, ToolError> {
    sandbox::validate_escalation_args(permissions, justification)?;
    match (permissions, justification) {
        (Some(requested), Some(justification)) => {
            let summary = format!("{tool} {file_path}");
            sandbox::approve_escalation(
                ctx,
                tool,
                "operation",
                requested,
                justification,
                ctx.sandbox,
                &summary,
            )
            .await
        }
        _ => Ok(ctx.sandbox),
    }
}

/// dsh `writeFileAtomic`: stage in a private sibling directory, fsync, keep
/// the old mode, then rename (or hard-link for create-only writes).
pub(crate) fn write_atomic(
    path: &Path,
    content: &[u8],
    mode: Option<u32>,
    create_only: bool,
    display: &str,
) -> Result<(), ToolError> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).map_err(|e| fail(format!("cannot write \"{display}\": {e}")))?;
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staging = dir.join(format!(
        ".{base}.{}.{}.tmpdir",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let io = |e: std::io::Error| fail(format!("cannot write \"{display}\": {e}"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(io)?;
    let result = (|| -> Result<(), ToolError> {
        let temp = staging.join(format!("{base}.tmp"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(io)?;
        file.write_all(content).map_err(io)?;
        file.sync_all().map_err(io)?;
        // New files get the usual 0644 (dsh would leave them 0600).
        let mode = mode.unwrap_or(0o644);
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(io)?;
        drop(file);
        if create_only {
            if let Err(error) = std::fs::hard_link(&temp, path) {
                return Err(match probe(path)? {
                    Some(info) if !info.is_file => {
                        fail(format!("cannot write \"{display}\": not a regular file"))
                    }
                    Some(_) => fail(format!(
                        "cannot modify \"{display}\": file has not been read — read the file, then retry"
                    )),
                    None if error.kind() == std::io::ErrorKind::AlreadyExists => fail(format!(
                        "cannot modify \"{display}\": file has not been read — read the file, then retry"
                    )),
                    None => io(error),
                });
            }
        } else {
            std::fs::rename(&temp, path).map_err(io)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// dsh `computeHunkDiffs`: one `{path, oldText, newText}` per hunk with three
/// lines of context (the UI's diff card).
pub fn hunk_diffs(path: &str, before: &str, after: &str) -> Vec<serde_json::Value> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(before, after);
    let mut diffs = Vec::new();
    for group in diff.grouped_ops(3) {
        let mut old_lines: Vec<String> = Vec::new();
        let mut new_lines: Vec<String> = Vec::new();
        for op in group {
            for change in diff.iter_changes(&op) {
                let text = change.value().strip_suffix('\n').unwrap_or(change.value());
                let text = text.strip_suffix('\r').unwrap_or(text).to_owned();
                match change.tag() {
                    ChangeTag::Delete => old_lines.push(text),
                    ChangeTag::Insert => new_lines.push(text),
                    ChangeTag::Equal => {
                        old_lines.push(text.clone());
                        new_lines.push(text);
                    }
                }
            }
        }
        let old_text = if old_lines.is_empty() {
            serde_json::Value::Null
        } else {
            old_lines.join("\n").into()
        };
        diffs.push(
            serde_json::json!({"path": path, "oldText": old_text, "newText": new_lines.join("\n")}),
        );
    }
    diffs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_paths_like_node() {
        let cwd = Path::new("/w");
        assert_eq!(display_path(cwd, "a.txt"), "/w/a.txt");
        assert_eq!(display_path(cwd, "./a//b/"), "/w/a/b");
        assert_eq!(display_path(cwd, "/abs/x"), "/abs/x");
        assert_eq!(display_path(cwd, "../x"), "/w/../x");
        assert_eq!(dirname("/a/b"), "/a");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(basename("/a/b/"), "b");
    }

    #[test]
    fn resolves_missing_targets_through_existing_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(tmp.path()).unwrap();
        let t = resolve(tmp.path(), "new/dir/file.txt").unwrap();
        assert_eq!(t.key, real.join("new/dir/file.txt"));
        std::fs::write(real.join("f"), "x").unwrap();
        let e = resolve(tmp.path(), "f/child").unwrap_err();
        assert!(
            e.to_string()
                .contains("a parent path segment is not a directory"),
            "{e}"
        );
    }

    #[test]
    fn hunks_with_context() {
        let d = hunk_diffs(
            "/p",
            "mode=DEBUG\nlevel=info\n",
            "mode=RELEASE\nlevel=info\n",
        );
        assert_eq!(
            d,
            vec![
                serde_json::json!({"path": "/p", "oldText": "mode=DEBUG\nlevel=info", "newText": "mode=RELEASE\nlevel=info"})
            ]
        );
    }
}
