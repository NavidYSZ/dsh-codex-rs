//! Folder transfer between instances (docs/aether/TRANSFER.md): the
//! manifest of a project folder, its export as a tar+zstd stream, the import
//! of such a stream, and deleting entries or the whole folder after a sync or
//! a move. The client that sees both instances carries the data.
//!
//! Every path must lie inside the allowed roots: the home folder, every saved
//! project folder and the parent of each; canonical paths decide and `/` is
//! never allowed. Walking and deleting never follow symlinks, and an import
//! never writes through one. Errors carry a `code`: `unsupported`,
//! `invalid`, `outside-roots`, `not-found`, `not-empty`, `rejected` (an
//! archive entry was refused), `failed`; the last two report what was
//! written before the stop.
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::AppState;

/// What the transfer routes need from the instance; `Backend::transfer`
/// hands it out (`None` = the routes answer 404).
#[async_trait]
pub trait TransferHost: Send + Sync + 'static {
    /// The user's home folder.
    fn home(&self) -> Option<PathBuf>;
    /// The project folders of `GET /api/workspaces`, most recently used first.
    async fn workspaces(&self) -> anyhow::Result<Vec<PathBuf>>;
    /// Drops a folder from the saved project folders; unknown ones are fine.
    async fn remove_workspace(&self, path: &Path) -> anyhow::Result<()>;
}

/// Request bodies of manifest, export and delete may list many paths.
const LIST_LIMIT: usize = 64 << 20;
/// Bytes per chunk of the export stream.
const CHUNK: usize = 256 << 10;
/// zstd level: fast enough that a small VPS keeps up with the tunnel.
const ZSTD_LEVEL: i32 = 3;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/transfer/roots", get(roots))
        .route("/transfer/manifest", post(manifest))
        .route("/transfer/export", post(export))
        .route("/transfer/import", post(import))
        .route("/transfer/delete", post(delete))
        .route("/transfer/remove", post(remove))
        .layer(DefaultBodyLimit::max(LIST_LIMIT))
}

#[derive(Debug)]
pub(crate) enum TransferError {
    Unsupported,
    Invalid(String),
    Outside(PathBuf),
    NotFound(PathBuf),
    NotEmpty(PathBuf),
    /// An import or delete stopped part-way.
    Stopped {
        code: &'static str,
        message: String,
        written: u64,
        bytes: u64,
    },
    Io(String),
}

impl TransferError {
    fn io(path: &Path, e: io::Error) -> Self {
        match e.kind() {
            ErrorKind::NotFound => TransferError::NotFound(path.into()),
            _ => TransferError::Io(format!("{}: {e}", path.display())),
        }
    }
}

impl IntoResponse for TransferError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            TransferError::Unsupported => (
                StatusCode::NOT_FOUND,
                json!({"code": "unsupported", "error": "folder transfer is not available"}),
            ),
            TransferError::Invalid(m) => (
                StatusCode::BAD_REQUEST,
                json!({"code": "invalid", "error": m}),
            ),
            TransferError::Outside(p) => (
                StatusCode::BAD_REQUEST,
                json!({"code": "outside-roots", "error": format!(
                    "{} is outside the home folder and the project folders", p.display()
                )}),
            ),
            TransferError::NotFound(p) => (
                StatusCode::NOT_FOUND,
                json!({"code": "not-found", "error": format!("{} does not exist", p.display())}),
            ),
            TransferError::NotEmpty(p) => (
                StatusCode::CONFLICT,
                json!({"code": "not-empty", "error": format!(
                    "{} already exists and is not empty", p.display()
                )}),
            ),
            TransferError::Stopped {
                code,
                message,
                written,
                bytes,
            } => (
                StatusCode::BAD_REQUEST,
                json!({"code": code, "error": message, "written": written, "bytes": bytes}),
            ),
            TransferError::Io(m) => (
                StatusCode::BAD_REQUEST,
                json!({"code": "failed", "error": m}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

type TransferResult<T> = Result<T, TransferError>;

fn host(state: &AppState) -> TransferResult<Arc<dyn TransferHost>> {
    state.backend.transfer().ok_or(TransferError::Unsupported)
}

fn is_fs_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn push_unique(list: &mut Vec<PathBuf>, path: &Path) {
    if !list.iter().any(|p| p == path) {
        list.push(path.to_path_buf());
    }
}

/// The allowed roots of one request, all canonical.
struct Roots {
    home: Option<PathBuf>,
    /// Home, then each project folder after its parent.
    all: Vec<PathBuf>,
    /// Home and the project parents: folders `remove` never deletes.
    containers: Vec<PathBuf>,
    default_parent: Option<PathBuf>,
}

impl Roots {
    async fn load(host: &dyn TransferHost) -> TransferResult<Self> {
        let home = host
            .home()
            .and_then(|h| h.canonicalize().ok())
            .filter(|h| h.is_dir() && !is_fs_root(h));
        let workspaces = host
            .workspaces()
            .await
            .map_err(|e| TransferError::Io(format!("{e:#}")))?;
        let mut all = Vec::new();
        let mut containers = Vec::new();
        if let Some(h) = &home {
            push_unique(&mut all, h);
            push_unique(&mut containers, h);
        }
        let mut default_parent = None;
        for w in workspaces {
            let Ok(w) = w.canonicalize() else { continue };
            if !w.is_dir() || is_fs_root(&w) {
                continue;
            }
            if let Some(parent) = w.parent().filter(|p| !is_fs_root(p)) {
                default_parent.get_or_insert_with(|| parent.to_path_buf());
                push_unique(&mut all, parent);
                push_unique(&mut containers, parent);
            }
            push_unique(&mut all, &w);
        }
        let default_parent = default_parent.or_else(|| home.clone());
        Ok(Roots {
            home,
            all,
            containers,
            default_parent,
        })
    }

    fn allows(&self, canonical: &Path) -> bool {
        !is_fs_root(canonical) && self.all.iter().any(|r| canonical.starts_with(r))
    }

    /// `raw` (absolute or `~/…`) without `..`; not canonicalized yet.
    fn absolute(&self, raw: &str) -> TransferResult<PathBuf> {
        let raw = raw.trim();
        let path = match (raw, raw.strip_prefix("~/"), &self.home) {
            ("~", _, Some(home)) => home.clone(),
            (_, Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(raw),
        };
        if !path.is_absolute() {
            return Err(TransferError::Invalid(format!(
                "{raw:?}: the path must be absolute"
            )));
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(TransferError::Invalid(format!(
                "{raw:?}: `..` is not allowed"
            )));
        }
        Ok(path)
    }

    /// An existing folder inside the roots, canonical.
    fn folder(&self, raw: &str) -> TransferResult<PathBuf> {
        let path = self.absolute(raw)?;
        let canonical = path
            .canonicalize()
            .map_err(|e| TransferError::io(&path, e))?;
        if is_fs_root(&canonical) {
            return Err(TransferError::Invalid("refusing to work on /".into()));
        }
        if !self.allows(&canonical) {
            return Err(TransferError::Outside(canonical));
        }
        if !canonical.is_dir() {
            return Err(TransferError::Invalid(format!(
                "{} is not a folder",
                canonical.display()
            )));
        }
        Ok(canonical)
    }
}

/// A relative entry path (`a/b/c`, no `..`, not absolute) as its components;
/// leading `./` and repeated slashes are tolerated.
fn components(raw: &[u8]) -> Result<Vec<OsString>, String> {
    let shown = String::from_utf8_lossy(raw);
    if raw.contains(&0) {
        return Err(format!("{shown:?}: NUL in a path"));
    }
    let mut out = Vec::new();
    for c in Path::new(OsStr::from_bytes(raw)).components() {
        match c {
            Component::Normal(n) => out.push(n.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => return Err(format!("{shown:?}: `..` is not allowed")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{shown:?}: absolute paths are not allowed"));
            }
        }
    }
    if out.is_empty() {
        return Err(format!("{shown:?}: empty path"));
    }
    Ok(out)
}

// ---------------------------------------------------------------- walking

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Symlink,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
        }
    }
}

/// One manifest entry: a file, a symlink or an empty folder.
#[derive(Debug)]
struct Entry {
    rel: String,
    kind: Kind,
    size: u64,
    mtime_ms: i64,
    mode: u32,
    target: Option<String>,
}

impl Entry {
    fn new(rel: String, kind: Kind, meta: &fs::Metadata, target: Option<String>) -> Self {
        Entry {
            rel,
            kind,
            size: if kind == Kind::File { meta.len() } else { 0 },
            mtime_ms: meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000,
            mode: meta.mode() & 0o7777,
            target,
        }
    }

    fn to_json(&self) -> Value {
        let mut v = json!({ "path": self.rel, "kind": self.kind.name(), "mtime": self.mtime_ms });
        match self.kind {
            Kind::File => {
                v["size"] = json!(self.size);
                v["mode"] = json!(self.mode);
            }
            Kind::Dir => v["mode"] = json!(self.mode),
            Kind::Symlink => v["target"] = json!(self.target),
        }
        v
    }
}

/// Exclude rules: a bare name matches that path component anywhere, a
/// name with `/` matches that relative path and everything below it.
struct Excludes {
    names: HashSet<String>,
    paths: Vec<String>,
}

impl Excludes {
    fn new(raw: &[String]) -> Self {
        let mut names = HashSet::new();
        let mut paths = Vec::new();
        for e in raw {
            let e = e.trim().trim_matches('/');
            if e.is_empty() {
                continue;
            }
            if e.contains('/') {
                paths.push(e.to_owned());
            } else {
                names.insert(e.to_owned());
            }
        }
        Excludes { names, paths }
    }

    fn matches(&self, name: &str, rel: &str) -> bool {
        self.names.contains(name)
            || self.paths.iter().any(|p| {
                rel == p || (rel.starts_with(p.as_str()) && rel.as_bytes()[p.len()] == b'/')
            })
    }
}

#[derive(Default)]
struct Walk {
    entries: Vec<Entry>,
    excluded: u64,
    /// Sockets, devices, FIFOs and names that are not UTF-8.
    skipped: u64,
}

/// Files, symlinks and empty folders below `root`, sorted, never following
/// a symlink. A folder whose entries are all excluded counts as empty.
fn walk(root: &Path, excludes: &Excludes) -> TransferResult<Walk> {
    let mut out = Walk::default();
    let root_meta = fs::symlink_metadata(root).map_err(|e| TransferError::io(root, e))?;
    let mut stack = vec![(root.to_path_buf(), String::new(), root_meta)];
    while let Some((dir, rel, dir_meta)) = stack.pop() {
        let listing = fs::read_dir(&dir).map_err(|e| TransferError::io(&dir, e))?;
        let mut kept = 0usize;
        for item in listing {
            let item = item.map_err(|e| TransferError::io(&dir, e))?;
            let name = item.file_name();
            let Some(name) = name.to_str() else {
                out.skipped += 1;
                continue;
            };
            let child = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            if excludes.matches(name, &child) {
                out.excluded += 1;
                continue;
            }
            let path = item.path();
            // DirEntry::metadata does not follow symlinks.
            let meta = match item.metadata() {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(TransferError::io(&path, e)),
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                let target = fs::read_link(&path).map_err(|e| TransferError::io(&path, e))?;
                let Some(target) = target.to_str() else {
                    out.skipped += 1;
                    continue;
                };
                let target = target.to_owned();
                out.entries
                    .push(Entry::new(child, Kind::Symlink, &meta, Some(target)));
            } else if ft.is_dir() {
                stack.push((path, child, meta));
            } else if ft.is_file() {
                out.entries.push(Entry::new(child, Kind::File, &meta, None));
            } else {
                out.skipped += 1;
                continue;
            }
            kept += 1;
        }
        if kept == 0 && !rel.is_empty() {
            out.entries
                .push(Entry::new(rel, Kind::Dir, &dir_meta, None));
        }
    }
    out.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

// ---------------------------------------------------------------- routes

#[derive(Debug, Deserialize)]
struct FolderRequest {
    path: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    files: Option<Vec<String>>,
}

async fn roots(State(state): State<AppState>) -> TransferResult<Json<Value>> {
    let roots = Roots::load(host(&state)?.as_ref()).await?;
    Ok(Json(json!({
        "home": roots.home,
        "roots": roots.all,
        "defaultParent": roots.default_parent,
    })))
}

async fn manifest(
    State(state): State<AppState>,
    Json(req): Json<FolderRequest>,
) -> TransferResult<Json<Value>> {
    let roots = Roots::load(host(&state)?.as_ref()).await?;
    let root = roots.folder(&req.path)?;
    let excludes = Excludes::new(&req.excludes);
    let walk_root = root.clone();
    let found = tokio::task::spawn_blocking(move || walk(&walk_root, &excludes))
        .await
        .map_err(|e| TransferError::Io(e.to_string()))??;
    let total: u64 = found.entries.iter().map(|e| e.size).sum();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Json(json!({
        "root": root,
        "name": name,
        "count": found.entries.len(),
        "totalBytes": total,
        "excluded": found.excluded,
        "skipped": found.skipped,
        "files": found.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
    })))
}

/// `io::Write` into the response body; a gone client stops the export.
struct ChannelWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        self.tx
            .blocking_send(Ok(Bytes::from(chunk)))
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "the client went away"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

async fn export(
    State(state): State<AppState>,
    Json(req): Json<FolderRequest>,
) -> TransferResult<Response> {
    let roots = Roots::load(host(&state)?.as_ref()).await?;
    let root = roots.folder(&req.path)?;
    let excludes = Excludes::new(&req.excludes);
    let walk_root = root.clone();
    let mut found = tokio::task::spawn_blocking(move || walk(&walk_root, &excludes))
        .await
        .map_err(|e| TransferError::Io(e.to_string()))??;
    if let Some(files) = &req.files {
        let wanted: HashSet<&str> = files.iter().map(|f| f.trim_end_matches('/')).collect();
        found.entries.retain(|e| wanted.contains(e.rel.as_str()));
    }
    let count = found.entries.len();
    let total: u64 = found.entries.iter().map(|e| e.size).sum();
    let (tx, mut rx) = mpsc::channel::<io::Result<Bytes>>(8);
    tokio::task::spawn_blocking(move || {
        let writer = ChannelWriter {
            tx: tx.clone(),
            buf: Vec::with_capacity(CHUNK),
        };
        if let Err(e) = write_archive(&root, &found.entries, writer) {
            tracing::warn!("transfer export of {} stopped: {e}", root.display());
            let _ = tx.blocking_send(Err(e));
        }
    });
    let body = Body::from_stream(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)));
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar+zstd".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (
                header::HeaderName::from_static("x-transfer-count"),
                count.to_string(),
            ),
            (
                header::HeaderName::from_static("x-transfer-bytes"),
                total.to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

/// The entries as a tar stream (GNU headers, mtimes in seconds) inside zstd.
fn write_archive(root: &Path, entries: &[Entry], out: impl Write) -> io::Result<()> {
    let encoder = zstd::Encoder::new(out, ZSTD_LEVEL)?;
    let mut tar = tar::Builder::new(encoder);
    for e in entries {
        let mut h = tar::Header::new_gnu();
        h.set_mode(e.mode);
        h.set_mtime(e.mtime_ms.div_euclid(1000).max(0) as u64);
        match e.kind {
            Kind::File => append_file(&mut tar, root, e, h)?,
            Kind::Dir => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_size(0);
                tar.append_data(&mut h, &e.rel, io::empty())?;
            }
            Kind::Symlink => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                let target = e.target.as_deref().unwrap_or_default();
                tar.append_link(&mut h, &e.rel, target)?;
            }
        }
    }
    let mut out = tar.into_inner()?.finish()?;
    out.flush()
}

/// One regular file as it is now; a file that turned into something else
/// since the walk (or vanished) is left out.
fn append_file<W: Write>(
    tar: &mut tar::Builder<W>,
    root: &Path,
    e: &Entry,
    mut h: tar::Header,
) -> io::Result<()> {
    let path = root.join(&e.rel);
    let before = match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => m,
        Ok(_) => return Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!("{}: {err}", path.display()),
            ));
        }
    };
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!("{}: {err}", path.display()),
            ));
        }
    };
    let meta = file.metadata()?;
    // Swapped for a symlink between lstat and open: not the walked file.
    if meta.ino() != before.ino() || meta.dev() != before.dev() || !meta.is_file() {
        return Ok(());
    }
    let len = meta.len();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(len);
    h.set_mode(meta.mode() & 0o7777);
    h.set_mtime(meta.mtime().max(0) as u64);
    // Exactly `len` bytes even if the file changes while it is read.
    let data = (&file).take(len).chain(io::repeat(0)).take(len);
    tar.append_data(&mut h, &e.rel, data)
}

#[derive(Debug, Deserialize)]
struct ImportQuery {
    path: String,
    #[serde(default)]
    mode: Option<String>,
}

/// `io::Read` over the request body, fed by an async pump.
struct ChannelReader {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    current: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.current.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.current = chunk,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.current.len());
        out[..n].copy_from_slice(&self.current[..n]);
        self.current = self.current.slice(n..);
        Ok(n)
    }
}

async fn import(
    State(state): State<AppState>,
    Query(query): Query<ImportQuery>,
    body: Body,
) -> TransferResult<Json<Value>> {
    let fresh = match query.mode.as_deref() {
        None | Some("fresh") => true,
        Some("merge") => false,
        Some(other) => {
            return Err(TransferError::Invalid(format!(
                "mode must be merge or fresh, not {other:?}"
            )));
        }
    };
    let roots = Roots::load(host(&state)?.as_ref()).await?;
    let target = prepare_target(&roots, &query.path, fresh)?;
    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(16);
    let pump = tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(io::Error::other);
            let failed = chunk.is_err();
            if tx.send(chunk).await.is_err() || failed {
                break;
            }
        }
    });
    let reader = ChannelReader {
        rx,
        current: Bytes::new(),
    };
    let dest = target.clone();
    let outcome = tokio::task::spawn_blocking(move || unpack(&dest, reader)).await;
    pump.abort();
    let stats = outcome.map_err(|e| TransferError::Io(e.to_string()))??;
    Ok(Json(json!({
        "path": target,
        "written": stats.written,
        "bytes": stats.bytes,
        "skipped": stats.skipped,
    })))
}

/// The destination folder of an import, created when missing. Its parent
/// must exist inside the roots; the folder itself must not be a symlink.
fn prepare_target(roots: &Roots, raw: &str, fresh: bool) -> TransferResult<PathBuf> {
    let path = roots.absolute(raw)?;
    let (Some(name), Some(parent)) = (path.file_name(), path.parent()) else {
        return Err(TransferError::Invalid(
            "the target needs a folder name".into(),
        ));
    };
    let parent = parent
        .canonicalize()
        .map_err(|e| TransferError::io(parent, e))?;
    if !roots.allows(&parent) {
        return Err(TransferError::Outside(parent));
    }
    if !parent.is_dir() {
        return Err(TransferError::Invalid(format!(
            "{} is not a folder",
            parent.display()
        )));
    }
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Ok(m) if m.file_type().is_symlink() => Err(TransferError::Invalid(format!(
            "{} is a symbolic link",
            target.display()
        ))),
        Ok(m) if !m.is_dir() => Err(TransferError::Invalid(format!(
            "{} exists and is not a folder",
            target.display()
        ))),
        Ok(_) => {
            let mut listing = fs::read_dir(&target).map_err(|e| TransferError::io(&target, e))?;
            if fresh && listing.next().is_some() {
                return Err(TransferError::NotEmpty(target));
            }
            Ok(target)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            fs::create_dir(&target).map_err(|e| TransferError::io(&target, e))?;
            Ok(target)
        }
        Err(e) => Err(TransferError::io(&target, e)),
    }
}

#[derive(Debug, Default)]
struct Stats {
    written: u64,
    bytes: u64,
    skipped: u64,
}

/// Why an import stopped.
enum Stop {
    /// An entry the import refuses (path or symlink rules).
    Rejected(String),
    Failed(String),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Self {
        Stop::Failed(e.to_string())
    }
}

fn unpack(target: &Path, input: impl Read) -> TransferResult<Stats> {
    let mut stats = Stats::default();
    match unpack_entries(target, input, &mut stats) {
        Ok(()) => Ok(stats),
        Err(stop) => {
            let (code, message) = match stop {
                Stop::Rejected(m) => ("rejected", m),
                Stop::Failed(m) => ("failed", m),
            };
            let message = format!(
                "{message} (stopped after {} written entries, {} bytes)",
                stats.written, stats.bytes
            );
            Err(TransferError::Stopped {
                code,
                message,
                written: stats.written,
                bytes: stats.bytes,
            })
        }
    }
}

fn unpack_entries(target: &Path, input: impl Read, stats: &mut Stats) -> Result<(), Stop> {
    let decoder = zstd::Decoder::new(input)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let kind = match kind {
            tar::EntryType::Regular | tar::EntryType::Continuous => Kind::File,
            tar::EntryType::Directory => Kind::Dir,
            tar::EntryType::Symlink => Kind::Symlink,
            tar::EntryType::XGlobalHeader => continue,
            _ => {
                stats.skipped += 1;
                continue;
            }
        };
        let parts = components(&entry.path_bytes()).map_err(Stop::Rejected)?;
        let shown = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
        let mtime = UNIX_EPOCH + Duration::from_secs(entry.header().mtime().unwrap_or(0));
        let dir = parent_dir(target, &parts, &shown)?;
        let leaf = dir.join(parts.last().expect("components are never empty"));
        let existing = match fs::symlink_metadata(&leaf) {
            Ok(m) => Some(m.file_type()),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(Stop::Failed(format!("{}: {e}", leaf.display()))),
        };
        match kind {
            Kind::File | Kind::Symlink => {
                if existing.is_some_and(|t| t.is_dir()) {
                    return Err(Stop::Rejected(format!(
                        "{shown}: a folder exists where the archive has a {}",
                        kind.name()
                    )));
                }
                // Written next to the leaf, then renamed over it: an
                // existing symlink there is replaced, never written through.
                let tmp = dir.join(format!(".dshx-transfer-{}", uuid::Uuid::new_v4().simple()));
                let written = if kind == Kind::File {
                    write_file(&tmp, &mut entry, mode, mtime)
                } else {
                    let link = entry.link_name_bytes().ok_or_else(|| {
                        Stop::Rejected(format!("{shown}: symlink without target"))
                    })?;
                    write_symlink(&tmp, OsStr::from_bytes(&link), mtime)
                };
                let size = match written.and_then(|n| fs::rename(&tmp, &leaf).map(|()| n)) {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = fs::remove_file(&tmp);
                        return Err(Stop::Failed(format!("{}: {e}", leaf.display())));
                    }
                };
                stats.bytes += size;
            }
            Kind::Dir => {
                match existing {
                    None => fs::create_dir(&leaf)
                        .map_err(|e| Stop::Failed(format!("{}: {e}", leaf.display())))?,
                    Some(t) if t.is_dir() => {}
                    Some(_) => {
                        return Err(Stop::Rejected(format!(
                            "{shown}: a file or symlink exists where the archive has a folder"
                        )));
                    }
                }
                let set = fs::set_permissions(&leaf, fs::Permissions::from_mode(mode))
                    .and_then(|()| File::open(&leaf))
                    .and_then(|f| {
                        f.set_times(FileTimes::new().set_accessed(mtime).set_modified(mtime))
                    });
                set.map_err(|e| Stop::Failed(format!("{}: {e}", leaf.display())))?;
            }
        }
        stats.written += 1;
    }
    // The rest of the zstd stream: a truncated upload fails here.
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    Ok(())
}

/// The folder an entry goes into, created as needed. Every step must be a
/// real folder: a symlink on the way is refused, never followed.
fn parent_dir(target: &Path, parts: &[OsString], shown: &str) -> Result<PathBuf, Stop> {
    let mut dir = target.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Stop::Rejected(format!(
                    "{shown}: {} is a symbolic link; refusing to write through it",
                    dir.display()
                )));
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(Stop::Rejected(format!(
                    "{shown}: {} is a file, not a folder",
                    dir.display()
                )));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                fs::create_dir(&dir).map_err(|e| Stop::Failed(format!("{}: {e}", dir.display())))?
            }
            Err(e) => return Err(Stop::Failed(format!("{}: {e}", dir.display()))),
        }
    }
    Ok(dir)
}

fn write_file(
    tmp: &Path,
    data: &mut impl Read,
    mode: u32,
    mtime: std::time::SystemTime,
) -> io::Result<u64> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)?;
    let n = io::copy(data, &mut file)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.set_times(FileTimes::new().set_accessed(mtime).set_modified(mtime))?;
    Ok(n)
}

fn write_symlink(tmp: &Path, target: &OsStr, mtime: std::time::SystemTime) -> io::Result<u64> {
    std::os::unix::fs::symlink(target, tmp)?;
    let t = filetime::FileTime::from_system_time(mtime);
    filetime::set_symlink_file_times(tmp, t, t)?;
    Ok(0)
}

#[derive(Debug, Deserialize)]
struct DeleteRequest {
    path: String,
    #[serde(default)]
    files: Vec<String>,
}

/// Removes relative entries below a folder (sync "delete extra files").
/// `name/` removes a whole folder, a bare name a file, symlink or empty
/// folder. Nothing is deleted when one entry is invalid or lies behind a
/// symlink.
async fn delete(
    State(state): State<AppState>,
    Json(req): Json<DeleteRequest>,
) -> TransferResult<Json<Value>> {
    let roots = Roots::load(host(&state)?.as_ref()).await?;
    let root = roots.folder(&req.path)?;
    let mut wanted = Vec::new();
    for f in &req.files {
        let whole = f.ends_with('/');
        let parts =
            components(f.trim_end_matches('/').as_bytes()).map_err(TransferError::Invalid)?;
        wanted.push((f.clone(), parts, whole));
    }
    let deleted = tokio::task::spawn_blocking(move || delete_entries(&root, &wanted))
        .await
        .map_err(|e| TransferError::Io(e.to_string()))??;
    Ok(Json(json!({ "deleted": deleted })))
}

fn delete_entries(root: &Path, wanted: &[(String, Vec<OsString>, bool)]) -> TransferResult<u64> {
    // First pass: every parent on the way must be a real folder.
    let mut leaves = Vec::new();
    'entries: for (shown, parts, whole) in wanted {
        let mut dir = root.to_path_buf();
        for part in &parts[..parts.len() - 1] {
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(TransferError::Invalid(format!(
                        "{shown}: {} is not a real folder",
                        dir.display()
                    )));
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => continue 'entries,
                Err(e) => return Err(TransferError::io(&dir, e)),
            }
        }
        leaves.push((shown, dir.join(&parts[parts.len() - 1]), *whole));
    }
    let mut deleted = 0;
    for (shown, leaf, whole) in leaves {
        let meta = match fs::symlink_metadata(&leaf) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(TransferError::io(&leaf, e)),
        };
        let result = if !meta.is_dir() {
            fs::remove_file(&leaf)
        } else if whole {
            // remove_dir_all removes symlinks inside, never their targets.
            fs::remove_dir_all(&leaf)
        } else {
            fs::remove_dir(&leaf)
        };
        result.map_err(|e| TransferError::Stopped {
            code: "failed",
            message: format!("{shown}: {e} (after deleting {deleted} entries)"),
            written: deleted,
            bytes: 0,
        })?;
        deleted += 1;
    }
    Ok(deleted)
}

#[derive(Debug, Deserialize)]
struct RemoveRequest {
    path: String,
}

/// Deletes a whole folder after a verified move and drops it from the
/// saved project folders. Never the home folder, a project parent or a
/// folder above one of them; a symlinked path is refused.
async fn remove(
    State(state): State<AppState>,
    Json(req): Json<RemoveRequest>,
) -> TransferResult<Json<Value>> {
    let host = host(&state)?;
    let roots = Roots::load(host.as_ref()).await?;
    let path = roots.absolute(&req.path)?;
    let meta = fs::symlink_metadata(&path).map_err(|e| TransferError::io(&path, e))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(TransferError::Invalid(format!(
            "{} is not a real folder",
            path.display()
        )));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| TransferError::io(&path, e))?;
    if let Some(c) = roots.containers.iter().find(|c| c.starts_with(&canonical)) {
        return Err(TransferError::Invalid(format!(
            "refusing to remove {}: it is or holds {}",
            canonical.display(),
            c.display()
        )));
    }
    // Strictly inside a root: a saved project lies inside its parent.
    let inside = roots
        .all
        .iter()
        .any(|r| canonical.starts_with(r) && canonical != *r);
    if is_fs_root(&canonical) || !inside {
        return Err(TransferError::Outside(canonical));
    }
    let gone = canonical.clone();
    tokio::task::spawn_blocking(move || fs::remove_dir_all(&gone))
        .await
        .map_err(|e| TransferError::Io(e.to_string()))?
        .map_err(|e| TransferError::io(&canonical, e))?;
    for p in [&path, &canonical] {
        host.remove_workspace(p)
            .await
            .map_err(|e| TransferError::Io(format!("{e:#}")))?;
    }
    Ok(Json(json!({ "removed": true, "path": canonical })))
}
