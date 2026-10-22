//! The session root: create/open/resume/read, listing across workspaces, delete.
//!
//! Layout (identical to dsh): `<root>/--<cwd>--/<encoded id>/session.v4.jsonl.zstd`
//! plus `session.lock`. Default root `~/.dsh-codex/sessions`; point it at
//! `~/.dsh/sessions` to share sessions with the official dsh.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use dshx_core::{
    Message, NewEvent, SESSION_FORMAT_VERSION, SessionEvent, SessionHeader, SessionLog, now_ms,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{IoContext, Result, SessionError};
use crate::file_log::FileLog;
use crate::format::{LOG_FILENAME, encode_segment, parse_generation_filename, session_dir};
use crate::lock::SessionLock;
use crate::reader::{ParsedLog, read_log_file};
use crate::repair::{interrupted_turn_closers, to_new_events};
use crate::surface::derive_messages;

/// One stored log read without taking ownership.
#[derive(Debug, Clone)]
pub struct StoredSession {
    pub header: SessionHeader,
    pub events: Vec<SessionEvent>,
    pub path: PathBuf,
    pub size_bytes: u64,
}

impl StoredSession {
    /// Model-visible history (see [`derive_messages`]).
    pub fn messages(&self) -> Vec<Message> {
        derive_messages(&self.events)
    }
}

/// Listing entry (SPEC §6 `GET /api/sessions`, plus a few extras).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    /// Latest `session/title`, if any.
    pub title: Option<String>,
    /// Header cwd (`""` for a session without one).
    pub cwd: String,
    pub created_at: u64,
    /// Time of the last event (header time for an empty session).
    pub updated_at: u64,
    /// `new` (no turn yet), `running` (open turn, writer alive), `interrupted`
    /// (open turn, no writer), or the last `turn/end` reason kind: `completed`,
    /// `aborted`, `error`, `blocked`, `max-tokens`, `interrupted`, `forked`.
    pub status: String,
    /// Number of turns started.
    pub turns: u64,
    pub event_count: u64,
    pub size_bytes: u64,
    /// Set for subagent child sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// The agent preset the session runs (see [`agent_preset`]); `None` for
    /// sessions that recorded none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

/// Where a session lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub dir: PathBuf,
    pub path: PathBuf,
    /// Format generation of the selected (highest) log file.
    pub version: u32,
}

/// File size and mtime the cached summary was computed from.
type CachedSummary = (u64, Option<SystemTime>, SessionSummary);

/// A session root directory.
pub struct SessionStore {
    root: PathBuf,
    /// Listing cache: path → (size, mtime, summary without live status).
    summaries: Mutex<HashMap<PathBuf, CachedSummary>>,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        SessionStore {
            root: root.into(),
            summaries: Mutex::new(HashMap::new()),
        }
    }

    /// `$DSHX_HOME/sessions`, default `~/.dsh-codex/sessions`.
    pub fn default_root() -> PathBuf {
        dshx_core::config::home_dir().join("sessions")
    }

    pub fn open_default() -> Self {
        Self::new(Self::default_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A fresh header like dsh's (`session-<uuid>`, format v4, unseeded).
    pub fn new_header(cwd: &str) -> SessionHeader {
        SessionHeader {
            kind: "session".into(),
            version: SESSION_FORMAT_VERSION,
            id: format!("session-{}", dshx_core::new_id()),
            created_at: now_ms(),
            cwd: cwd.into(),
            is_seeded: false,
            delegation_depth: 0,
            extra: Default::default(),
        }
    }

    /// Creates a session for `cwd` (absolute). Nothing touches disk until the
    /// first flush, so an abandoned session leaves no footprint.
    pub fn create(&self, cwd: &str) -> Result<FileLog> {
        self.create_with_header(Self::new_header(cwd))
    }

    /// Creates a session with a caller-built header (subagent children set
    /// `parentSession`, `origin: "subagent"`, `delegationDepth` in `extra`).
    pub fn create_with_header(&self, header: SessionHeader) -> Result<FileLog> {
        if self.locate(&header.id)?.is_some() {
            return Err(SessionError::AlreadyExists(header.id));
        }
        FileLog::created(&self.root, header)
    }

    /// Finds the unique session directory for `id` across project directories.
    pub fn locate(&self, id: &str) -> Result<Option<Located>> {
        let encoded = encode_segment(id)?;
        let mut found = Vec::new();
        for project in self.project_dirs()? {
            if let Some(located) = select_generation(&project.join(&encoded))? {
                found.push(located);
            }
        }
        if found.len() > 1 {
            return Err(SessionError::Corrupt(format!(
                "duplicate session id \"{id}\" appears in multiple project directories"
            )));
        }
        Ok(found.pop())
    }

    fn require(&self, id: &str) -> Result<Located> {
        let located = self
            .locate(id)?
            .ok_or_else(|| SessionError::NotFound(id.to_owned()))?;
        if located.version != SESSION_FORMAT_VERSION {
            return Err(SessionError::Unsupported(format!(
                "session \"{id}\" is stored in format v{} ({}); open it once with the official dsh to migrate it to v{SESSION_FORMAT_VERSION}",
                located.version,
                located.path.display()
            )));
        }
        Ok(located)
    }

    /// Reads a stored session without taking its lock.
    pub fn read(&self, id: &str) -> Result<StoredSession> {
        let located = self.require(id)?;
        let parsed = read_log_file(&located.path)?;
        self.check_identity(id, &located, &parsed)?;
        Ok(StoredSession {
            header: parsed.header,
            events: parsed.events,
            path: located.path,
            size_bytes: parsed.size_bytes,
        })
    }

    /// [`read`](Self::read) plus in-memory closers for an interrupted final
    /// turn, so a crashed log folds as a balanced transcript (dsh cold read).
    pub fn read_repaired(&self, id: &str) -> Result<StoredSession> {
        let mut stored = self.read(id)?;
        let closers = interrupted_turn_closers(&stored.events);
        stored.events.extend(closers);
        Ok(stored)
    }

    /// Opens an existing session for writing: takes the lock (fails with
    /// `Locked` while dsh or another handle owns it) and recovers a torn tail.
    pub fn open(&self, id: &str) -> Result<FileLog> {
        let located = self.require(id)?;
        let lock = SessionLock::acquire(&located.dir, id)?;
        let parsed = read_log_file(&located.path)?;
        self.check_identity(id, &located, &parsed)?;
        Ok(FileLog::opened(&self.root, located.dir, parsed, lock))
    }

    /// Opens a session to continue it the way dsh's agent loop resumes:
    /// closes an interrupted final turn durably (error results for unanswered
    /// tool calls, `step/end`, `turn/end {interrupted}`), then appends the
    /// `session/end-seed {}` restore marker dsh writes on every resume.
    pub async fn resume(&self, id: &str) -> Result<FileLog> {
        let log = self.open(id)?;
        let events = log.events();
        let closers = interrupted_turn_closers(&events);
        if !closers.is_empty() {
            log.append_now(to_new_events(&closers))?;
        }
        if log
            .events()
            .last()
            .is_some_and(|e| e.kind != "session/end-seed")
        {
            log.append_now(vec![NewEvent::new("session/end-seed", json!({}))])?;
        }
        log.checkpoint().await?;
        Ok(log)
    }

    /// Every readable v4 session under the root, newest first. Unreadable or
    /// historical-format logs are skipped (like dsh's listing).
    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        for project in self.project_dirs()? {
            self.list_project(&project, None, &mut out)?;
        }
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// Sessions whose header cwd is exactly `cwd`, newest first.
    pub fn list_in(&self, cwd: &str) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        let project = crate::format::project_dir(&self.root, cwd);
        if project.is_dir() {
            self.list_project(&project, Some(cwd), &mut out)?;
        }
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// Distinct workspaces with their latest activity, most recent first.
    pub fn workspaces(&self) -> Result<Vec<(String, u64)>> {
        let mut latest: HashMap<String, u64> = HashMap::new();
        for summary in self.list()? {
            let entry = latest.entry(summary.cwd).or_default();
            *entry = (*entry).max(summary.updated_at);
        }
        let mut out: Vec<(String, u64)> = latest
            .into_iter()
            .filter(|(cwd, _)| !cwd.is_empty())
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    fn list_project(
        &self,
        project: &Path,
        cwd: Option<&str>,
        out: &mut Vec<SessionSummary>,
    ) -> Result<()> {
        let entries = match std::fs::read_dir(project) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(SessionError::io(project, e)),
        };
        for entry in entries {
            let entry = entry.at(project)?;
            if !entry.file_type().at(entry.path())?.is_dir() {
                continue;
            }
            let Some(located) = select_generation(&entry.path())? else {
                continue;
            };
            if located.version != SESSION_FORMAT_VERSION {
                tracing::debug!(path = %located.path.display(), "skipping historical session format in listing");
                continue;
            }
            match self.summary(&located) {
                Ok(summary) if cwd.is_none_or(|c| summary.cwd == c) => out.push(summary),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(path = %located.path.display(), %error, "skipping unreadable session")
                }
            }
        }
        Ok(())
    }

    fn summary(&self, located: &Located) -> Result<SessionSummary> {
        let meta = std::fs::metadata(&located.path).at(&located.path)?;
        let key = (meta.len(), meta.modified().ok());
        let cached = {
            let cache = self.summaries.lock().unwrap_or_else(|p| p.into_inner());
            cache
                .get(&located.path)
                .filter(|(size, mtime, _)| (*size, *mtime) == key)
                .map(|(_, _, s)| s.clone())
        };
        let mut summary = match cached {
            Some(summary) => summary,
            None => {
                let parsed = read_log_file(&located.path)?;
                let summary = summarize(&parsed);
                let mut cache = self.summaries.lock().unwrap_or_else(|p| p.into_inner());
                cache.insert(located.path.clone(), (key.0, key.1, summary.clone()));
                summary
            }
        };
        // An open final turn is live only while some writer holds the lock.
        if summary.status == "interrupted-open" {
            summary.status = if SessionLock::is_held(&located.dir) {
                "running"
            } else {
                "interrupted"
            }
            .into();
        }
        Ok(summary)
    }

    /// Deletes a session directory; refuses while a writer holds its lock.
    pub fn delete(&self, id: &str) -> Result<()> {
        let encoded = encode_segment(id)?;
        let mut dirs = Vec::new();
        for project in self.project_dirs()? {
            let dir = project.join(&encoded);
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
        if dirs.is_empty() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        for dir in dirs {
            let lock = SessionLock::acquire(&dir, id)?;
            std::fs::remove_dir_all(&dir).at(&dir)?;
            drop(lock);
            self.summaries
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .retain(|path, _| !path.starts_with(&dir));
            if let Some(project) = dir.parent() {
                let _ = std::fs::remove_dir(project); // only succeeds when empty
            }
        }
        Ok(())
    }

    fn project_dirs(&self) -> Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SessionError::io(&self.root, e)),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.at(&self.root)?;
            if entry.file_type().at(entry.path())?.is_dir() {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        Ok(dirs)
    }

    /// The header must name the file it was found in (dsh `assertStoredIdentity`).
    fn check_identity(&self, id: &str, located: &Located, parsed: &ParsedLog) -> Result<()> {
        if parsed.header.id != id {
            return Err(SessionError::Corrupt(format!(
                "requested id \"{id}\" does not match header id \"{}\" ({})",
                parsed.header.id,
                located.path.display()
            )));
        }
        let expected = session_dir(&self.root, &parsed.header.cwd, id)?.join(LOG_FILENAME);
        let same = expected == located.path
            || matches!((std::fs::canonicalize(&expected), std::fs::canonicalize(&located.path)), (Ok(a), Ok(b)) if a == b);
        if !same {
            return Err(SessionError::Corrupt(format!(
                "header id and cwd identify \"{}\", not \"{}\"",
                expected.display(),
                located.path.display()
            )));
        }
        Ok(())
    }
}

/// Highest canonical generation inside a session directory.
fn select_generation(dir: &Path) -> Result<Option<Located>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(SessionError::io(dir, e)),
    };
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in entries {
        let entry = entry.at(dir)?;
        if let Some(version) = parse_generation_filename(&entry.file_name().to_string_lossy())
            && best.as_ref().is_none_or(|(v, _)| version > *v)
        {
            best = Some((version, entry.path()));
        }
    }
    Ok(best.map(|(version, path)| Located {
        dir: dir.to_owned(),
        path,
        version,
    }))
}

/// dsh's `agentPreset` session projection: the header's creation-time
/// preset, advanced by `agent-preset/selected` events.
pub fn agent_preset(header: &SessionHeader, events: &[SessionEvent]) -> Option<String> {
    let mut preset = header
        .extra
        .get("agentPreset")
        .and_then(Value::as_str)
        .map(str::to_owned);
    for event in events.iter().filter(|e| e.kind == "agent-preset/selected") {
        preset = event
            .data
            .get("agentPreset")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    preset
}

/// Title, status and timestamps of a parsed log. An open final turn is
/// reported as `interrupted-open` and resolved against the lock by the caller.
pub fn summarize(parsed: &ParsedLog) -> SessionSummary {
    let mut title = None;
    let mut turns = 0;
    let mut open = false;
    let mut last_reason: Option<String> = None;
    for event in &parsed.events {
        match event.kind.as_str() {
            "session/title" => {
                if let Some(t) = event.data.get("title").and_then(Value::as_str) {
                    title = Some(t.to_owned());
                }
            }
            "turn/start" => {
                turns += 1;
                open = true;
            }
            "turn/end" => {
                open = false;
                last_reason = event
                    .data
                    .pointer("/reason/kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    let status = if open {
        "interrupted-open".to_owned()
    } else {
        last_reason.unwrap_or_else(|| {
            if turns == 0 {
                "new".into()
            } else {
                "completed".into()
            }
        })
    };
    let header = &parsed.header;
    SessionSummary {
        id: header.id.clone(),
        title,
        cwd: header.cwd.clone(),
        created_at: header.created_at,
        updated_at: parsed
            .events
            .last()
            .map(|e| e.time)
            .unwrap_or(header.created_at),
        status,
        turns,
        event_count: parsed.events.len() as u64,
        size_bytes: parsed.size_bytes,
        parent_session: header
            .extra
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        preset: agent_preset(header, &parsed.events),
    }
}
