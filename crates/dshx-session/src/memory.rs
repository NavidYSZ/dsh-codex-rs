use std::sync::Mutex;

use async_trait::async_trait;
use dshx_core::{NewEvent, SessionEvent, SessionHeader, SessionLog, now_ms};
use tokio::sync::broadcast;

/// In-memory session log for tests and ephemeral runs.
pub struct MemoryLog {
    header: SessionHeader,
    events: Mutex<Vec<SessionEvent>>,
    tx: broadcast::Sender<SessionEvent>,
}

impl MemoryLog {
    pub fn new(cwd: &str) -> Self {
        Self::with_events(crate::SessionStore::new_header(cwd), Vec::new())
    }

    /// A log that starts from stored events (e.g. a [`crate::StoredSession`]).
    /// Unlike [`crate::FileLog`] it does not validate appended events.
    pub fn with_events(header: SessionHeader, events: Vec<SessionEvent>) -> Self {
        MemoryLog {
            header,
            events: Mutex::new(events),
            tx: broadcast::channel(1024).0,
        }
    }
}

#[async_trait]
impl SessionLog for MemoryLog {
    fn id(&self) -> &str {
        &self.header.id
    }
    fn header(&self) -> SessionHeader {
        self.header.clone()
    }
    async fn append(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        let mut log = self.events.lock().unwrap();
        let mut stored = Vec::with_capacity(events.len());
        for e in events {
            let event = SessionEvent {
                kind: e.kind,
                seq: log.len() as u64,
                time: now_ms(),
                data: e.data,
                extra: e.extra,
            };
            log.push(event.clone());
            stored.push(event);
        }
        for e in &stored {
            let _ = self.tx.send(e.clone());
        }
        drop(log);
        Ok(stored)
    }
    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn events(&self) -> Vec<SessionEvent> {
        self.events.lock().unwrap().clone()
    }
    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.tx.subscribe()
    }
}
