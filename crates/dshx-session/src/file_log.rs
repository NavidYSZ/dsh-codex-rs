//! File-backed `SessionLog` writing dsh session format v4 exactly like
//! `session-persistence-jsonl`: lazy materialization through a no-overwrite
//! hard-link publish, one checksummed zstd frame per durable batch, fsync on
//! every batch, rollback of a failed append, torn-tail repair before the
//! first new batch, and the dsh-compatible `flock` on `session.lock`.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{Message, NewEvent, SessionEvent, SessionHeader, SessionLog, now_ms};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::error::{IoContext, Result, SessionError};
use crate::format::{
    LOG_FILENAME, encode_event_line, encode_header_line, is_surface_event_type,
    needs_ignorable_for_dsh_0_2_0, parse_generation_filename,
};
use crate::frames::compress_frame;
use crate::lock::{SessionLock, create_private_dir};
use crate::reader::ParsedLog;
use crate::surface::{SurfaceFold, fold_lenient};

/// dsh's live batching window (`LIVE_WRITE_BATCH_MAX_DELAY_MS`): appended
/// events reach disk at the latest this long after `append`, or at `flush`.
const BATCH_DELAY: Duration = Duration::from_millis(200);

/// A session log on disk. Cheap to clone; all clones share one writer.
#[derive(Clone)]
pub struct FileLog {
    inner: Arc<Inner>,
}

struct Inner {
    header: SessionHeader,
    root: PathBuf,
    dir: PathBuf,
    path: PathBuf,
    state: Mutex<State>,
    io: Mutex<Io>,
    tx: broadcast::Sender<SessionEvent>,
}

struct State {
    events: Vec<SessionEvent>,
    fold: SurfaceFold,
    /// Refuse events whose surface metadata dsh would refuse.
    strict: bool,
    timer_armed: bool,
    closed: bool,
}

struct Io {
    /// Events already in complete, fsynced frames.
    durable: usize,
    materialized: bool,
    lock: Option<SessionLock>,
    /// Torn final frame to cut before the next batch.
    torn_truncate_to: Option<u64>,
    /// `events[durable..recovered_until]` came from the torn frame and are
    /// rewritten as their own batch first.
    recovered_until: Option<usize>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FileLog {
    /// A new session that reaches disk at its first flush (no footprint before).
    pub(crate) fn created(root: &Path, header: SessionHeader) -> Result<FileLog> {
        encode_header_line(&header)?;
        let dir = crate::format::session_dir(root, &header.cwd, &header.id)?;
        Ok(Self::build(
            root,
            dir,
            header,
            Vec::new(),
            Io {
                durable: 0,
                materialized: false,
                lock: None,
                torn_truncate_to: None,
                recovered_until: None,
            },
        ))
    }

    /// A new session that starts with `seed` (dense from seq 0, times kept),
    /// written with the header at the first flush.
    pub(crate) fn seeded(
        root: &Path,
        header: SessionHeader,
        seed: Vec<SessionEvent>,
    ) -> Result<FileLog> {
        encode_header_line(&header)?;
        for (index, event) in seed.iter().enumerate() {
            if event.seq != index as u64 {
                return Err(SessionError::InvalidEvent(format!(
                    "seed event {index} has seq {} (seeds are dense from 0)",
                    event.seq
                )));
            }
            encode_event_line(event)?;
        }
        let dir = crate::format::session_dir(root, &header.cwd, &header.id)?;
        Ok(Self::build(
            root,
            dir,
            header,
            seed,
            Io {
                durable: 0,
                materialized: false,
                lock: None,
                torn_truncate_to: None,
                recovered_until: None,
            },
        ))
    }

    /// An existing session opened for writing under a held lock.
    pub(crate) fn opened(
        root: &Path,
        dir: PathBuf,
        parsed: ParsedLog,
        lock: SessionLock,
    ) -> FileLog {
        let complete = parsed.events.len() - parsed.recovered_tail;
        let io = Io {
            durable: complete,
            materialized: true,
            lock: Some(lock),
            torn_truncate_to: parsed.torn_truncate_to,
            recovered_until: (parsed.recovered_tail > 0).then_some(parsed.events.len()),
        };
        Self::build(root, dir, parsed.header, parsed.events, io)
    }

    fn build(
        root: &Path,
        dir: PathBuf,
        header: SessionHeader,
        events: Vec<SessionEvent>,
        io: Io,
    ) -> FileLog {
        let (fold, strict) = match crate::surface::fold_surface(&events) {
            Ok(fold) => (fold, true),
            Err(error) => {
                // Keep a stored log usable even where our port disagrees.
                tracing::warn!(session = %header.id, %error, "stored log violates surface rules; appends are not validated");
                (fold_lenient(&events), false)
            }
        };
        let path = dir.join(LOG_FILENAME);
        FileLog {
            inner: Arc::new(Inner {
                header,
                root: root.to_owned(),
                dir,
                path,
                state: Mutex::new(State {
                    events,
                    fold,
                    strict,
                    timer_armed: false,
                    closed: false,
                }),
                io: Mutex::new(io),
                tx: broadcast::channel(1024).0,
            }),
        }
    }

    /// The log file (`…/session.v4.jsonl.zstd`), present once materialized.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The session's own directory (also home of `session.lock`).
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// The store root this log lives under.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Model-visible history, from the incrementally maintained surface.
    pub fn messages(&self) -> Vec<Message> {
        let state = lock(&self.inner.state);
        state.fold.messages(&state.events)
    }

    /// Surface content generation; changes when history is rewritten
    /// (compaction, pruning, image offload) rather than only appended.
    pub fn content_generation(&self) -> u64 {
        lock(&self.inner.state).fold.content_generation()
    }

    /// Validates, sequences and buffers events (durable at the next flush or
    /// within the batching window). The whole batch is refused when one
    /// event would make the log unreadable for dsh.
    pub fn append_now(&self, events: Vec<NewEvent>) -> Result<Vec<SessionEvent>> {
        let mut state = lock(&self.inner.state);
        if state.closed {
            return Err(SessionError::Closed(self.inner.header.id.clone()));
        }
        let base = state.events.len();
        let strict = state.strict;
        // Validate the whole batch against a scratch fold: all or nothing.
        let mut fold = state.fold.clone();
        for new in events {
            let event = prepare_event(new, state.events.len() as u64);
            let checked = encode_event_line(&event).map(|_| ()).and_then(|()| {
                if strict {
                    fold.push(&event, &state.events).map_err(|e| {
                        SessionError::InvalidEvent(format!(
                            "{} at seq {}: {e}",
                            event.kind, event.seq
                        ))
                    })
                } else {
                    fold.push_lenient(&event, &state.events);
                    Ok(())
                }
            });
            if let Err(error) = checked {
                state.events.truncate(base);
                return Err(error);
            }
            state.events.push(event);
        }
        state.fold = fold;
        let stored = state.events[base..].to_vec();
        if !stored.is_empty()
            && !state.timer_armed
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            state.timer_armed = true;
            runtime.spawn(batch_timer(Arc::downgrade(&self.inner)));
        }
        drop(state);
        for event in &stored {
            let _ = self.inner.tx.send(event.clone());
        }
        Ok(stored)
    }

    /// Durability barrier without an async runtime.
    pub fn flush_now(&self) -> Result<()> {
        self.inner.flush_blocking()
    }

    /// Durability barrier (the trait's `flush` with a typed error).
    pub async fn checkpoint(&self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.flush_blocking())
            .await
            .map_err(|e| SessionError::io(self.path(), std::io::Error::other(e)))?
    }

    /// Flushes and releases the write lock; later appends fail with `Closed`.
    /// A created session without events stays without footprint (like dsh).
    pub async fn close(&self) -> Result<()> {
        lock(&self.inner.state).closed = true;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let empty = !lock(&inner.io).materialized && lock(&inner.state).events.is_empty();
            let result = if empty {
                Ok(())
            } else {
                inner.flush_blocking()
            };
            lock(&inner.io).lock = None;
            result
        })
        .await
        .map_err(|e| SessionError::io(self.path(), std::io::Error::other(e)))?
    }

    /// Whether the session has a file on disk yet.
    pub fn is_materialized(&self) -> bool {
        lock(&self.inner.io).materialized
    }
}

/// Assigns seq/time and marks types the installed dsh 0.2.0 cannot read as ignorable.
fn prepare_event(new: NewEvent, seq: u64) -> SessionEvent {
    let mut extra = new.extra;
    if !is_surface_event_type(&new.kind)
        && needs_ignorable_for_dsh_0_2_0(&new.kind)
        && extra.get("ignorable") != Some(&Value::Bool(true))
    {
        extra.insert("ignorable".into(), Value::Bool(true));
    }
    SessionEvent {
        kind: new.kind,
        seq,
        time: now_ms(),
        data: new.data,
        extra,
    }
}

async fn batch_timer(inner: Weak<Inner>) {
    tokio::time::sleep(BATCH_DELAY).await;
    let Some(inner) = inner.upgrade() else { return };
    lock(&inner.state).timer_armed = false;
    let id = inner.header.id.clone();
    let result = tokio::task::spawn_blocking(move || inner.flush_blocking()).await;
    if let Ok(Err(error)) = result {
        tracing::warn!(session = %id, %error, "background session flush failed; retried at the next flush");
    }
}

impl Inner {
    fn flush_blocking(&self) -> Result<()> {
        let mut io = lock(&self.io);
        let (pending, total) = {
            let state = lock(&self.state);
            (state.events[io.durable..].to_vec(), state.events.len())
        };
        if io.materialized && pending.is_empty() {
            return Ok(());
        }
        if !io.materialized {
            if io.lock.is_none() {
                io.lock = Some(SessionLock::acquire(&self.dir, &self.header.id)?);
            }
            self.materialize(&pending)?;
            io.materialized = true;
        } else {
            if io.lock.is_none() {
                return Err(SessionError::Closed(self.header.id.clone()));
            }
            if let Some(offset) = io.torn_truncate_to {
                truncate_durably(&self.path, offset)?;
                tracing::warn!(session = %self.header.id, "recovered from a torn tail; incomplete tail bytes were discarded");
                io.torn_truncate_to = None;
            }
            let mut rest = &pending[..];
            if let Some(until) = io.recovered_until {
                let (recovered, new) = pending.split_at(until - io.durable);
                if !recovered.is_empty() {
                    append_frame(&self.path, &encode_batch(recovered)?)?;
                }
                io.durable = until;
                io.recovered_until = None;
                rest = new;
            }
            if !rest.is_empty() {
                append_frame(&self.path, &encode_batch(rest)?)?;
            }
        }
        io.durable = total;
        Ok(())
    }

    /// First write: header frame (+ first batch frame) published without overwrite.
    fn materialize(&self, events: &[SessionEvent]) -> Result<()> {
        let project = self.dir.parent().expect("session dir has a project dir");
        create_private_dir(&self.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent);
        }
        create_private_dir(project)?;
        sync_dir(&self.root);
        create_private_dir(&self.dir)?;
        sync_dir(project);
        if existing_generation(&self.dir)? {
            return Err(SessionError::AlreadyExists(self.header.id.clone()));
        }
        let mut content =
            compress_frame(format!("{}\n", encode_header_line(&self.header)?).as_bytes());
        if !events.is_empty() {
            content.extend(encode_batch(events)?);
        }
        let tmp = self.path.with_file_name(format!(
            "{LOG_FILENAME}.{}.tmp",
            &dshx_core::new_id().replace('-', "")[..12]
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&tmp).and_then(|mut f| {
            f.write_all(&content)?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(SessionError::io(&tmp, e));
        }
        // link() fails with EEXIST instead of clobbering a concurrent creator.
        let linked = std::fs::hard_link(&tmp, &self.path);
        if let Err(e) = linked {
            let _ = std::fs::remove_file(&tmp);
            return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
                SessionError::AlreadyExists(self.header.id.clone())
            } else {
                SessionError::io(&self.path, e)
            });
        }
        sync_dir(&self.dir);
        let _ = std::fs::remove_file(&tmp);
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let has_pending = {
            let io = lock(&self.io);
            let closed_writer = io.materialized && io.lock.is_none();
            !closed_writer && lock(&self.state).events.len() > io.durable
        };
        if has_pending && let Err(error) = self.flush_blocking() {
            tracing::warn!(session = %self.header.id, %error, "final session flush failed");
        }
    }
}

fn encode_batch(events: &[SessionEvent]) -> Result<Vec<u8>> {
    let mut body = String::new();
    for event in events {
        body.push_str(&encode_event_line(event)?);
        body.push('\n');
    }
    Ok(compress_frame(body.as_bytes()))
}

/// Appends and fsyncs one frame; on failure the file is cut back so a retry
/// cannot duplicate sequence numbers.
fn append_frame(path: &Path, frame: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().append(true).open(path).at(path)?;
    let before = file.metadata().at(path)?.len();
    if let Err(error) = file.write_all(frame).and_then(|()| file.sync_all()) {
        drop(file);
        if let Err(rollback) = truncate_durably(path, before) {
            tracing::error!(path = %path.display(), %rollback, "failed to roll back a partial append");
        }
        return Err(SessionError::io(path, error));
    }
    Ok(())
}

fn truncate_durably(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new().write(true).open(path).at(path)?;
    file.set_len(len).at(path)?;
    file.sync_all().at(path)
}

/// Directory fsync so a new entry survives power loss (best effort).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn existing_generation(dir: &Path) -> Result<bool> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(SessionError::io(dir, e)),
    };
    for entry in entries {
        let entry = entry.at(dir)?;
        if parse_generation_filename(&entry.file_name().to_string_lossy()).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[async_trait]
impl SessionLog for FileLog {
    fn id(&self) -> &str {
        &self.inner.header.id
    }

    fn header(&self) -> SessionHeader {
        self.inner.header.clone()
    }

    async fn append(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self.append_now(events)?)
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(self.checkpoint().await?)
    }

    fn events(&self) -> Vec<SessionEvent> {
        lock(&self.inner.state).events.clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.inner.tx.subscribe()
    }
}
