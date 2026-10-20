//! Conservative results for tool calls left unanswered by a failed step or a
//! crashed process (dsh `session/src/repair.ts`).
use dshx_core::{SessionEvent, kinds};
use serde_json::{Value, json};

pub(crate) const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
pub(crate) const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const STARTED_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";
const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";

struct Pending {
    call_id: String,
    turn: u64,
    step: u64,
    call_seq: Option<u64>,
}

/// Tracks assistant tool requests without a committed result.
#[derive(Default)]
pub(crate) struct ToolCallRecovery {
    pending: Vec<Pending>,
}

impl ToolCallRecovery {
    pub fn observe(&mut self, event: &SessionEvent) {
        match event.kind.as_str() {
            kinds::TURN_START | kinds::TURN_END | kinds::STEP_END => self.pending.clear(),
            kinds::ASSISTANT_MESSAGE => {
                let turn = event.data["turn"].as_u64().unwrap_or(0);
                let step = event.data["step"].as_u64().unwrap_or(0);
                if let Some(content) = event.data["message"]["content"].as_array() {
                    for b in content {
                        if b["type"] == "tool-call"
                            && let Some(id) = b["id"].as_str()
                        {
                            self.pending.retain(|p| p.call_id != id);
                            self.pending.push(Pending {
                                call_id: id.to_owned(),
                                turn,
                                step,
                                call_seq: None,
                            });
                        }
                    }
                }
            }
            kinds::TOOL_CALL => {
                if let Some(id) = event.data["callId"].as_str()
                    && let Some(p) = self.pending.iter_mut().find(|p| p.call_id == id)
                {
                    p.call_seq = Some(event.seq);
                }
            }
            kinds::TOOL_RESULT => {
                let id = event.data["message"]["source"]["callId"]
                    .as_str()
                    .unwrap_or_default();
                let append = event.extra.get("surfaceOp") == Some(&json!("append"));
                let turn = event.data["turn"].as_u64();
                let step = event.data["step"].as_u64();
                self.pending.retain(|p| {
                    !(append && p.call_id == id && Some(p.turn) == turn && Some(p.step) == step)
                });
            }
            _ => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// `tool/result` payloads for every unanswered call, in assistant order.
    /// `seq_of(i)` predicts the seq the i-th result will be appended at (it names the message).
    pub fn results(&self, mut seq_of: impl FnMut(usize) -> u64) -> Vec<(Value, Option<Vec<u64>>)> {
        self.pending
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let started = p.call_seq.is_some();
                let message = json!({
                    "id": format!("interrupted-tool-result-{}-{}", p.call_id, seq_of(i)),
                    "role": "tool",
                    "toolCallId": p.call_id,
                    "isError": true,
                    "source": {"kind": "tool", "callId": p.call_id},
                    "content": [{"type": "text", "text": if started { STARTED_TEXT } else { NOT_STARTED_TEXT }}],
                });
                let error = if started {
                    json!({"name": "ToolOutcomeUnknownError", "code": TOOL_OUTCOME_UNKNOWN})
                } else {
                    json!({"name": "ToolNotStartedError", "code": TOOL_NOT_STARTED})
                };
                let data = json!({"turn": p.turn, "step": p.step, "message": message, "error": error});
                (data, p.call_seq.map(|s| vec![s]))
            })
            .collect()
    }
}
