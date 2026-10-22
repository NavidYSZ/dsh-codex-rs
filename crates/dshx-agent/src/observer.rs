//! Live, non-persisted agent notifications (stream deltas and status).
//! Everything durable is in the session log; this is only for live UIs.
use serde::Serialize;

/// Externally visible agent state (matches the UI protocol's `status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum AgentStatus {
    Idle,
    Running,
    WaitingApproval,
    Error { error: String },
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
            AgentStatus::WaitingApproval => "waiting-approval",
            AgentStatus::Error { .. } => "error",
        }
    }
}

/// `text` or `reasoning`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeltaKind {
    Text,
    Reasoning,
}

/// One streamed fragment of an assistant block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamDelta {
    pub turn: u64,
    pub step: u64,
    /// Block index within the current model attempt.
    pub index: usize,
    #[serde(rename = "blockType")]
    pub block_type: DeltaKind,
    pub text: String,
}

/// Live callbacks. All methods default to no-ops and must not block.
pub trait AgentObserver: Send + Sync {
    fn on_delta(&self, delta: &StreamDelta) {
        let _ = delta;
    }
    fn on_status(&self, status: &AgentStatus) {
        let _ = status;
    }
    /// A model attempt started (`attempt` counts from 1 within the step). A new
    /// attempt after a failure discards the previous attempt's deltas.
    fn on_attempt(&self, turn: u64, step: u64, attempt: u32) {
        let _ = (turn, step, attempt);
    }
    /// The model started streaming a call of tool `name` as block `index`;
    /// its arguments follow until the assistant message lands.
    fn on_tool_call(&self, turn: u64, step: u64, index: usize, name: &str) {
        let _ = (turn, step, index, name);
    }
}

/// Observer that ignores everything.
pub struct NoopObserver;

impl AgentObserver for NoopObserver {}
