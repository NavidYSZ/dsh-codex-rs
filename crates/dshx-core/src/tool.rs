//! Tools the model can call.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::attachment::{AttachmentStore, ImageAttachmentRef};
use crate::event::SessionEvent;
use crate::llm::ToolSchema;
use crate::message::Message;

/// File-system policy for tools (dsh sandbox modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

/// Asks the user to allow one call with wider access. Implemented by the
/// front end (web UI, SDK client); headless runs use a fixed answer.
#[async_trait]
pub trait Approver: Send + Sync {
    /// `summary` is a one-line description of the call, `justification` the model's reason.
    async fn approve(
        &self,
        call_id: &str,
        tool: &str,
        summary: &str,
        justification: Option<&str>,
    ) -> ApprovalDecision;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalDecision {
    AllowedOnce,
    Rejected,
    Cancelled,
    Unavailable,
}

/// Everything a tool call may use. Cheap to clone.
#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub call_id: String,
    /// Workspace root; relative paths resolve against it.
    pub cwd: PathBuf,
    pub sandbox: SandboxMode,
    pub cancel: CancellationToken,
    pub attachments: Arc<dyn AttachmentStore>,
    /// Whether the current model can see images (screenshots are only attached then).
    pub model_accepts_images: bool,
    pub approver: Arc<dyn Approver>,
    /// Per-session shared state between tools (e.g. read-before-write tracking).
    pub state: Arc<dyn std::any::Any + Send + Sync>,
    /// The agent the call runs in (dsh `exec.agent`); `None` outside an agent loop.
    pub caller: Option<CallerInfo>,
}

/// Read access to the calling agent's own session log.
pub trait CallerSession: Send + Sync {
    /// Events with `seq >= from`, in order.
    fn events_from(&self, from: u64) -> Vec<SessionEvent>;
}

/// What a tool knows about its calling agent: dsh's `agents.roots()`
/// membership and the session's open-turn boundary, which policy tools
/// (goals) read to tell direct human input from automatic continuation.
#[derive(Clone)]
pub struct CallerInfo {
    /// A top-level agent: its session is no delegated child (a branch,
    /// which names its source as `parentSession`, still is one).
    pub root: bool,
    /// The open turn and the `seq` of its `turn/start`.
    pub turn: u64,
    pub turn_start_seq: u64,
    pub session: Arc<dyn CallerSession>,
}

impl CallerInfo {
    /// Events of the open turn after its `turn/start`, up to now.
    pub fn open_turn_events(&self) -> Vec<SessionEvent> {
        self.session.events_from(self.turn_start_seq + 1)
    }
}

/// Content of a tool result as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ToolResultPart {
    Text { text: String },
    Image { attachment: ImageAttachmentRef },
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub content: Vec<ToolResultPart>,
    pub is_error: bool,
    /// Structured result value (dsh tool `output.schema`), kept in the session log.
    pub value: Option<Value>,
    /// Data for the transcript row in the UI (dsh `presentationMeta`).
    pub meta: Option<Value>,
    /// Messages for the next step of the turn (dsh `exec.deferContext`).
    pub contexts: Vec<Message>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        ToolOutput {
            content: vec![ToolResultPart::Text { text: text.into() }],
            ..Default::default()
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        ToolOutput {
            content: vec![ToolResultPart::Text { text: text.into() }],
            is_error: true,
            ..Default::default()
        }
    }
}

/// A failure that the model sees as an error result (never crashes the loop).
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    InvalidArguments(String),
    #[error("{0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// A typed failure (dsh `HarnessError`): the model sees `message`, the
    /// log records `{name, code}` beside the result.
    #[error("{message}")]
    Coded {
        name: String,
        code: String,
        message: String,
    },
}

impl ToolError {
    pub fn coded(name: &str, code: &str, message: impl Into<String>) -> Self {
        ToolError::Coded {
            name: name.to_owned(),
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    /// Safe to run in parallel with other concurrency-safe calls (read-only tools).
    fn concurrency_safe(&self) -> bool {
        false
    }
    /// Hard limit armed by the agent loop; `None` = no limit beyond the tool's own.
    fn timeout(&self) -> Option<Duration> {
        None
    }
    /// One-line title for the transcript row, e.g. `Read src/main.rs`.
    fn describe(&self, args: &Value) -> String {
        let _ = args;
        self.schema().name
    }
    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;
}

/// The set of tools offered to the model, in declaration order.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name;
        self.tools.retain(|t| t.schema().name != name);
        self.tools.push(tool);
    }
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.schema().name == name).cloned()
    }
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.iter().map(|t| t.schema()).collect()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }
}
