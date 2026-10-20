//! Interrupted-turn repair (dsh `packages/core/session/src/repair.ts`): a log
//! whose writer died mid-turn gets synthetic closers — error results for
//! unanswered tool calls, then `step/end` and `turn/end {reason: interrupted}`.
use dshx_core::{NewEvent, SessionEvent};
use serde_json::{Map, Value, json};

/// Recovery code for an assistant tool request that never reached `tool/call`.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
/// Recovery code for a recorded `tool/call` without a durable result.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const STARTED_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";
const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";

#[derive(Debug, Clone)]
struct Pending {
    call_id: String,
    turn: Value,
    step: Value,
    call_seq: Option<u64>,
}

/// Tracks unanswered assistant tool requests (dsh `ToolCallRecovery`).
#[derive(Debug, Default, Clone)]
pub struct ToolCallRecovery {
    pending: Vec<Pending>,
    last: Option<(u64, u64)>,
}

impl ToolCallRecovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes the next committed event; closed steps and turn boundaries discard pending requests.
    pub fn observe(&mut self, event: &SessionEvent) {
        self.last = Some((event.seq, event.time));
        match event.kind.as_str() {
            "turn/start" | "turn/end" | "step/end" => self.pending.clear(),
            "assistant/message" => {
                let blocks = event
                    .data
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                for block in blocks.filter(|b| b.get("type") == Some(&Value::from("tool-call"))) {
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let entry = Pending {
                        call_id: id.to_owned(),
                        turn: event.data.get("turn").cloned().unwrap_or(Value::Null),
                        step: event.data.get("step").cloned().unwrap_or(Value::Null),
                        call_seq: None,
                    };
                    // Map semantics: a repeated id keeps its first position.
                    match self.pending.iter_mut().find(|p| p.call_id == id) {
                        Some(existing) => *existing = entry,
                        None => self.pending.push(entry),
                    }
                }
            }
            "tool/call" => {
                let id = event.data.get("callId").and_then(Value::as_str);
                if let Some(entry) = self
                    .pending
                    .iter_mut()
                    .find(|p| Some(p.call_id.as_str()) == id)
                {
                    entry.call_seq = Some(event.seq);
                }
            }
            "tool/result" => {
                let id = event
                    .data
                    .pointer("/message/source/callId")
                    .and_then(Value::as_str);
                let appended = event.extra.get("surfaceOp") == Some(&Value::from("append"));
                if let Some(pos) = self
                    .pending
                    .iter()
                    .position(|p| Some(p.call_id.as_str()) == id)
                    && appended
                    && Some(&self.pending[pos].turn) == event.data.get("turn")
                    && Some(&self.pending[pos].step) == event.data.get("step")
                {
                    self.pending.remove(pos);
                }
            }
            _ => {}
        }
    }

    /// Conservative error results in assistant order; seqs follow the last
    /// observed event and times reuse its time.
    pub fn results(&self) -> Vec<SessionEvent> {
        let Some((last_seq, time)) = self.last else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (seq, p) in (last_seq + 1..).zip(&self.pending) {
            let started = p.call_seq.is_some();
            let message = json!({
                "id": format!("interrupted-tool-result-{}-{seq}", p.call_id),
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
            let mut extra = Map::new();
            extra.insert("surfaceOp".into(), Value::from("append"));
            if let Some(call_seq) = p.call_seq {
                extra.insert("sourceEventSeqs".into(), json!([call_seq]));
            }
            out.push(SessionEvent {
                kind: "tool/result".into(),
                seq,
                time,
                data: json!({"turn": p.turn, "step": p.step, "message": message, "error": error}),
                extra,
            });
        }
        out
    }
}

/// Synthetic closers that balance a log whose final turn was interrupted;
/// empty for a balanced log (dsh `interruptedTurnClosers`).
pub fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<SessionEvent> {
    let mut open_turn: Option<Value> = None;
    let mut open_step: Option<Value> = None;
    let mut recovery = ToolCallRecovery::new();
    for event in events {
        recovery.observe(event);
        match event.kind.as_str() {
            "turn/start" => {
                open_turn = event.data.get("turn").cloned();
                open_step = None;
            }
            "turn/end" => {
                open_turn = None;
                open_step = None;
            }
            "step/start" => open_step = event.data.get("step").cloned(),
            "step/end" => open_step = None,
            _ => {}
        }
    }
    let (Some(turn), Some(last)) = (open_turn, events.last()) else {
        return Vec::new();
    };
    let mut closers = recovery.results();
    let mut seq = last.seq + closers.len() as u64 + 1;
    if let Some(step) = open_step {
        closers.push(SessionEvent {
            kind: "step/end".into(),
            seq,
            time: last.time,
            data: json!({"turn": turn, "step": step}),
            extra: Map::new(),
        });
        seq += 1;
    }
    closers.push(SessionEvent {
        kind: "turn/end".into(),
        seq,
        time: last.time,
        data: json!({"turn": turn, "reason": {"kind": "interrupted"}}),
        extra: Map::new(),
    });
    closers
}

/// Converts stored events back to appendable ones (seq/time are reassigned).
pub fn to_new_events(events: &[SessionEvent]) -> Vec<NewEvent> {
    events
        .iter()
        .map(|e| NewEvent {
            kind: e.kind.clone(),
            data: e.data.clone(),
            extra: e.extra.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 100 + seq,
            data,
            extra: Map::new(),
        }
    }

    #[test]
    fn balanced_log_needs_no_closers() {
        let events = vec![
            ev(0, "turn/start", json!({"turn":1})),
            ev(
                1,
                "turn/end",
                json!({"turn":1,"reason":{"kind":"completed"}}),
            ),
        ];
        assert!(interrupted_turn_closers(&events).is_empty());
        assert!(interrupted_turn_closers(&[]).is_empty());
    }

    #[test]
    fn closes_pending_calls_step_and_turn() {
        let assistant = json!({"turn":1,"step":1,"message":{"role":"assistant","content":[
            {"type":"tool-call","id":"a","name":"bash","arguments":"{}"},
            {"type":"tool-call","id":"b","name":"read","arguments":"{}"}]}});
        let events = vec![
            ev(0, "turn/start", json!({"turn":1})),
            ev(1, "step/start", json!({"turn":1,"step":1})),
            ev(2, "assistant/message", assistant),
            ev(
                3,
                "tool/call",
                json!({"turn":1,"step":1,"callId":"a","name":"bash","arguments":"{}"}),
            ),
        ];
        let closers = interrupted_turn_closers(&events);
        let kinds: Vec<&str> = closers.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["tool/result", "tool/result", "step/end", "turn/end"]
        );
        assert_eq!(
            closers.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [4, 5, 6, 7]
        );
        assert!(closers.iter().all(|e| e.time == 103));
        assert_eq!(closers[0].data["error"]["code"], TOOL_OUTCOME_UNKNOWN);
        assert_eq!(closers[0].extra["sourceEventSeqs"], json!([3]));
        assert_eq!(
            closers[0].data["message"]["id"],
            "interrupted-tool-result-a-4"
        );
        assert_eq!(closers[1].data["error"]["code"], TOOL_NOT_STARTED);
        assert!(!closers[1].extra.contains_key("sourceEventSeqs"));
        assert_eq!(closers[3].data["reason"]["kind"], "interrupted");
    }
}
