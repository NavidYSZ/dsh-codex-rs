//! Tools the model can call.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::attachment::{AttachmentStore, ImageAttachmentRef};
use crate::llm::ToolSchema;

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
