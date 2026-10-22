//! Choosing a project folder (docs/rust/SPEC.md §6): the subfolders of a
//! folder inside the allowed roots, git repositories worth suggesting, a new
//! folder, and on a Mac at its own screen the native "choose folder" dialog.
//!
//! The roots come from [`Backend::folder_roots`](crate::Backend::folder_roots);
//! without them `/fs/dirs`, `/fs/suggestions` and `/fs/mkdir` answer 404.
//! Errors carry a `code` the UI words: `not-found`, `not-a-folder`,
//! `outside-roots`, `permission-denied`, `exists`, `invalid`, `busy`,
//! `unsupported`.
use std::cmp::Reverse;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AppState, NotFound};

/// Folders listed at most; the UI says that there are more.
const MAX_DIRS: usize = 500;
/// Suggested repositories at most, newest first.
const MAX_REPOS: usize = 12;
/// Folders the repository scan looks into at most, and how deep.
const SCAN_BUDGET: usize = 4000;
const SCAN_DEPTH: usize = 3;
/// Never descended into by the scan (big, generated or not projects).
const SCAN_SKIP: &[&str] = &[
    "node_modules",
    "target",
    "Library",
    "vendor",
    "dist",
    "build",
];
/// macOS asks the user before an app may read these; a background scan must not.
const MAC_PROTECTED: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];
/// How long the native dialog may stay open.
const PICK_TIMEOUT: Duration = Duration::from_secs(600);

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/fs/dirs", get(dirs))
        .route("/fs/suggestions", get(suggestions))
        .route("/fs/mkdir", post(mkdir))
        .route("/fs/pick", post(pick))
}

/// Why a folder request failed.
#[derive(Debug)]
pub(crate) enum FsError {
    /// The backend offers no roots.
    Unsupported,
    NotFound(PathBuf),
    NotAFolder(PathBuf),
    Outside(PathBuf),
    Denied(PathBuf),
    Exists(PathBuf),
    Invalid(String),
    Busy,
    NoNativeDialog,
    Other(String),
}

impl FsError {
    fn io(e: std::io::Error, path: &Path) -> Self {
        match e.kind() {
            ErrorKind::NotFound => FsError::NotFound(path.into()),
            ErrorKind::PermissionDenied => FsError::Denied(path.into()),
            ErrorKind::AlreadyExists => FsError::Exists(path.into()),
            ErrorKind::NotADirectory => FsError::NotAFolder(path.into()),
            _ => FsError::Other(format!("{}: {e}", path.display())),
        }
    }
}

impl IntoResponse for FsError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            FsError::Unsupported => (
                StatusCode::NOT_FOUND,
                "unsupported",
                "folder browsing is not available".to_owned(),
            ),
            FsError::NotFound(p) => (
                StatusCode::NOT_FOUND,
                "not-found",
                format!("{} does not exist", p.display()),
            ),
            FsError::NotAFolder(p) => (
                StatusCode::BAD_REQUEST,
                "not-a-folder",
                format!("{} is not a folder", p.display()),
            ),
            FsError::Outside(p) => (
                StatusCode::FORBIDDEN,
                "outside-roots",
                format!("{} is outside the folders this server lists", p.display()),
            ),
            FsError::Denied(p) => (
                StatusCode::FORBIDDEN,
                "permission-denied",
                format!("{}: permission denied", p.display()),
            ),
            FsError::Exists(p) => (
                StatusCode::CONFLICT,
                "exists",
                format!("{} already exists", p.display()),
            ),
            FsError::Invalid(m) => (StatusCode::BAD_REQUEST, "invalid", m.clone()),
            FsError::Busy => (
                StatusCode::CONFLICT,
                "busy",
                "a folder dialog is already open".to_owned(),
            ),
            FsError::NoNativeDialog => (
                StatusCode::NOT_IMPLEMENTED,
                "unsupported",
                "no native folder dialog on this server".to_owned(),
            ),
            FsError::Other(m) => (StatusCode::BAD_REQUEST, "error", m.clone()),
        };
        (status, Json(json!({ "error": message, "code": code }))).into_response()
    }
}

type FsResult<T> = Result<T, FsError>;

/// The home folder of the server process, canonical.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .and_then(|h| PathBuf::from(h).canonicalize().ok())
}

/// Canonical, existing roots in the backend's order, without the ones inside
/// another root (those stay reachable from it).
pub(crate) fn normalize_roots(raw: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for root in raw {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if root.is_dir() && !found.contains(&root) {
            found.push(root);
        }
    }
    let all = found.clone();
    found.retain(|r| !all.iter().any(|o| o != r && r.starts_with(o)));
    found
}

fn root_of<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    roots.iter().find(|r| path.starts_with(r))
}

/// `raw` (absolute or `~/…`; empty = home, or the first root when home is
/// not allowed) as a canonical folder inside a root.
pub(crate) fn resolve(roots: &[PathBuf], home: Option<&Path>, raw: &str) -> FsResult<PathBuf> {
    let raw = raw.trim();
    let path = match raw {
        "" => {
            return home
                .filter(|h| root_of(roots, h).is_some())
                .map(Path::to_path_buf)
                .or_else(|| roots.first().cloned())
                .ok_or(FsError::Unsupported);
        }
        "~" => home.map(Path::to_path_buf).unwrap_or_default(),
        _ => match raw.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)).unwrap_or_default(),
            None => PathBuf::from(raw),
        },
    };
    if !path.is_absolute() {
        return Err(FsError::Invalid("the path must be absolute".into()));
    }
    let canonical = path.canonicalize().map_err(|e| FsError::io(e, &path))?;
    if root_of(roots, &canonical).is_none() {
        return Err(FsError::Outside(canonical));
    }
    if !canonical.is_dir() {
        return Err(FsError::NotAFolder(canonical));
    }
    Ok(canonical)
}

#[derive(Debug, Serialize)]
pub(crate) struct Entry {
    pub name: String,
    pub path: String,
    pub git: bool,
    pub hidden: bool,
}

/// Hidden by name or, on macOS, by the Finder's hidden flag (`~/Library`).
fn is_hidden(name: &str, path: &Path) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        const UF_HIDDEN: u32 = 0x8000;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            return meta.st_flags() & UF_HIDDEN != 0;
        }
    }
    let _ = path;
    false
}

fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// The subfolders of `dir`, sorted by name, at most [`MAX_DIRS`]; links count
/// when they lead to a folder inside a root. Returns whether more exist.
pub(crate) fn list(roots: &[PathBuf], dir: &Path, hidden: bool) -> FsResult<(Vec<Entry>, bool)> {
    let read = std::fs::read_dir(dir).map_err(|e| FsError::io(e, dir))?;
    let mut found: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in read.flatten() {
        // A name that is not UTF-8 could not come back as a path.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let folder = kind.is_dir()
            || (kind.is_symlink()
                && path
                    .canonicalize()
                    .is_ok_and(|t| t.is_dir() && root_of(roots, &t).is_some()));
        if !folder {
            continue;
        }
        let secret = is_hidden(&name, &path);
        if secret && !hidden {
            continue;
        }
        found.push((name, path, secret));
    }
    found.sort_by(|a, b| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    });
    let truncated = found.len() > MAX_DIRS;
    found.truncate(MAX_DIRS);
    let entries = found
        .into_iter()
        .map(|(name, path, hidden)| Entry {
            git: is_repo(&path),
            path: path.display().to_string(),
            name,
            hidden,
        })
        .collect();
    Ok((entries, truncated))
}

/// Creates `name` in `parent` (a folder inside a root) and returns it.
pub(crate) fn make_dir(
    roots: &[PathBuf],
    home: Option<&Path>,
    parent: &str,
    name: &str,
) -> FsResult<PathBuf> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || name.len() > 255
    {
        return Err(FsError::Invalid(format!(
            "{name:?} is not a valid folder name"
        )));
    }
    let parent = resolve(roots, home, parent)?;
    let path = parent.join(name);
    std::fs::create_dir(&path).map_err(|e| FsError::io(e, &path))?;
    Ok(path)
}

/// Git repositories in the first levels below the roots, most recently
/// worked on first. Hidden folders, build output and (on macOS) the folders
/// behind a privacy prompt are skipped.
pub(crate) fn repos(roots: &[PathBuf], home: Option<&Path>) -> Vec<(PathBuf, SystemTime)> {
    let mut found = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
        roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut budget = SCAN_BUDGET;
    while let Some((dir, depth)) = queue.pop_front() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') || SCAN_SKIP.contains(&name.as_str()) {
                continue;
            }
            if !entry.file_type().is_ok_and(|k| k.is_dir()) {
                continue;
            }
            let path = entry.path();
            if cfg!(target_os = "macos")
                && home == Some(dir.as_path())
                && MAC_PROTECTED.contains(&name.as_str())
            {
                continue;
            }
            if is_repo(&path) {
                found.push((path.clone(), last_work(&path)));
            } else if depth + 1 < SCAN_DEPTH {
                queue.push_back((path, depth + 1));
            }
        }
    }
    found.sort_by_key(|(_, t)| Reverse(*t));
    found.truncate(MAX_REPOS);
    found
}

/// When the repository last changed: its index or HEAD.
fn last_work(repo: &Path) -> SystemTime {
    ["index", "HEAD"]
        .iter()
        .filter_map(|f| std::fs::metadata(repo.join(".git").join(f)).ok())
        .filter_map(|m| m.modified().ok())
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

async fn roots(s: &AppState) -> FsResult<Vec<PathBuf>> {
    match s.backend.folder_roots().await {
        Ok(raw) => Ok(tokio::task::spawn_blocking(move || normalize_roots(raw))
            .await
            .unwrap_or_default()),
        Err(e) if e.downcast_ref::<NotFound>().is_some() => Err(FsError::Unsupported),
        Err(e) => Err(FsError::Other(format!("{e:#}"))),
    }
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

#[derive(Deserialize)]
struct DirsQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    hidden: Option<String>,
}

async fn dirs(State(s): State<AppState>, Query(q): Query<DirsQuery>) -> Response {
    let roots = match roots(&s).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let hidden = matches!(q.hidden.as_deref(), Some("1" | "true"));
    let result = tokio::task::spawn_blocking(move || -> FsResult<Value> {
        let home = home();
        let dir = resolve(&roots, home.as_deref(), &q.path)?;
        let (entries, truncated) = list(&roots, &dir, hidden)?;
        let root = root_of(&roots, &dir)
            .cloned()
            .unwrap_or_else(|| dir.clone());
        let parent = dir.parent().filter(|p| root_of(&roots, p).is_some());
        Ok(json!({
            "path": shown(&dir),
            "parent": parent.map(shown),
            "root": shown(&root),
            "home": home.filter(|h| root_of(&roots, h).is_some()).as_deref().map(shown),
            "roots": roots.iter().map(|r| shown(r)).collect::<Vec<_>>(),
            "git": is_repo(&dir),
            "entries": entries,
            "truncated": truncated,
        }))
    })
    .await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => FsError::Other(e.to_string()).into_response(),
    }
}

async fn suggestions(State(s): State<AppState>) -> Response {
    let roots = match roots(&s).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let found = tokio::task::spawn_blocking(move || repos(&roots, home().as_deref()))
        .await
        .unwrap_or_default();
    let repos: Vec<Value> = found
        .iter()
        .map(|(path, _)| {
            json!({
                "path": shown(path),
                "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            })
        })
        .collect();
    Json(json!({ "repos": repos })).into_response()
}

#[derive(Deserialize)]
struct MkdirBody {
    path: String,
    name: String,
}

async fn mkdir(State(s): State<AppState>, Json(body): Json<MkdirBody>) -> Response {
    let roots = match roots(&s).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let made = tokio::task::spawn_blocking(move || {
        make_dir(&roots, home().as_deref(), &body.path, &body.name)
    })
    .await;
    match made {
        Ok(Ok(path)) => {
            (StatusCode::CREATED, Json(json!({ "path": shown(&path) }))).into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => FsError::Other(e.to_string()).into_response(),
    }
}

/// True when someone sits at this Mac's screen: macOS, not started from an
/// SSH session (the server only listens on loopback, so the request comes
/// from this machine or through a tunnel of that session).
fn native_dialog() -> bool {
    let unset = |key: &str| std::env::var_os(key).is_none_or(|v| v.is_empty());
    cfg!(target_os = "macos")
        && unset("SSH_CONNECTION")
        && unset("SSH_TTY")
        && Path::new("/usr/bin/osascript").exists()
}

/// `osascript` lines; prompt and start folder arrive as arguments, never in the source.
const PICK_SCRIPT: &[&str] = &[
    "on run argv",
    "activate",
    "set promptText to item 1 of argv",
    "set startPath to item 2 of argv",
    "if startPath is \"\" then",
    "set chosen to choose folder with prompt promptText",
    "else",
    "set chosen to choose folder with prompt promptText default location (POSIX file startPath)",
    "end if",
    "return POSIX path of chosen",
    "end run",
];

/// The chosen folder from `osascript`'s exit status and output; `None` when cancelled.
pub(crate) fn pick_outcome(status: i32, stdout: &str, stderr: &str) -> FsResult<Option<String>> {
    if status != 0 {
        if stderr.contains("-128") || stderr.contains("User canceled") {
            return Ok(None);
        }
        return Err(FsError::Other(format!(
            "osascript failed: {}",
            stderr.trim()
        )));
    }
    let path = stdout.trim_end_matches(['\n', '\r']);
    let path = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(path.to_owned()))
}

static PICKING: AtomicBool = AtomicBool::new(false);

struct PickGuard;

impl Drop for PickGuard {
    fn drop(&mut self) {
        PICKING.store(false, Ordering::SeqCst);
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PickBody {
    prompt: Option<String>,
    default_path: Option<String>,
}

async fn pick(Json(body): Json<PickBody>) -> Response {
    if !native_dialog() {
        return FsError::NoNativeDialog.into_response();
    }
    if PICKING.swap(true, Ordering::SeqCst) {
        return FsError::Busy.into_response();
    }
    let _guard = PickGuard;
    let prompt = body
        .prompt
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "Choose a project folder".into());
    let start = body
        .default_path
        .map(|p| match p.strip_prefix("~/") {
            Some(rest) => home().map(|h| h.join(rest)).unwrap_or_default(),
            None => PathBuf::from(p),
        })
        .filter(|p| p.is_absolute() && p.is_dir())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut command = tokio::process::Command::new("/usr/bin/osascript");
    for line in PICK_SCRIPT {
        command.arg("-e").arg(line);
    }
    command
        .arg(prompt)
        .arg(start)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(PICK_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return FsError::Other(format!("osascript: {e}")).into_response(),
        Err(_) => return Json(json!({ "path": null })).into_response(),
    };
    match pick_outcome(
        output.status.code().unwrap_or(-1),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    ) {
        Ok(path) => Json(json!({ "path": path })).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in [
            "code/app/.git",
            "code/site",
            "code/Zeta",
            "code/alpha",
            "code/.cache",
            "notes",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("code/readme.md"), "x").unwrap();
        (tmp, root)
    }

    #[test]
    fn lists_only_folders_sorted_with_git_marks() {
        let (_tmp, root) = tree();
        let roots = vec![root.clone()];
        let (entries, truncated) = list(&roots, &root.join("code"), false).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "app", "site", "Zeta"]);
        assert!(entries[1].git && !entries[0].git);
        assert!(!truncated);
        let (all, _) = list(&roots, &root.join("code"), true).unwrap();
        assert!(all.iter().any(|e| e.name == ".cache" && e.hidden));
    }

    #[test]
    fn caps_long_listings() {
        let (_tmp, root) = tree();
        let many = root.join("many");
        for i in 0..(MAX_DIRS + 3) {
            std::fs::create_dir_all(many.join(format!("d{i:04}"))).unwrap();
        }
        let (entries, truncated) = list(std::slice::from_ref(&root), &many, false).unwrap();
        assert_eq!(entries.len(), MAX_DIRS);
        assert!(truncated);
    }

    #[test]
    fn resolves_inside_roots_only() {
        let (_tmp, root) = tree();
        let roots = vec![root.join("code")];
        let home = root.clone();
        assert_eq!(
            resolve(&roots, Some(&home), "~/code/app").unwrap(),
            root.join("code/app")
        );
        // Home is not a root here: the first root is the start.
        assert_eq!(resolve(&roots, Some(&home), "").unwrap(), root.join("code"));
        assert!(matches!(
            resolve(
                &roots,
                Some(&home),
                &root.join("notes").display().to_string()
            ),
            Err(FsError::Outside(_))
        ));
        assert!(matches!(
            resolve(
                &roots,
                Some(&home),
                &format!("{}/code/../notes", root.display())
            ),
            Err(FsError::Outside(_))
        ));
        assert!(matches!(
            resolve(
                &roots,
                Some(&home),
                &format!("{}/code/missing", root.display())
            ),
            Err(FsError::NotFound(_))
        ));
        assert!(matches!(
            resolve(
                &roots,
                Some(&home),
                &format!("{}/code/readme.md", root.display())
            ),
            Err(FsError::NotAFolder(_))
        ));
        assert!(matches!(
            resolve(&roots, Some(&home), "code"),
            Err(FsError::Invalid(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_roots_are_not_listed() {
        let (_tmp, root) = tree();
        let roots = vec![root.join("code")];
        std::os::unix::fs::symlink(root.join("notes"), root.join("code/outside")).unwrap();
        std::os::unix::fs::symlink(root.join("code/site"), root.join("code/inside")).unwrap();
        let (entries, _) = list(&roots, &root.join("code"), false).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"inside") && !names.contains(&"outside"),
            "{names:?}"
        );
    }

    #[test]
    fn nested_roots_collapse() {
        let (_tmp, root) = tree();
        let roots = normalize_roots(vec![
            root.join("code"),
            root.clone(),
            root.join("missing"),
            root.join("code/app"),
        ]);
        assert_eq!(roots, [root]);
    }

    #[test]
    fn makes_folders_with_plain_names_only() {
        let (_tmp, root) = tree();
        let roots = vec![root.clone()];
        let parent = root.join("code").display().to_string();
        let made = make_dir(&roots, None, &parent, "new one").unwrap();
        assert!(made.is_dir());
        assert!(matches!(
            make_dir(&roots, None, &parent, "new one"),
            Err(FsError::Exists(_))
        ));
        for bad in ["", "..", "a/b", "."] {
            assert!(
                matches!(
                    make_dir(&roots, None, &parent, bad),
                    Err(FsError::Invalid(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn finds_repositories_below_the_roots() {
        let (_tmp, root) = tree();
        std::fs::create_dir_all(root.join("deep/a/b/c/.git")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg/.git")).unwrap();
        let found = repos(std::slice::from_ref(&root), None);
        let paths: Vec<_> = found.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(paths, [root.join("code/app")]);
    }

    #[test]
    fn reads_the_dialog_outcome() {
        assert_eq!(
            pick_outcome(0, "/Users/alex/code/app/\n", "")
                .unwrap()
                .as_deref(),
            Some("/Users/alex/code/app")
        );
        assert_eq!(pick_outcome(0, "/\n", "").unwrap().as_deref(), Some("/"));
        assert_eq!(
            pick_outcome(1, "", "execution error: User canceled. (-128)").unwrap(),
            None
        );
        assert!(pick_outcome(1, "", "syntax error").is_err());
    }
}
