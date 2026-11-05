//! The pure state machine between Pi's RPC events and Aether's session log.
//!
//! An Aether turn runs from the `prompt` Aether sends to Pi's
//! `agent_settled`; an Aether step is one Pi turn (opened lazily at the first
//! assistant output, so a steered user message lands before its step like in
//! dsh). Every log event of a Pi chat comes from here, which is how the
//! mapper knows each event's `seq` (tool results cite their call, the
//! compaction checkpoint cites the span it shadows). The queue (`next-turn`
//! for prompts, `next-step` for steers) lives here too and is written as
//! `agent/inbox/spliced`, so the UI's queue and the input's durability work
//! exactly like in a DSH chat.
use std::collections::HashMap;
use std::path::PathBuf;

use dshx_core::{
    ContentBlock, LlmFailure, Message, MessageSource, NewEvent, Role, SessionEvent, kinds,
};
use serde_json::{Map, Value, json};

/// The Aether names of a Pi model (`deepseek` → `deepseek-official`).
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub provider: String,
    pub model: String,
    pub context_window: Option<u64>,
}

/// One live stream delta (the UI's `delta` message).
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub turn: u64,
    pub step: u64,
    pub index: usize,
    /// `text`, `reasoning` or `tool-call` (with the tool name as text).
    pub block_type: &'static str,
    pub text: String,
}

/// How a settled run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Settled {
    Completed,
    Aborted,
    Error(String),
    MaxTokens,
}

#[derive(Debug, Default)]
pub struct Output {
    pub events: Vec<NewEvent>,
    pub deltas: Vec<Delta>,
    /// Set by `agent_settled`: the Aether turn is closed.
    pub settled: Option<Settled>,
    /// A dialog Pi waits on (`extension_ui_request` id); the MVP cancels it.
    pub dialog: Option<String>,
}

/// Where a queued input waits (dsh inbox targets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    NextTurn,
    NextStep,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Target::NextTurn => "next-turn",
            Target::NextStep => "next-step",
        }
    }
}

/// A change to one queued prompt.
#[derive(Debug, Clone)]
pub enum QueueChange {
    Edit(String),
    Remove,
    Steer,
}

/// Why Aether itself closes a turn.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    Aborted,
    Error { code: String, message: String },
    Interrupted,
}

#[derive(Debug)]
struct Call {
    id: String,
    name: String,
    raw: String,
    call_seq: Option<u64>,
}

#[derive(Debug)]
struct Turn {
    n: u64,
    step: u64,
    step_open: bool,
    /// The next-turn entry this turn was started for, until Pi echoes it.
    active: Option<String>,
    claimed: bool,
    calls: Vec<Call>,
    failure: Option<LlmFailure>,
    max_tokens: bool,
    /// The step's last request failed; a retry continues the same step.
    retrying: bool,
    retry: Option<String>,
}

#[derive(Debug)]
struct Compacting {
    id: String,
    turn: Option<u64>,
    /// Seq of `compaction/start` once written (a quiet one waits for success).
    start: Option<u64>,
}

/// A surface node: its seq and whether it is a human prompt.
#[derive(Debug, Clone, Copy)]
struct Node {
    seq: u64,
    human: bool,
}

pub struct Mapper {
    cwd: PathBuf,
    next_seq: u64,
    last_turn: u64,
    turn: Option<Turn>,
    next_turn: Vec<Message>,
    next_step: Vec<Message>,
    surface: Vec<Node>,
    titled: bool,
    context: Option<Value>,
    routes: HashMap<(String, String), Route>,
    provider: Option<Route>,
    compaction: Option<Compacting>,
    quiet_next: bool,
    cursor: Option<String>,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The text Pi receives for (and echoes back from) an Aether message.
pub fn prompt_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_human(message: &Value) -> bool {
    message.pointer("/source/kind").and_then(Value::as_str) == Some("user")
}

/// Pi's usage in Aether's `TokenUsage` shape plus `costUsd`.
pub fn usage_json(usage: &Value) -> Option<Value> {
    let usage = usage.as_object()?;
    let n = |k: &str| usage.get(k).and_then(Value::as_u64);
    let mut out = Map::new();
    out.insert("inputTokens".into(), n("input").unwrap_or(0).into());
    out.insert("outputTokens".into(), n("output").unwrap_or(0).into());
    if let Some(t) = n("totalTokens") {
        out.insert("totalTokens".into(), t.into());
    }
    if let Some(t) = n("cacheRead") {
        out.insert("cacheReadTokens".into(), t.into());
    }
    if let Some(t) = n("cacheWrite") {
        out.insert("cacheWriteTokens".into(), t.into());
    }
    if let Some(t) = n("reasoning") {
        out.insert("reasoningTokens".into(), t.into());
    }
    if let Some(cost) = usage
        .get("cost")
        .and_then(|c| c.get("total"))
        .and_then(Value::as_f64)
    {
        out.insert("costUsd".into(), json!(cost));
    }
    Some(Value::Object(out))
}

/// A short title from the first prompt (like dsh's fallback title).
pub fn fallback_title(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(8).collect();
    let mut title = words.join(" ");
    if title.len() > 80 {
        let mut end = 80;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
    }
    title
}

fn failure_of(message: &str) -> LlmFailure {
    let lower = message.to_lowercase();
    let code = if lower.contains("401") || lower.contains("auth") || lower.contains("api key") {
        "AUTH"
    } else if lower.contains("429") || lower.contains("rate limit") {
        "RATE_LIMIT"
    } else if lower.contains("context") && (lower.contains("length") || lower.contains("window")) {
        "CONTEXT_WINDOW_EXCEEDED"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "TIMEOUT"
    } else {
        "PROVIDER_ERROR"
    };
    LlmFailure::new(code, message)
}

impl Mapper {
    /// A mapper for an empty log.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Mapper {
            cwd: cwd.into(),
            next_seq: 0,
            last_turn: 0,
            turn: None,
            next_turn: Vec::new(),
            next_step: Vec::new(),
            surface: Vec::new(),
            titled: false,
            context: None,
            routes: HashMap::new(),
            provider: None,
            compaction: None,
            quiet_next: false,
            cursor: None,
        }
    }

    /// A mapper continuing `events` (a log `SessionStore::resume` repaired:
    /// no open turn).
    pub fn resume(cwd: impl Into<PathBuf>, events: &[SessionEvent]) -> Self {
        let mut m = Mapper::new(cwd);
        m.next_seq = events.len() as u64;
        for e in events {
            match e.kind.as_str() {
                kinds::TURN_START => {
                    m.last_turn = m.last_turn.max(e.data["turn"].as_u64().unwrap_or(0))
                }
                kinds::SESSION_TITLE => m.titled = true,
                kinds::REQUEST_CONTEXT => m.context = Some(e.data.clone()),
                kinds::INBOX_SPLICED => {
                    let list = match e.data["target"].as_str() {
                        Some("next-turn") => &mut m.next_turn,
                        Some("next-step") => &mut m.next_step,
                        _ => continue,
                    };
                    let start = (e.data["start"].as_u64().unwrap_or(0) as usize).min(list.len());
                    let removed = (e.data["removedCount"].as_u64().unwrap_or(0) as usize)
                        .min(list.len() - start);
                    let inserted: Vec<Message> = e.data["inserted"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect();
                    list.splice(start..start + removed, inserted);
                }
                "pi/cursor" => m.cursor = e.data["entryId"].as_str().map(str::to_owned),
                _ => {}
            }
        }
        // Steers that never reached Pi wait for the next turn, first.
        let steers = std::mem::take(&mut m.next_step);
        let fold = dshx_session::fold_lenient(events);
        m.surface = fold
            .nodes()
            .iter()
            .map(|seq| Node {
                seq: *seq,
                human: events
                    .get(*seq as usize)
                    .is_some_and(|e| e.kind == kinds::USER_MESSAGE && is_human(&e.data)),
            })
            .collect();
        m.next_step = steers;
        m
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The turn open right now.
    pub fn running(&self) -> Option<u64> {
        self.turn.as_ref().map(|t| t.n)
    }

    pub fn queued_turns(&self) -> &[Message] {
        &self.next_turn
    }

    pub fn queued_steers(&self) -> &[Message] {
        &self.next_step
    }

    /// The last mirrored Pi entry (`pi/cursor`).
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// Context window and Aether names of `(pi provider, pi model)`.
    pub fn set_route(&mut self, pi_provider: &str, pi_model: &str, route: Route) {
        self.routes
            .insert((pi_provider.to_owned(), pi_model.to_owned()), route);
    }

    /// The next compaction is Aether's own turn-end compaction: it shows
    /// only when it succeeds.
    pub fn quiet_next_compaction(&mut self) {
        self.quiet_next = true;
    }

    fn route(&self, provider: &str, model: &str) -> Route {
        self.routes
            .get(&(provider.to_owned(), model.to_owned()))
            .cloned()
            .unwrap_or_else(|| Route {
                provider: provider.to_owned(),
                model: model.to_owned(),
                context_window: None,
            })
    }

    fn emit(&mut self, out: &mut Vec<NewEvent>, event: NewEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if event.extra.get("surfaceOp") == Some(&json!("append")) {
            let human = event.kind == kinds::USER_MESSAGE && is_human(&event.data);
            self.surface.push(Node { seq, human });
        }
        out.push(event);
        seq
    }

    fn surface_append(kind: &str, data: Value, sources: Option<Vec<u64>>) -> NewEvent {
        let mut e = NewEvent::new(kind, data);
        if let Some(s) = sources {
            e = e.with("sourceEventSeqs", json!(s));
        }
        e.with("surfaceOp", json!("append"))
    }

    fn splice(
        &mut self,
        out: &mut Vec<NewEvent>,
        target: Target,
        start: usize,
        removed: usize,
        inserted: &[Message],
        canceled: bool,
    ) {
        let mut data = json!({
            "target": target.as_str(),
            "start": start,
            "inserted": inserted.iter().map(|m| serde_json::to_value(m).unwrap_or(Value::Null)).collect::<Vec<_>>(),
        });
        if removed > 0 {
            data["removedCount"] = removed.into();
            if canceled {
                data["outcome"] = "canceled".into();
            }
        }
        self.emit(out, NewEvent::new(kinds::INBOX_SPLICED, data));
    }

    // ---- the queue ----------------------------------------------------------

    /// Queues an accepted input durably (end of `target`).
    pub fn enqueue(&mut self, target: Target, message: Message) -> Vec<NewEvent> {
        let mut out = Vec::new();
        let list = match target {
            Target::NextTurn => &self.next_turn,
            Target::NextStep => &self.next_step,
        };
        let at = list.len();
        self.splice(
            &mut out,
            target,
            at,
            0,
            std::slice::from_ref(&message),
            false,
        );
        match target {
            Target::NextTurn => self.next_turn.push(message),
            Target::NextStep => self.next_step.push(message),
        }
        out
    }

    /// Opens a turn for the front of the next-turn queue; the message stays
    /// queued until Pi echoes it. `None` while a turn runs or nothing waits.
    pub fn begin_turn(&mut self) -> Option<(Vec<NewEvent>, Message)> {
        if self.turn.is_some() {
            return None;
        }
        let message = self.next_turn.first()?.clone();
        let mut out = Vec::new();
        let n = self.last_turn + 1;
        self.last_turn = n;
        self.emit(
            &mut out,
            NewEvent::new(kinds::TURN_START, json!({ "turn": n })),
        );
        self.turn = Some(Turn {
            n,
            step: 0,
            step_open: false,
            active: Some(message.id.clone()),
            claimed: false,
            calls: Vec::new(),
            failure: None,
            max_tokens: false,
            retrying: false,
            retry: None,
        });
        Some((out, message))
    }

    /// Edits, removes or steers one queued prompt. A steer returns the
    /// message to hand to Pi.
    pub fn change_queued(
        &mut self,
        id: &str,
        change: QueueChange,
    ) -> anyhow::Result<(Vec<NewEvent>, Option<Message>)> {
        let active = self.turn.as_ref().and_then(|t| t.active.clone());
        let index = self
            .next_turn
            .iter()
            .position(|m| m.id == id)
            .filter(|_| active.as_deref() != Some(id))
            .ok_or_else(|| anyhow::anyhow!("queued item is no longer pending"))?;
        let mut out = Vec::new();
        match change {
            QueueChange::Edit(text) => {
                let mut edited = self.next_turn[index].clone();
                let images: Vec<ContentBlock> = edited
                    .content
                    .into_iter()
                    .filter(|b| matches!(b, ContentBlock::Image { .. }))
                    .collect();
                edited.content = vec![ContentBlock::text(text)];
                edited.content.extend(images);
                self.splice(
                    &mut out,
                    Target::NextTurn,
                    index,
                    1,
                    std::slice::from_ref(&edited),
                    false,
                );
                self.next_turn[index] = edited;
                Ok((out, None))
            }
            QueueChange::Remove => {
                self.splice(&mut out, Target::NextTurn, index, 1, &[], true);
                self.next_turn.remove(index);
                Ok((out, None))
            }
            QueueChange::Steer => {
                anyhow::ensure!(self.turn.is_some(), "no turn is running to steer into");
                let message = self.next_turn.remove(index);
                self.splice(&mut out, Target::NextTurn, index, 1, &[], false);
                out.extend(self.enqueue(Target::NextStep, message.clone()));
                Ok((out, Some(message)))
            }
        }
    }

    /// After Pi's `clear_queue`: steers Pi still held go back to the front
    /// of the next-turn queue (in Pi's order), visible and not lost.
    pub fn restore_steers(&mut self, texts: &[String]) -> Vec<NewEvent> {
        let mut moved = Vec::new();
        for text in texts {
            if let Some(i) = self.next_step.iter().position(|m| &prompt_text(m) == text) {
                moved.push(self.next_step.remove(i));
            }
        }
        self.requeue(moved)
    }

    fn requeue(&mut self, moved: Vec<Message>) -> Vec<NewEvent> {
        let mut out = Vec::new();
        if moved.is_empty() {
            return out;
        }
        // Rewrite the next-step list without them, then insert them first.
        let total = self.next_step.len() + moved.len();
        let remaining = self.next_step.clone();
        self.splice(&mut out, Target::NextStep, 0, total, &remaining, false);
        self.splice(&mut out, Target::NextTurn, 0, 0, &moved, false);
        self.next_turn.splice(0..0, moved);
        out
    }

    // ---- turn bookkeeping ---------------------------------------------------

    fn open_step(&mut self, out: &mut Vec<NewEvent>) -> Option<(u64, u64)> {
        let (n, step, open) = {
            let t = self.turn.as_mut()?;
            if !t.step_open {
                t.step += 1;
                t.step_open = true;
                (t.n, t.step, false)
            } else {
                (t.n, t.step, true)
            }
        };
        if !open {
            self.emit(
                out,
                NewEvent::new(kinds::STEP_START, json!({ "turn": n, "step": step })),
            );
        }
        Some((n, step))
    }

    /// Closes every unanswered call of the step with an error result.
    fn close_calls(&mut self, out: &mut Vec<NewEvent>, text: &str) {
        let Some(t) = self.turn.as_mut() else { return };
        let (n, step) = (t.n, t.step);
        let calls = std::mem::take(&mut t.calls);
        for call in calls {
            let seq = match call.call_seq {
                Some(seq) => seq,
                None => self.emit(
                    out,
                    NewEvent::new(
                        kinds::TOOL_CALL,
                        json!({"turn": n, "step": step, "callId": call.id, "name": call.name, "arguments": call.raw}),
                    ),
                ),
            };
            let message =
                Message::tool_result(call.id.clone(), vec![ContentBlock::text(text)], true);
            let data = json!({"turn": n, "step": step, "message": message});
            self.emit(
                out,
                Self::surface_append(kinds::TOOL_RESULT, data, Some(vec![seq])),
            );
        }
    }

    fn close_step(&mut self, out: &mut Vec<NewEvent>, unanswered: &str) {
        if !self.turn.as_ref().is_some_and(|t| t.step_open) {
            return;
        }
        self.close_calls(out, unanswered);
        let t = self.turn.as_mut().expect("open turn");
        t.step_open = false;
        t.retrying = false;
        let data = json!({ "turn": t.n, "step": t.step });
        self.emit(out, NewEvent::new(kinds::STEP_END, data));
    }

    /// Writes the active message as committed input if Pi never echoed it.
    fn claim_active(&mut self, out: &mut Vec<NewEvent>) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.claimed {
            return;
        }
        t.claimed = true;
        let Some(id) = t.active.take() else { return };
        if let Some(i) = self.next_turn.iter().position(|m| m.id == id) {
            let message = self.next_turn.remove(i);
            self.splice(out, Target::NextTurn, i, 1, &[], false);
            self.user_message(out, message);
        }
    }

    fn user_message(&mut self, out: &mut Vec<NewEvent>, message: Message) {
        let human = message.source.is_user();
        let text = prompt_text(&message);
        let data = serde_json::to_value(&message).unwrap_or(Value::Null);
        let seq = self.emit(out, Self::surface_append(kinds::USER_MESSAGE, data, None));
        if human && !self.titled {
            let title = fallback_title(&text);
            if !title.is_empty() {
                self.titled = true;
                self.emit(
                    out,
                    NewEvent::new(
                        kinds::SESSION_TITLE,
                        json!({"title": title, "messageSeqs": [seq], "source": {"kind": "fallback"}}),
                    ),
                );
            }
        }
    }

    fn turn_end_event(&mut self, out: &mut Vec<NewEvent>, reason: Value) {
        let Some(t) = self.turn.take() else { return };
        self.emit(
            out,
            NewEvent::new(kinds::TURN_END, json!({ "turn": t.n, "reason": reason })),
        );
    }

    fn close_compaction(&mut self, out: &mut Vec<NewEvent>, code: &str, message: &str) {
        if let Some(c) = self.compaction.take()
            && c.start.is_some()
        {
            self.emit(
                out,
                NewEvent::new(
                    kinds::COMPACTION_END,
                    json!({"compactionId": c.id, "turn": c.turn, "error": {"code": code, "message": message}}),
                ),
            );
        }
    }

    /// Aether closes the open turn itself (the engine exited or a prompt
    /// was refused): unanswered calls fail, the step and the turn end.
    pub fn close_turn(&mut self, reason: TurnEnd) -> Vec<NewEvent> {
        let mut out = Vec::new();
        if self.turn.is_none() {
            return out;
        }
        self.close_compaction(&mut out, "ENGINE_STOPPED", "the Pi engine stopped");
        self.claim_active(&mut out);
        self.close_step(&mut out, "Engine stopped before this call finished.");
        let reason = match reason {
            TurnEnd::Aborted => json!({"kind": "aborted", "reason": {"kind": "user"}}),
            TurnEnd::Interrupted => json!({"kind": "interrupted"}),
            TurnEnd::Error { code, message } => {
                json!({"kind": "error", "error": LlmFailure::new(code, message)})
            }
        };
        self.turn_end_event(&mut out, reason);
        let leftover = std::mem::take(&mut self.next_step);
        out.extend(self.requeue_all(leftover));
        out
    }

    fn requeue_all(&mut self, leftover: Vec<Message>) -> Vec<NewEvent> {
        if leftover.is_empty() {
            return Vec::new();
        }
        self.next_step = leftover;
        let texts: Vec<String> = self.next_step.iter().map(prompt_text).collect();
        self.restore_steers(&texts)
    }

    /// `model/selection` for the model the next prompt uses.
    pub fn model_selection(&mut self, provider: &str, model: &str, effort: &str) -> Vec<NewEvent> {
        let mut out = Vec::new();
        self.emit(
            &mut out,
            NewEvent::new(
                kinds::MODEL_SELECTION,
                json!({"provider": provider, "model": model, "reasoningEffort": effort}),
            ),
        );
        out
    }

    /// `sandbox/mode` after the access mode changed.
    pub fn sandbox_mode(&mut self, mode: &str) -> Vec<NewEvent> {
        let mut out = Vec::new();
        self.emit(
            &mut out,
            NewEvent::new(kinds::SANDBOX_MODE, json!({ "mode": mode })),
        );
        out
    }

    /// `pi/cursor`: the last Pi entry the log mirrors (ignorable for dsh).
    pub fn mark_cursor(&mut self, entry_id: &str) -> Vec<NewEvent> {
        let mut out = Vec::new();
        if self.cursor.as_deref() == Some(entry_id) {
            return out;
        }
        self.cursor = Some(entry_id.to_owned());
        self.emit(
            &mut out,
            NewEvent::new("pi/cursor", json!({ "entryId": entry_id }))
                .with("ignorable", json!(true)),
        );
        out
    }

    // ---- Pi events ------------------------------------------------------------

    /// Maps one Pi stdout record.
    pub fn on_record(&mut self, record: &Value) -> Output {
        let mut out = Output::default();
        let mut events = Vec::new();
        match record["type"].as_str().unwrap_or_default() {
            "turn_start" => {
                if self.turn.is_none() {
                    // Pi started work Aether did not ask for: still a turn.
                    let n = self.last_turn + 1;
                    self.last_turn = n;
                    self.emit(
                        &mut events,
                        NewEvent::new(kinds::TURN_START, json!({ "turn": n })),
                    );
                    self.turn = Some(Turn {
                        n,
                        step: 0,
                        step_open: false,
                        active: None,
                        claimed: true,
                        calls: Vec::new(),
                        failure: None,
                        max_tokens: false,
                        retrying: false,
                        retry: None,
                    });
                }
            }
            "message_start" => {
                let message = &record["message"];
                if message["role"] == "assistant" && self.turn.is_some() {
                    self.open_step(&mut events);
                    let route = self.route(
                        message["provider"].as_str().unwrap_or_default(),
                        message["model"].as_str().unwrap_or_default(),
                    );
                    self.request_context(&mut events, route);
                }
            }
            "message_update" => self.delta(record, &mut out.deltas),
            "message_end" => {
                let message = &record["message"];
                match message["role"].as_str() {
                    Some("user") => self.user_echo(&mut events, message),
                    Some("assistant") => self.assistant(&mut events, message),
                    _ => {}
                }
            }
            "tool_execution_start" => self.tool_start(&mut events, record),
            "tool_execution_end" => self.tool_end(&mut events, record),
            "turn_end" => {
                let retrying = self.turn.as_ref().is_some_and(|t| t.retrying);
                if !retrying {
                    self.close_step(&mut events, "Pi ended the step before this call finished.");
                }
            }
            "auto_retry_start" => self.retry(&mut events, record),
            "auto_retry_end" => {
                if record["success"] == false
                    && let Some(t) = self.turn.as_mut()
                {
                    let message = record["finalError"]
                        .as_str()
                        .unwrap_or("the request failed");
                    t.failure = Some(failure_of(message));
                }
            }
            "compaction_start" => self.compaction_start(&mut events),
            "compaction_end" => self.compaction_end(&mut events, record),
            "agent_settled" => {
                out.settled = Some(self.settle(&mut events, record["aborted"] == true));
            }
            "extension_ui_request" => {
                let dialog = matches!(
                    record["method"].as_str(),
                    Some("select" | "confirm" | "input" | "editor")
                );
                if dialog {
                    out.dialog = record["id"].as_str().map(str::to_owned);
                }
            }
            _ => {}
        }
        out.events = events;
        out
    }

    fn request_context(&mut self, out: &mut Vec<NewEvent>, route: Route) {
        let mut context = Map::new();
        context.insert("provider".into(), route.provider.clone().into());
        context.insert("model".into(), route.model.clone().into());
        if let Some(w) = route.context_window {
            context.insert("contextWindow".into(), w.into());
        }
        let context = Value::Object(context);
        if self.context.as_ref() != Some(&context) {
            self.context = Some(context.clone());
            self.emit(out, NewEvent::new(kinds::REQUEST_CONTEXT, context));
        }
        self.provider = Some(route);
    }

    fn delta(&mut self, record: &Value, deltas: &mut Vec<Delta>) {
        let Some(t) = self.turn.as_ref().filter(|t| t.step_open) else {
            return;
        };
        let e = &record["assistantMessageEvent"];
        let index = e["contentIndex"].as_u64().unwrap_or(0) as usize;
        let (block_type, text) = match e["type"].as_str().unwrap_or_default() {
            "text_delta" => ("text", e["delta"].as_str().unwrap_or_default()),
            "thinking_delta" => ("reasoning", e["delta"].as_str().unwrap_or_default()),
            "toolcall_start" => ("tool-call", e["toolName"].as_str().unwrap_or_default()),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        deltas.push(Delta {
            turn: t.n,
            step: t.step,
            index,
            block_type,
            text: text.to_owned(),
        });
    }

    fn user_echo(&mut self, out: &mut Vec<NewEvent>, message: &Value) {
        let Some(t) = self.turn.as_mut() else { return };
        let text = text_of(&message["content"]);
        if !t.claimed {
            t.claimed = true;
            if let Some(id) = t.active.take()
                && let Some(i) = self.next_turn.iter().position(|m| m.id == id)
            {
                let message = self.next_turn.remove(i);
                self.splice(out, Target::NextTurn, i, 1, &[], false);
                self.user_message(out, message);
                return;
            }
        }
        // A steer: FIFO by text, else the oldest one.
        let index = self
            .next_step
            .iter()
            .position(|m| prompt_text(m) == text)
            .or_else(|| (!self.next_step.is_empty()).then_some(0));
        let message = match index {
            Some(i) => {
                let m = self.next_step.remove(i);
                self.splice(out, Target::NextStep, i, 1, &[], false);
                m
            }
            None => Message {
                id: dshx_core::new_id(),
                role: Role::User,
                content: vec![ContentBlock::text(text)],
                source: MessageSource::User,
                tool_call_id: None,
                is_error: None,
                extra: Default::default(),
            },
        };
        self.user_message(out, message);
    }

    fn assistant(&mut self, out: &mut Vec<NewEvent>, message: &Value) {
        if self.turn.is_none() {
            return;
        }
        let Some((n, step)) = self.open_step(out) else {
            return;
        };
        let stop = message["stopReason"].as_str().unwrap_or("stop");
        let failed = matches!(stop, "error" | "aborted");
        let mut content = Vec::new();
        let mut calls = Vec::new();
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        content.push(ContentBlock::text(text));
                    }
                }
                Some("thinking") => {
                    let text = block["thinking"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        content.push(ContentBlock::Reasoning {
                            text: text.to_owned(),
                        });
                    }
                }
                Some("toolCall") if !failed => {
                    let id = block["id"].as_str().unwrap_or_default().to_owned();
                    let name = block["name"].as_str().unwrap_or_default().to_owned();
                    if id.is_empty() || name.is_empty() {
                        continue;
                    }
                    let raw =
                        serde_json::to_string(&block["arguments"]).unwrap_or_else(|_| "{}".into());
                    content.push(ContentBlock::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: raw.clone(),
                    });
                    calls.push(Call {
                        id,
                        name,
                        raw,
                        call_seq: None,
                    });
                }
                _ => {}
            }
        }
        let route = self.route(
            message["provider"].as_str().unwrap_or_default(),
            message["model"].as_str().unwrap_or_default(),
        );
        let usage = usage_json(&message["usage"]);
        if content.is_empty() && failed {
            self.emit(
                out,
                NewEvent::new(
                    dshx_session_attempt(),
                    json!({"turn": n, "step": step, "stream": []}),
                ),
            );
        } else {
            let msg = Message {
                id: dshx_core::new_id(),
                role: Role::Assistant,
                content,
                source: MessageSource::Model {
                    provider: route.provider.clone(),
                    model: route.model.clone(),
                    replay_state: None,
                },
                tool_call_id: None,
                is_error: None,
                extra: Default::default(),
            };
            let mut data = Map::new();
            data.insert("turn".into(), n.into());
            data.insert("step".into(), step.into());
            data.insert(
                "message".into(),
                serde_json::to_value(&msg).unwrap_or_default(),
            );
            if failed {
                data.insert("interrupted".into(), true.into());
            }
            if let Some(u) = usage {
                data.insert("usage".into(), u);
            }
            data.insert("stream".into(), json!([]));
            self.emit(
                out,
                Self::surface_append(kinds::ASSISTANT_MESSAGE, Value::Object(data), None),
            );
        }
        let t = self.turn.as_mut().expect("open turn");
        t.calls.extend(calls);
        t.retrying = false;
        match stop {
            "error" => {
                let message = message["errorMessage"]
                    .as_str()
                    .unwrap_or("the model request failed");
                t.failure = Some(failure_of(message));
                t.retrying = t.calls.is_empty();
            }
            "length" => t.max_tokens = true,
            _ => {
                t.failure = None;
                t.max_tokens = false;
            }
        }
    }

    fn tool_start(&mut self, out: &mut Vec<NewEvent>, record: &Value) {
        let id = record["toolCallId"].as_str().unwrap_or_default();
        let Some(t) = self.turn.as_ref() else { return };
        let (n, step) = (t.n, t.step);
        let Some(index) = t
            .calls
            .iter()
            .position(|c| c.id == id && c.call_seq.is_none())
        else {
            return;
        };
        let (name, raw) = {
            let c = &t.calls[index];
            (c.name.clone(), c.raw.clone())
        };
        let seq = self.emit(
            out,
            NewEvent::new(
                kinds::TOOL_CALL,
                json!({"turn": n, "step": step, "callId": id, "name": name, "arguments": raw}),
            ),
        );
        if let Some(t) = self.turn.as_mut() {
            t.calls[index].call_seq = Some(seq);
        }
    }

    fn tool_end(&mut self, out: &mut Vec<NewEvent>, record: &Value) {
        let id = record["toolCallId"].as_str().unwrap_or_default().to_owned();
        let Some(t) = self.turn.as_ref() else { return };
        let (n, step) = (t.n, t.step);
        let Some(index) = t.calls.iter().position(|c| c.id == id) else {
            return;
        };
        if t.calls[index].call_seq.is_none() {
            // Pi finished a call it never announced as started.
            self.tool_start(out, &json!({ "toolCallId": id }));
        }
        let Some(t) = self.turn.as_mut() else { return };
        let call = t.calls.remove(index);
        let call_seq = call.call_seq.unwrap_or(0);
        let result = &record["result"];
        let is_error = record["isError"] == true;
        let args: Value = serde_json::from_str(&call.raw).unwrap_or(Value::Null);
        if call.name == "todo_write" && !is_error {
            let todos = result
                .pointer("/details/aether/value/todos")
                .or_else(|| args.get("todos"))
                .cloned();
            if let Some(todos) = todos {
                self.emit(
                    out,
                    NewEvent::new(kinds::TODO_WRITE, json!({ "todos": todos })),
                );
            }
        }
        let mut content = Vec::new();
        for block in result["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => content.push(ContentBlock::text(
                    block["text"].as_str().unwrap_or_default(),
                )),
                // The engine replaced Pi's base64 image with a stored attachment.
                Some("aether-image") => {
                    if let Ok(attachment) = serde_json::from_value(block["attachment"].clone()) {
                        content.push(ContentBlock::Image {
                            attachment,
                            offloaded: false,
                        });
                    }
                }
                Some("image") => content.push(ContentBlock::text("[image]")),
                _ => {}
            }
        }
        let message = Message::tool_result(id, content, is_error);
        let mut data = Map::new();
        data.insert("turn".into(), n.into());
        data.insert("step".into(), step.into());
        data.insert(
            "message".into(),
            serde_json::to_value(&message).unwrap_or_default(),
        );
        if let Some(meta) = crate::meta::tool_meta(&call.name, &args, result, &self.cwd) {
            data.insert("meta".into(), meta);
        }
        self.emit(
            out,
            Self::surface_append(
                kinds::TOOL_RESULT,
                Value::Object(data),
                Some(vec![call_seq]),
            ),
        );
    }

    fn retry(&mut self, out: &mut Vec<NewEvent>, record: &Value) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.step == 0 {
            return;
        }
        let id = t.retry.get_or_insert_with(dshx_core::new_id).clone();
        let (n, step) = (t.n, t.step);
        let message = record["errorMessage"]
            .as_str()
            .unwrap_or("the request failed");
        let mut data = json!({
            "retryId": id,
            "turn": n,
            "step": step,
            "retry": record["attempt"].as_u64().unwrap_or(1),
            "delayMs": record["delayMs"].as_u64().unwrap_or(0),
            "failure": failure_of(message),
        });
        if let Some(max) = record["maxAttempts"].as_u64() {
            data["maxRetries"] = max.into();
        }
        self.emit(out, NewEvent::new(kinds::LLM_RETRY, data));
    }

    fn compaction_start(&mut self, out: &mut Vec<NewEvent>) {
        if self.compaction.is_some() {
            return;
        }
        let quiet = std::mem::take(&mut self.quiet_next);
        let mut c = Compacting {
            id: dshx_core::new_id(),
            turn: self.turn.as_ref().map(|t| t.n),
            start: None,
        };
        if !quiet {
            c.start = Some(self.emit(
                out,
                NewEvent::new(
                    kinds::COMPACTION_START,
                    json!({"compactionId": c.id, "turn": c.turn}),
                ),
            ));
        }
        self.compaction = Some(c);
    }

    fn compaction_end(&mut self, out: &mut Vec<NewEvent>, record: &Value) {
        if self.compaction.is_none() {
            self.compaction_start(out);
        }
        let Some(c) = self.compaction.take() else {
            return;
        };
        let summary = record
            .pointer("/result/summary")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let Some(summary) = summary else {
            if c.start.is_some() {
                let (code, message) = if record["aborted"] == true {
                    ("ABORTED", "compaction was aborted".to_owned())
                } else {
                    (
                        "COMPACTION_FAILED",
                        record["errorMessage"]
                            .as_str()
                            .unwrap_or("compaction failed")
                            .to_owned(),
                    )
                };
                self.emit(
                    out,
                    NewEvent::new(
                        kinds::COMPACTION_END,
                        json!({"compactionId": c.id, "turn": c.turn, "error": {"code": code, "message": message}}),
                    ),
                );
            }
            return;
        };
        // The shadowed span: everything before the latest prompt (Pi keeps
        // the recent part itself; its own file decides what the model sees).
        let nodes: Vec<Node> = self.surface.clone();
        let cut = nodes
            .iter()
            .rposition(|n| n.human)
            .filter(|i| *i > 0)
            .unwrap_or(nodes.len().saturating_sub(1));
        let span: Vec<u64> = nodes[..cut].iter().map(|n| n.seq).collect();
        let start_seq = match c.start {
            Some(seq) => seq,
            None => self.emit(
                out,
                NewEvent::new(
                    kinds::COMPACTION_START,
                    json!({"compactionId": c.id, "turn": c.turn}),
                ),
            ),
        };
        if span.is_empty() {
            self.emit(
                out,
                NewEvent::new(
                    kinds::COMPACTION_END,
                    json!({"compactionId": c.id, "turn": c.turn, "error": {"code": "NOTHING_TO_MIRROR", "message": "Pi compacted, but the log has nothing older to fold"}}),
                ),
            );
            return;
        }
        let route = self.provider.clone();
        let mut data = Map::new();
        data.insert("compactionId".into(), c.id.clone().into());
        data.insert(
            "summary".into(),
            json!([{ "type": "text", "text": summary }]),
        );
        data.insert(
            "shadowedRange".into(),
            json!({"start": span[0], "end": span[span.len() - 1]}),
        );
        data.insert("shadowedSeqs".into(), json!(span));
        if let Some(t) = record
            .pointer("/result/tokensBefore")
            .and_then(Value::as_u64)
        {
            data.insert("shadowedTokenCount".into(), t.into());
        }
        if let Some(r) = &route {
            data.insert("provider".into(), r.provider.clone().into());
            data.insert("model".into(), r.model.clone().into());
        }
        if let Some(u) = record.pointer("/result/usage").and_then(usage_json) {
            data.insert("usage".into(), u);
        }
        let summary_seq = self.emit(
            out,
            NewEvent::new(kinds::COMPACTION_SUMMARY, Value::Object(data)),
        );
        let checkpoint = Message {
            id: dshx_core::new_id(),
            role: Role::User,
            content: vec![ContentBlock::text(format!(
                "Summary of the earlier conversation (Pi compaction):\n\n{summary}"
            ))],
            source: MessageSource::Other(
                json!({"kind": "compact-checkpoint", "compactionId": c.id}),
            ),
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        };
        let mut sources = vec![start_seq, summary_seq];
        sources.extend(&span);
        let event = NewEvent::new(
            kinds::USER_MESSAGE,
            serde_json::to_value(&checkpoint).unwrap_or_default(),
        )
        .with("sourceEventSeqs", json!(sources))
        .with(
            "surfaceOp",
            json!({"op": "replace", "startSeq": span[0], "endSeq": span[span.len() - 1]}),
        );
        let seq = self.emit(out, event);
        self.surface.splice(..cut, [Node { seq, human: false }]);
        self.emit(
            out,
            NewEvent::new(
                kinds::COMPACTION_END,
                json!({"compactionId": c.id, "turn": c.turn}),
            ),
        );
    }

    fn settle(&mut self, out: &mut Vec<NewEvent>, aborted: bool) -> Settled {
        let Some(t) = self.turn.as_ref() else {
            return if aborted {
                Settled::Aborted
            } else {
                Settled::Completed
            };
        };
        let failure = t.failure.clone();
        let max_tokens = t.max_tokens;
        self.close_compaction(out, "ABORTED", "the run ended during compaction");
        self.claim_active(out);
        self.close_step(out, "Stopped before this call finished.");
        let (settled, reason) = if aborted {
            (
                Settled::Aborted,
                json!({"kind": "aborted", "reason": {"kind": "user"}}),
            )
        } else if let Some(f) = failure {
            (
                Settled::Error(f.message.clone()),
                json!({"kind": "error", "error": f}),
            )
        } else if max_tokens {
            (Settled::MaxTokens, json!({"kind": "max-tokens"}))
        } else {
            (Settled::Completed, json!({"kind": "completed"}))
        };
        self.turn_end_event(out, reason);
        let leftover = std::mem::take(&mut self.next_step);
        out.extend(self.requeue_all(leftover));
        settled
    }
}

fn dshx_session_attempt() -> &'static str {
    "assistant/attempt"
}
