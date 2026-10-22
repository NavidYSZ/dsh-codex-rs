//! Local mirror of the session log plus the positional surface fold.
//!
//! Every append goes through here so the loop can read its own history without
//! cloning the whole log per step. The surface fold mirrors dsh `surface.ts`:
//! `surfaceOp: "append"` pushes a node, `{op: "replace", startSeq, endSeq}`
//! swaps an inclusive node range for the new event, and `image/offload`
//! marks image occurrences of existing nodes as offloaded.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dshx_core::{ContentBlock, Message, NewEvent, SessionEvent, SessionHeader, SessionLog, kinds};
use serde_json::Value;

/// Event types that may carry a `surfaceOp`.
pub(crate) fn is_surface_type(kind: &str) -> bool {
    matches!(
        kind,
        kinds::SYSTEM_MESSAGE
            | kinds::DEVELOPER_MESSAGE
            | kinds::USER_MESSAGE
            | kinds::ASSISTANT_MESSAGE
            | kinds::TOOL_RESULT
    )
}

/// The model-visible message of one surface event (dsh `deriveEventMessage`):
/// empty system/developer/assistant nodes keep their position but send nothing.
pub(crate) fn event_message(event: &SessionEvent) -> Option<Message> {
    let raw = match event.kind.as_str() {
        kinds::USER_MESSAGE => &event.data,
        kinds::SYSTEM_MESSAGE | kinds::DEVELOPER_MESSAGE | kinds::ASSISTANT_MESSAGE => {
            let m = event.data.get("message")?;
            if m.get("content")
                .and_then(Value::as_array)
                .is_some_and(|c| c.is_empty())
            {
                return None;
            }
            m
        }
        kinds::TOOL_RESULT => event.data.get("message")?,
        _ => return None,
    };
    serde_json::from_value(raw.clone()).ok()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Surface {
    pub nodes: Vec<u64>,
    /// Messages rewritten by `image/offload`, keyed by their event seq.
    pub projected: HashMap<u64, Message>,
    pub replace_generation: u64,
    pub content_generation: u64,
}

impl Surface {
    /// The model-visible message of `event` with offloaded images marked.
    pub(crate) fn message(&self, event: &SessionEvent) -> Option<Message> {
        match self.projected.get(&event.seq) {
            Some(message) => Some(message.clone()),
            None => event_message(event),
        }
    }

    /// `events` holds the log before `event`, indexed by seq.
    pub(crate) fn apply(&mut self, event: &SessionEvent, events: &[SessionEvent]) {
        if event.kind == kinds::IMAGE_OFFLOAD {
            self.offload(event, events);
            return;
        }
        if !is_surface_type(&event.kind) {
            return;
        }
        match event.extra.get("surfaceOp") {
            Some(Value::String(op)) if op == "append" => self.nodes.push(event.seq),
            Some(Value::Object(op)) if op.get("op").and_then(Value::as_str) == Some("replace") => {
                let start = op.get("startSeq").and_then(Value::as_u64);
                let end = op.get("endSeq").and_then(Value::as_u64);
                let (Some(start), Some(end)) = (start, end) else {
                    return;
                };
                let si = self.nodes.iter().position(|s| *s == start);
                let ei = self.nodes.iter().position(|s| *s == end);
                if let (Some(si), Some(ei)) = (si, ei)
                    && si <= ei
                {
                    self.nodes.splice(si..=ei, [event.seq]);
                    self.replace_generation += 1;
                    self.content_generation += 1;
                } else {
                    tracing::warn!(
                        seq = event.seq,
                        "surface replace names an unknown range; ignored"
                    );
                }
            }
            _ => {}
        }
    }

    /// dsh `imageOffloadProjection`: each target names a current node and the
    /// indexes of its image blocks (counting already offloaded ones).
    fn offload(&mut self, event: &SessionEvent, events: &[SessionEvent]) {
        let targets = event.data.get("targets").and_then(Value::as_array);
        for target in targets.into_iter().flatten() {
            let source = target
                .get("seq")
                .and_then(Value::as_u64)
                .filter(|s| self.nodes.contains(s))
                .and_then(|s| events.get(s as usize))
                .filter(|e| e.kind == kinds::USER_MESSAGE || e.kind == kinds::TOOL_RESULT);
            let Some((source, mut message)) = source.and_then(|e| Some((e, self.message(e)?)))
            else {
                tracing::warn!(
                    seq = event.seq,
                    "image/offload names an unknown node; ignored"
                );
                continue;
            };
            let wanted: Vec<u64> = target
                .get("imageIndexes")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let mut index = 0;
            for block in &mut message.content {
                if let ContentBlock::Image { offloaded, .. } = block {
                    *offloaded |= wanted.contains(&index);
                    index += 1;
                }
            }
            self.projected.insert(source.seq, message);
        }
        self.content_generation += 1;
    }
}

struct State {
    events: Vec<SessionEvent>,
    surface: Surface,
    folded: usize,
}

impl State {
    fn fold(&mut self) {
        while self.folded < self.events.len() {
            let (before, rest) = self.events.split_at(self.folded);
            self.surface.apply(&rest[0], before);
            self.folded += 1;
        }
    }
}

/// Shared session log handle with a local mirror. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Journal {
    log: Arc<dyn SessionLog>,
    state: Arc<Mutex<State>>,
}

impl Journal {
    pub fn new(log: Arc<dyn SessionLog>) -> Self {
        let events = log.events();
        let mut state = State {
            events,
            surface: Surface::default(),
            folded: 0,
        };
        state.fold();
        Journal {
            log,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn id(&self) -> String {
        self.log.id().to_owned()
    }

    pub fn header(&self) -> SessionHeader {
        self.log.header()
    }

    pub async fn flush(&self) -> anyhow::Result<()> {
        self.log.flush().await
    }

    pub async fn append(&self, event: NewEvent) -> anyhow::Result<SessionEvent> {
        let mut stored = self.append_all(vec![event]).await?;
        stored
            .pop()
            .ok_or_else(|| anyhow::anyhow!("session log returned no event"))
    }

    pub async fn append_all(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let stored = self.log.append(events).await?;
        let mut resync = false;
        {
            let mut st = self.state.lock().unwrap();
            for e in &stored {
                let len = st.events.len() as u64;
                if e.seq == len {
                    st.events.push(e.clone());
                } else if e.seq > len {
                    // Someone else appended in between (or a concurrent append of ours
                    // finished first): reload the authoritative log.
                    resync = true;
                    break;
                }
            }
            if !resync {
                st.fold();
            }
        }
        if resync {
            self.resync();
        }
        Ok(stored)
    }

    /// Reloads the mirror from the log (after an out-of-order append).
    pub fn resync(&self) {
        let events = self.log.events();
        let mut st = self.state.lock().unwrap();
        if events.len() >= st.events.len() {
            let keep = st.folded.min(events.len());
            // Already folded prefixes are immutable; only fold the new tail.
            st.events = events;
            st.folded = keep;
            st.fold();
        }
    }

    /// Runs `f` over the mirrored events and current surface.
    pub fn read<R>(&self, f: impl FnOnce(&[SessionEvent], &Surface) -> R) -> R {
        let mut st = self.state.lock().unwrap();
        st.fold();
        f(&st.events, &st.surface)
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.log.subscribe()
    }

    pub fn len(&self) -> u64 {
        self.state.lock().unwrap().events.len() as u64
    }

    pub fn surface(&self) -> Surface {
        self.read(|_, s| s.clone())
    }

    /// The derived model-visible history (dsh `Session.deriveMessages()`).
    pub fn derive_messages(&self) -> Vec<Message> {
        self.read(|events, surface| {
            surface
                .nodes
                .iter()
                .filter_map(|seq| events.get(*seq as usize).and_then(|e| surface.message(e)))
                .collect()
        })
    }

    /// Latest event of `kind`.
    pub fn last_of(&self, kind: &str) -> Option<SessionEvent> {
        self.read(|events, _| events.iter().rev().find(|e| e.kind == kind).cloned())
    }

    /// Header of the latest `request/header` event.
    pub fn request_header(&self) -> Option<Value> {
        self.last_of(kinds::REQUEST_HEADER)
            .and_then(|e| e.data.get("header").cloned())
    }

    /// Data of the latest `request/context` event.
    pub fn request_context(&self) -> Option<Value> {
        self.last_of(kinds::REQUEST_CONTEXT).map(|e| e.data)
    }
}

impl dshx_core::CallerSession for Journal {
    fn events_from(&self, from: u64) -> Vec<SessionEvent> {
        self.read(|events, _| {
            events
                .get(from.min(events.len() as u64) as usize..)
                .map(<[SessionEvent]>::to_vec)
                .unwrap_or_default()
        })
    }
}
