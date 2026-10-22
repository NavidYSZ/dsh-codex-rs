//! Session log events (dsh session format v4).
//!
//! On disk every line is `{"type", "seq", "time", "data", ...}`; some events
//! carry extra top-level keys (`surfaceOp`, `sourceEventSeqs`), kept in `extra`.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_FORMAT_VERSION: u32 = 4;

/// First line of a session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub kind: String, // always "session"
    pub version: u32,
    pub id: String,
    pub created_at: u64,
    pub cwd: String,
    pub is_seeded: bool,
    pub delegation_depth: u32,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub seq: u64,
    pub time: u64,
    #[serde(default)]
    pub data: Value,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// An event before the log assigns `seq` and `time`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: String,
    pub data: Value,
    pub extra: serde_json::Map<String, Value>,
}

impl NewEvent {
    pub fn new(kind: impl Into<String>, data: Value) -> Self {
        NewEvent {
            kind: kind.into(),
            data,
            extra: Default::default(),
        }
    }
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.insert(key.to_owned(), value);
        self
    }
}

/// Event type names used by dsh-codex. The full upstream catalog is
/// `ref/deepseek-harness/docs/persistence-catalog.md`.
pub mod kinds {
    pub const TURN_START: &str = "turn/start";
    pub const TURN_END: &str = "turn/end";
    pub const STEP_START: &str = "step/start";
    pub const STEP_END: &str = "step/end";
    pub const SYSTEM_MESSAGE: &str = "system/message";
    pub const USER_MESSAGE: &str = "user/message";
    pub const ASSISTANT_MESSAGE: &str = "assistant/message";
    pub const TOOL_CALL: &str = "tool/call";
    pub const TOOL_RESULT: &str = "tool/result";
    pub const REQUEST_HEADER: &str = "request/header";
    pub const REQUEST_CONTEXT: &str = "request/context";
    pub const SESSION_TITLE: &str = "session/title";
    pub const INBOX_SPLICED: &str = "agent/inbox/spliced";
    pub const PERMISSION_PRESET: &str = "permission/preset";
    pub const SANDBOX_MODE: &str = "sandbox/mode";
    pub const APPROVAL_POLICY: &str = "approval/policy";
    pub const LLM_RETRY: &str = "llm/retry";
    pub const LLM_RETRY_STARTED: &str = "llm/retry-started";
    pub const TODO_WRITE: &str = "todo/write";
    pub const COMPACTION_START: &str = "compaction/start";
    pub const COMPACTION_SUMMARY: &str = "compaction/summary";
    pub const COMPACTION_PRUNE: &str = "compaction/prune";
    pub const COMPACTION_END: &str = "compaction/end";
    pub const IMAGE_OFFLOAD: &str = "image/offload";
    pub const APPROVAL_ASKED: &str = "approval/asked";
    pub const APPROVAL_DECIDED: &str = "approval/decided";
    pub const MODEL_SELECTION: &str = "model/selection";
    pub const JOB_REGISTERED: &str = "job/registered";
    pub const JOB_SETTLED: &str = "job/settled";
    pub const DEVELOPER_MESSAGE: &str = "developer/message";
    pub const WORKING_DIRECTORY_CHANGE: &str = "working-directory/change";
}

/// Append-only session log as the agent loop sees it. Implemented by
/// `dshx-session` (file-backed, format v4) and by in-memory fakes in tests.
#[async_trait::async_trait]
pub trait SessionLog: Send + Sync {
    fn id(&self) -> &str;
    fn header(&self) -> SessionHeader;
    /// Assigns `seq`/`time`, persists (buffered), broadcasts, returns the stored events.
    async fn append(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>>;
    /// Durability checkpoint (dsh flushes before each model request and tool dispatch).
    async fn flush(&self) -> anyhow::Result<()>;
    /// Snapshot of all events so far (without the header).
    fn events(&self) -> Vec<SessionEvent>;
    /// Live feed of appended events (for the UI and SDK clients).
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent>;
}
