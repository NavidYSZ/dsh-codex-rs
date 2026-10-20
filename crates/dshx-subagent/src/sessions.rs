//! Where child sessions live. Children are created in the same store as
//! their parent (dsh: the same `ctx.sessions`), so [`StoreSessions`] wraps the
//! binary's [`SessionStore`]; tests use [`MemorySessions`].
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{NewEvent, SessionEvent, SessionHeader, SessionLog};
use dshx_session::{FileLog, MemoryLog, SessionStore, interrupted_turn_closers, to_new_events};
use serde_json::json;

/// Creates, reopens and reads child session logs.
#[async_trait]
pub trait SessionFactory: Send + Sync {
    /// A new session with `header`; `seed` (contiguous from seq 0) is written first.
    async fn create(
        &self,
        header: SessionHeader,
        seed: Vec<SessionEvent>,
    ) -> anyhow::Result<Arc<dyn SessionLog>>;
    /// Reopens a stored session to continue it like dsh's resume: an
    /// interrupted final turn is closed and `session/end-seed {}` appended.
    async fn resume(&self, id: &str) -> anyhow::Result<Arc<dyn SessionLog>>;
    /// Reads a session without opening it for writing.
    async fn read(&self, id: &str) -> anyhow::Result<(SessionHeader, Vec<SessionEvent>)>;
    /// Flushes and releases a log this factory opened (after its agent is disposed).
    async fn close(&self, id: &str) -> anyhow::Result<()>;
}

fn seed_events(seed: Vec<SessionEvent>) -> Vec<NewEvent> {
    seed.into_iter()
        .map(|e| NewEvent {
            kind: e.kind,
            data: e.data,
            extra: e.extra,
        })
        .collect()
}

/// Children as dsh v4 logs in a [`SessionStore`]. Seed events are rewritten
/// through the regular append path, so they carry the copy's time.
pub struct StoreSessions {
    store: Arc<SessionStore>,
    open: Mutex<HashMap<String, FileLog>>,
}

impl StoreSessions {
    pub fn new(store: Arc<SessionStore>) -> Self {
        StoreSessions {
            store,
            open: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &Arc<SessionStore> {
        &self.store
    }
}

#[async_trait]
impl SessionFactory for StoreSessions {
    async fn create(
        &self,
        header: SessionHeader,
        seed: Vec<SessionEvent>,
    ) -> anyhow::Result<Arc<dyn SessionLog>> {
        let id = header.id.clone();
        let log = self.store.create_with_header(header)?;
        if !seed.is_empty() {
            log.append_now(seed_events(seed))?;
        }
        self.open.lock().unwrap().insert(id, log.clone());
        Ok(Arc::new(log))
    }

    async fn resume(&self, id: &str) -> anyhow::Result<Arc<dyn SessionLog>> {
        let log = self.store.resume(id).await?;
        self.open.lock().unwrap().insert(id.to_owned(), log.clone());
        Ok(Arc::new(log))
    }

    async fn read(&self, id: &str) -> anyhow::Result<(SessionHeader, Vec<SessionEvent>)> {
        if let Some(log) = self.open.lock().unwrap().get(id) {
            return Ok((log.header(), log.events()));
        }
        let store = self.store.clone();
        let id = id.to_owned();
        let stored = tokio::task::spawn_blocking(move || store.read(&id)).await??;
        Ok((stored.header, stored.events))
    }

    async fn close(&self, id: &str) -> anyhow::Result<()> {
        let log = self.open.lock().unwrap().remove(id);
        if let Some(log) = log {
            log.close().await?;
        }
        Ok(())
    }
}

/// In-memory children (tests, ephemeral runs). Seed events keep their times.
#[derive(Default)]
pub struct MemorySessions {
    logs: Mutex<HashMap<String, Arc<MemoryLog>>>,
}

impl MemorySessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an existing log (e.g. a top-level session) so children can be read back.
    pub fn insert(&self, log: Arc<MemoryLog>) {
        self.logs.lock().unwrap().insert(log.id().to_owned(), log);
    }

    pub fn get(&self, id: &str) -> Option<Arc<MemoryLog>> {
        self.logs.lock().unwrap().get(id).cloned()
    }

    /// Ids of every stored session.
    pub fn ids(&self) -> Vec<String> {
        self.logs.lock().unwrap().keys().cloned().collect()
    }
}

#[async_trait]
impl SessionFactory for MemorySessions {
    async fn create(
        &self,
        header: SessionHeader,
        seed: Vec<SessionEvent>,
    ) -> anyhow::Result<Arc<dyn SessionLog>> {
        let mut logs = self.logs.lock().unwrap();
        anyhow::ensure!(
            !logs.contains_key(&header.id),
            "session \"{}\" already exists",
            header.id
        );
        let log = Arc::new(MemoryLog::with_events(header, seed));
        logs.insert(log.id().to_owned(), log.clone());
        Ok(log)
    }

    async fn resume(&self, id: &str) -> anyhow::Result<Arc<dyn SessionLog>> {
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session \"{id}\" not found"))?;
        let closers = interrupted_turn_closers(&log.events());
        if !closers.is_empty() {
            log.append(to_new_events(&closers)).await?;
        }
        if log
            .events()
            .last()
            .is_some_and(|e| e.kind != "session/end-seed")
        {
            log.append(vec![NewEvent::new("session/end-seed", json!({}))])
                .await?;
        }
        Ok(log)
    }

    async fn read(&self, id: &str) -> anyhow::Result<(SessionHeader, Vec<SessionEvent>)> {
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session \"{id}\" not found"))?;
        Ok((log.header(), log.events()))
    }

    async fn close(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
