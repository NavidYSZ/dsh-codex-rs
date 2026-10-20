//! Lifecycle notifications for hosts (dsh SDK `subagent.started` /
//! `subagent.finished`). Nothing here is persisted.
use serde::Serialize;
use serde_json::Value;

/// A child session was created or cold-resumed under a parent (dsh emits the
/// SDK notification on every child `session/created`, resumes included).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStarted {
    pub parent_session_id: String,
    pub child_session_id: String,
}

/// One child run (one-shot) or residency epoch (continuable) settled.
/// Serializes to the dsh SDK `subagent.finished` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentFinished {
    /// Delegation provider: `spawn` or `fork`.
    pub provider: String,
    pub agent_id: String,
    pub parent_session_id: String,
    pub child_session_id: String,
    /// `ok` for `completed`, otherwise `error`.
    pub status: String,
    /// `completed`, `aborted`, `error`, `max-tokens` or `refusal`.
    pub stop_reason: String,
    /// The child's final assistant content blocks, when it produced any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<Vec<Value>>,
}

impl SubagentFinished {
    /// dsh `successStatus`; `max_tokens_as_success` mirrors the SDK server option.
    pub fn status_for(stop_reason: &str, max_tokens_as_success: bool) -> &'static str {
        if stop_reason == "completed" || (stop_reason == "max-tokens" && max_tokens_as_success) {
            "ok"
        } else {
            "error"
        }
    }
}

/// Host hook for subagent lifecycle edges. Methods must not block.
pub trait SubagentObserver: Send + Sync {
    fn on_started(&self, event: &SubagentStarted) {
        let _ = event;
    }
    fn on_finished(&self, event: &SubagentFinished) {
        let _ = event;
    }
}
