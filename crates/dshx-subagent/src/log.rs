//! A child's session log with dsh's one-shot descriptor timing: the
//! in-process driver appends `subagent/descriptor` from an `agent/pre-step`
//! listener after every inner listener ran, i.e. directly before the first
//! `step/start` of the child's initial turn. Inserting it into the batch that
//! carries that `step/start` gives the same position without a loop hook.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{NewEvent, SessionEvent, SessionHeader, SessionLog, kinds};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::records::DESCRIPTOR;

pub(crate) struct DescriptorLog {
    inner: Arc<dyn SessionLog>,
    pending: Mutex<Option<Value>>,
}

impl DescriptorLog {
    pub(crate) fn new(inner: Arc<dyn SessionLog>, descriptor: Value) -> Self {
        DescriptorLog {
            inner,
            pending: Mutex::new(Some(descriptor)),
        }
    }
}

#[async_trait]
impl SessionLog for DescriptorLog {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn header(&self) -> SessionHeader {
        self.inner.header()
    }

    async fn append(&self, mut events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        if let Some(at) = events.iter().position(|e| e.kind == kinds::STEP_START) {
            let descriptor = self.pending.lock().unwrap().take();
            if let Some(descriptor) = descriptor {
                events.insert(at, NewEvent::new(DESCRIPTOR, descriptor));
            }
        }
        self.inner.append(events).await
    }

    async fn flush(&self) -> anyhow::Result<()> {
        self.inner.flush().await
    }

    fn events(&self) -> Vec<SessionEvent> {
        self.inner.events()
    }

    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.inner.subscribe()
    }
}
