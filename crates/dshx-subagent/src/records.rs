//! The durable and model-visible vocabulary of dsh's subagent package:
//! `subagent/descriptor`, `subagent/catalog`, adjacent-agent messages,
//! return guidance, settlement notices, and the folds that read a child's
//! own events back into a result (`assistant-output.ts`, `lifecycle.ts`,
//! `foldConsumedWork`).
use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent, SessionHeader};
use serde_json::{Map, Value, json};

pub(crate) const DESCRIPTOR: &str = "subagent/descriptor";
pub(crate) const CATALOG: &str = "subagent/catalog";
pub(crate) const END_SEED: &str = "session/end-seed";
/// `SUBAGENT_DESCRIPTOR_VERSION`.
const DESCRIPTOR_VERSION: u64 = 3;
/// `SUBAGENT_CATALOG_VERSION` of live child creation.
const CATALOG_VERSION: u64 = 0;

/// The runtime-context statement every in-process child receives.
pub const DELEGATION_CONTEXT: &str = "You are a delegated subagent: your permission scope was fixed when you were started and cannot be widened from inside this session — operations that require approval are rejected automatically. When the task needs access beyond that scope, do not retry the denied operation; state the limitation in your reply so the delegating agent can handle it.";
pub(crate) const DELEGATION_CONTEXT_NAME: &str = "subagent:delegation";

/// One-shot descriptor (`mode: one-shot`).
pub(crate) fn one_shot_descriptor(provider: &str, label: &str) -> Value {
    json!({"version": DESCRIPTOR_VERSION, "mode": "one-shot", "provider": provider, "label": label})
}

/// Continuable descriptor with the resolved child route for cold resume.
pub(crate) fn continuable_descriptor(
    provider: &str,
    label: &str,
    agent_provider: &str,
    agent_model: &str,
    agent_effort: Option<&str>,
) -> Value {
    let mut d = Map::new();
    d.insert("version".into(), DESCRIPTOR_VERSION.into());
    d.insert("mode".into(), "continuable".into());
    d.insert("provider".into(), provider.into());
    d.insert("label".into(), label.into());
    d.insert("agentProvider".into(), agent_provider.into());
    d.insert("agentModel".into(), agent_model.into());
    if let Some(effort) = agent_effort {
        d.insert("agentReasoningEffort".into(), effort.into());
    }
    Value::Object(d)
}

/// The parent's direct-child discovery fact.
pub(crate) fn catalog_fact(child: &SessionHeader, mode: &str, label: &str) -> Value {
    json!({
        "version": CATALOG_VERSION,
        "childId": child.id,
        "childCreatedAt": child.created_at,
        "mode": mode,
        "label": label,
    })
}

/// Length of the fork-inherited prefix: the position of the last
/// `session/end-seed {inherited: true}` marker of a seeded session.
pub(crate) fn inherited_cut(header: &SessionHeader, events: &[SessionEvent]) -> usize {
    if !header.is_seeded {
        return 0;
    }
    events
        .iter()
        .rposition(|e| e.kind == END_SEED && e.data["inherited"] == true)
        .unwrap_or(0)
}

/// The first own `subagent/descriptor` of a child log.
pub(crate) fn own_descriptor(header: &SessionHeader, events: &[SessionEvent]) -> Option<Value> {
    let cut = inherited_cut(header, events);
    events[cut..]
        .iter()
        .find(|e| e.kind == DESCRIPTOR)
        .map(|e| e.data.clone())
        .filter(|d| d["version"] == DESCRIPTOR_VERSION)
}

/// One direct-child row of a parent's catalog projection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatalogEntry {
    pub id: String,
    pub mode: String,
    pub label: Option<String>,
}

/// The parent-owned catalog (`subagentCatalog`), inherited facts excluded.
pub(crate) fn catalog_entries(
    header: &SessionHeader,
    events: &[SessionEvent],
) -> Vec<CatalogEntry> {
    let cut = inherited_cut(header, events) as u64;
    events
        .iter()
        .filter(|e| e.kind == CATALOG && e.seq >= cut)
        .filter_map(|e| {
            Some(CatalogEntry {
                id: e.data["childId"].as_str()?.to_owned(),
                mode: e.data["mode"].as_str()?.to_owned(),
                label: e.data["label"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

fn user_message(content: Vec<ContentBlock>, source: MessageSource) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content,
        source,
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// A prompt or host message with `source: {kind: "user"}`.
pub(crate) fn prompt_message(content: Vec<ContentBlock>) -> Message {
    user_message(content, MessageSource::User)
}

/// dsh `createAgentMessage`.
pub(crate) fn agent_message(sender: &str, text: &str) -> Message {
    user_message(
        vec![
            ContentBlock::text(format!("Agent {sender} sent a message: ")),
            ContentBlock::text(text),
        ],
        MessageSource::Other(
            json!({"kind": "agent-message", "form": "relay", "senderSessionId": sender}),
        ),
    )
}

/// dsh `withContinuableReturnGuidance` (the block after the task).
pub(crate) fn return_guidance(parent: &str) -> String {
    let id = serde_json::to_string(parent).unwrap_or_default();
    format!(
        "Your parent agent id is {id}. Before you finish, send your result to that agent with send_message({{ agent_id: {id}, message: \"<self-contained result>\" }}). The parent shares your workspace but does not automatically receive your transcript, tool output, or reasoning. Send earlier messages as well when a finding changes what the parent should do next; sending a message does not end your turn."
    )
}

/// dsh `boundContextSummary` (120 characters).
fn bound_summary(summary: &str) -> String {
    if summary.chars().count() <= 120 {
        summary.to_owned()
    } else {
        let cut: String = summary.chars().take(119).collect();
        format!("{cut}…")
    }
}

fn settlement_summary(child: &str, stop_reason: &str) -> String {
    let subject = format!("Background subagent {child}");
    match stop_reason {
        "completed" => {
            format!("{subject} finished and will do no further work unless you send it more.")
        }
        "aborted" => format!("{subject} was stopped before it finished."),
        "max-tokens" => format!("{subject} ran out of room before it finished."),
        "refusal" => format!("{subject} declined the task."),
        "error" => format!("{subject} failed before it finished."),
        other => format!("{subject} ended abnormally ({other}) before it finished."),
    }
}

/// dsh `createSettlementMessage`: the parent's notice for one settled epoch.
pub(crate) fn settlement_message(
    child: &str,
    stop_reason: &str,
    output: Option<&[Value]>,
) -> Message {
    let summary = settlement_summary(child, stop_reason);
    let closing: Vec<String> = output
        .unwrap_or_default()
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    let mut content = vec![ContentBlock::text(summary.clone())];
    if closing.is_empty() {
        content.push(ContentBlock::text("It left no closing message."));
    } else {
        content.push(ContentBlock::text("Its closing message:"));
        content.extend(closing.into_iter().map(ContentBlock::text));
    }
    user_message(
        content,
        MessageSource::Other(json!({
            "kind": "subagent-settled",
            "form": "notice",
            "summary": bound_summary(&summary),
            "senderSessionId": child,
        })),
    )
}

/// dsh `joinAssistantStreamText` over a persisted stream snapshot.
fn stream_text(stream: &Value) -> String {
    let mut out = String::new();
    for record in stream.as_array().into_iter().flatten() {
        match record["type"].as_str() {
            Some("text-chunks") => {
                for t in record["texts"].as_array().into_iter().flatten() {
                    out.push_str(t.as_str().unwrap_or_default());
                }
            }
            Some("chunk") if record["chunk"]["type"] == "text-delta" => {
                out.push_str(record["chunk"]["text"].as_str().unwrap_or_default());
            }
            _ => {}
        }
    }
    out
}

/// dsh `finalAssistantOutput`: the last non-empty assistant message, else
/// the streamed text of every assistant message and attempt.
pub(crate) fn final_output(events: &[SessionEvent]) -> Option<Vec<Value>> {
    let mut message: Option<Vec<Value>> = None;
    let mut partial = String::new();
    for e in events {
        let assistant = e.kind == "assistant/message";
        if assistant
            && let Some(content) = e.data["message"]["content"].as_array()
            && !content.is_empty()
        {
            message = Some(content.clone());
        }
        if assistant || e.kind == "assistant/attempt" {
            partial.push_str(&stream_text(&e.data["stream"]));
        }
    }
    message
        .or_else(|| (!partial.is_empty()).then(|| vec![json!({"type": "text", "text": partial})]))
}

/// Concatenated text blocks (dsh `finalText` / `outputValueText`).
pub(crate) fn output_text(output: &[Value]) -> String {
    output
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect()
}

/// dsh `accountsForClaim`.
fn accounts_for_claim(reason: &Value) -> bool {
    reason["kind"] != "completed"
}

/// dsh `foldConsumedWork`: the turn ending that accounts for the work an
/// event suffix consumed, and whether accepted work was dropped unrun after it.
pub(crate) fn consumed_work(events: &[SessionEvent]) -> (Option<Value>, bool) {
    let mut stepped = std::collections::HashSet::new();
    let mut claimed = std::collections::HashSet::new();
    let mut open: Option<u64> = None;
    let mut end: Option<Value> = None;
    let mut dropped = false;
    for e in events {
        match e.kind.as_str() {
            "turn/start" => open = e.data["turn"].as_u64(),
            "step/start" => {
                if let Some(t) = e.data["turn"].as_u64() {
                    stepped.insert(t);
                }
            }
            "agent/inbox/spliced" => {
                if e.data.get("removedCount").is_none() {
                    continue;
                }
                if e.data["outcome"] == "canceled" {
                    dropped |= e.data["inserted"].as_array().is_none_or(|a| a.is_empty());
                } else if let Some(t) = open {
                    claimed.insert(t);
                }
            }
            "turn/end" => {
                open = None;
                let turn = e.data["turn"].as_u64().unwrap_or(0);
                let reason = &e.data["reason"];
                if stepped.remove(&turn) || (claimed.remove(&turn) && accounts_for_claim(reason)) {
                    end = Some(reason.clone());
                    dropped = false;
                }
            }
            _ => {}
        }
    }
    (end, dropped)
}

/// dsh in-process driver `toStopReason` (one-shot runs).
pub(crate) fn one_shot_stop_reason(end: Option<&Value>) -> &'static str {
    match end.and_then(|r| r["kind"].as_str()) {
        Some("completed") => "completed",
        Some("max-tokens") => "max-tokens",
        Some("aborted") => "aborted",
        Some("blocked") => "refusal",
        _ => "error",
    }
}

/// dsh `epochStopReason` (continuable residency epochs).
pub(crate) fn epoch_stop_reason(events: &[SessionEvent]) -> &'static str {
    let (end, dropped) = consumed_work(events);
    match end.as_ref().and_then(|r| r["kind"].as_str()) {
        Some("max-tokens") => "max-tokens",
        Some("aborted") | Some("interrupted") => "aborted",
        Some("error") => "error",
        Some("blocked") => "refusal",
        None | Some("completed") => {
            if dropped {
                "aborted"
            } else {
                "completed"
            }
        }
        Some(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 0,
            data,
            extra: Default::default(),
        }
    }

    #[test]
    fn guidance_quotes_the_parent_id() {
        let text = return_guidance("session-1");
        assert!(text.starts_with("Your parent agent id is \"session-1\". Before you finish, send your result to that agent with send_message({ agent_id: \"session-1\", message: \"<self-contained result>\" })."));
    }

    #[test]
    fn settlement_notice_like_dsh() {
        let m = settlement_message("c1", "aborted", None);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(
            v["content"][0]["text"],
            "Background subagent c1 was stopped before it finished."
        );
        assert_eq!(v["content"][1]["text"], "It left no closing message.");
        assert_eq!(v["source"]["kind"], "subagent-settled");
        let out = vec![
            json!({"type": "reasoning", "text": "x"}),
            json!({"type": "text", "text": "DONE"}),
        ];
        let m = settlement_message("c1", "completed", Some(&out));
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"][1]["text"], "Its closing message:");
        assert_eq!(v["content"][2]["text"], "DONE");
        assert_eq!(v["content"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn consumed_work_and_stop_reasons() {
        let events = vec![
            ev(0, "turn/start", json!({"turn": 1})),
            ev(
                1,
                "agent/inbox/spliced",
                json!({"target": "next-turn", "start": 0, "removedCount": 1, "inserted": []}),
            ),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "aborted", "reason": {"kind": "parent"}}}),
            ),
        ];
        assert_eq!(epoch_stop_reason(&events), "aborted");
        let (end, _) = consumed_work(&events);
        assert_eq!(one_shot_stop_reason(end.as_ref()), "aborted");
        assert_eq!(epoch_stop_reason(&[]), "completed");
        assert_eq!(one_shot_stop_reason(None), "error");
    }

    #[test]
    fn final_output_prefers_the_last_nonempty_message() {
        let events = vec![
            ev(
                0,
                "assistant/message",
                json!({"message": {"content": [{"type": "text", "text": "A"}]}, "stream": []}),
            ),
            ev(
                1,
                "assistant/message",
                json!({"message": {"content": []}, "stream": []}),
            ),
        ];
        assert_eq!(final_output(&events).unwrap()[0]["text"], "A");
        let attempt = vec![ev(
            0,
            "assistant/attempt",
            json!({"stream": [{"type": "text-chunks", "texts": ["pa", "rt"]}]}),
        )];
        assert_eq!(final_output(&attempt).unwrap()[0]["text"], "part");
    }
}
