//! Small builders for session events and the shared value types the loop logs.
use dshx_core::{LlmFailure, Message, NewEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) fn event(kind: &str, data: Value) -> NewEvent {
    NewEvent::new(kind, data)
}

/// A surface event appended to the tail (`surfaceOp: "append"`), optionally citing sources.
pub(crate) fn surface_append(kind: &str, data: Value, sources: Option<Vec<u64>>) -> NewEvent {
    let mut e = NewEvent::new(kind, data);
    if let Some(s) = sources {
        e = e.with("sourceEventSeqs", json!(s));
    }
    e.with("surfaceOp", json!("append"))
}

/// A surface event replacing the inclusive node range `start..=end`.
pub(crate) fn surface_replace(
    kind: &str,
    data: Value,
    start: u64,
    end: u64,
    sources: Vec<u64>,
) -> NewEvent {
    NewEvent::new(kind, data)
        .with("sourceEventSeqs", json!(sources))
        .with(
            "surfaceOp",
            json!({"op": "replace", "startSeq": start, "endSeq": end}),
        )
}

pub(crate) fn message_json(m: &Message) -> Value {
    serde_json::to_value(m).unwrap_or(Value::Null)
}

/// Why a running turn was cancelled (dsh `AgentCancelCause`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CancelCause {
    User,
    Parent,
    Hook { reason: String },
    Disposed,
}

/// Why a turn ended (dsh `TurnEndReason`), as logged in `turn/end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndReason {
    Completed,
    Aborted { reason: CancelCause },
    Blocked,
    Error { error: LlmFailure },
    MaxTokens,
    Interrupted,
}

/// Result of [`crate::Agent::run_turn`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnOutcome {
    /// The turn number, or `None` when the input was cancelled before its turn opened.
    pub turn: Option<u64>,
    pub reason: TurnEndReason,
}

/// A model-request failure that ends a turn with its dsh facts.
#[derive(Debug, Clone)]
pub(crate) struct LlmError(pub LlmFailure);

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.0.message, self.0.code)
    }
}

impl std::error::Error for LlmError {}

/// Cancellation reached a checkpoint.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("aborted")
    }
}

impl std::error::Error for Aborted {}

/// dsh `errorChain`: the message plus its causes.
pub(crate) fn error_chain(e: &anyhow::Error) -> String {
    let parts: Vec<String> = e.chain().map(|c| c.to_string()).collect();
    parts.join(": ")
}
