//! The model-visible surface over the event log and the message history it
//! derives to — a port of dsh `packages/core/session/src/surface.ts`.
//!
//! Message-producing events (`system/message`, `developer/message`,
//! `user/message`, `assistant/message`, `tool/result`) carry a `surfaceOp`:
//! `"append"` adds a node at the tail, `{op:"replace",startSeq,endSeq}`
//! replaces a node range (compaction summaries, pruned tool results) and must
//! cite every shadowed node in `sourceEventSeqs`. `image/offload` rewrites
//! existing messages without moving nodes.
use std::collections::{HashMap, HashSet};

use dshx_core::{ContentBlock, Message, SessionEvent};
use serde_json::Value;

use crate::error::SessionError;
use crate::format::{is_known_event_type, is_surface_event_type};

/// How a surface event entered the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceOp {
    Append,
    Replace { start: u64, end: u64 },
}

impl SurfaceOp {
    /// The JSON envelope value (`"append"` or the replace object).
    pub fn to_value(self) -> Value {
        match self {
            SurfaceOp::Append => Value::from("append"),
            SurfaceOp::Replace { start, end } => {
                serde_json::json!({"op": "replace", "startSeq": start, "endSeq": end})
            }
        }
    }
}

/// One replacement observed while folding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceReplacement {
    pub seq: u64,
    pub start: u64,
    pub end: u64,
    pub shadowed: Vec<u64>,
}

fn is_ignorable(event: &SessionEvent) -> bool {
    event.extra.get("ignorable") == Some(&Value::Bool(true))
}

fn count(value: &Value) -> Option<u64> {
    value.as_u64().filter(|v| *v < (1u64 << 53))
}

fn parse_surface_op(value: &Value) -> Option<SurfaceOp> {
    if value == "append" {
        return Some(SurfaceOp::Append);
    }
    let obj = value.as_object()?;
    if obj.len() != 3 || obj.get("op")? != "replace" {
        return None;
    }
    Some(SurfaceOp::Replace {
        start: count(obj.get("startSeq")?)?,
        end: count(obj.get("endSeq")?)?,
    })
}

/// Event-local surface metadata check (dsh `validateSurfaceMetadata`).
/// Returns the operation, or `None` for log-only and unknown ignorable events.
pub fn surface_op_of(event: &SessionEvent) -> Result<Option<SurfaceOp>, String> {
    let raw_op = event.extra.get("surfaceOp");
    let raw_sources = event.extra.get("sourceEventSeqs");
    if !is_surface_event_type(&event.kind) {
        if !is_known_event_type(&event.kind) && is_ignorable(event) {
            return Ok(None);
        }
        if raw_op.is_some() {
            return Err(format!(
                "session event \"{}\" is not surface-eligible and cannot carry surfaceOp",
                event.kind
            ));
        }
        if raw_sources.is_some() {
            return Err(format!(
                "session event \"{}\" is not surface-eligible and cannot carry sourceEventSeqs",
                event.kind
            ));
        }
        return Ok(None);
    }
    let Some(raw_op) = raw_op else {
        return Err(format!(
            "session event \"{}\" is surface-eligible and requires a surfaceOp marker",
            event.kind
        ));
    };
    let op = parse_surface_op(raw_op).ok_or_else(|| {
        format!(
            "session event \"{}\" carries an invalid surfaceOp",
            event.kind
        )
    })?;
    if let SurfaceOp::Replace { start, end } = op
        && (start >= event.seq || end >= event.seq)
    {
        return Err(format!(
            "surface replace at seq {}: startSeq and endSeq must reference earlier events",
            event.seq
        ));
    }
    source_references(event, &[])?;
    Ok(Some(op))
}

/// `sourceEventSeqs` must be unique earlier seqs that cite every shadowed node.
fn source_references(event: &SessionEvent, shadowed: &[u64]) -> Result<(), String> {
    let raw = event.extra.get("sourceEventSeqs");
    if event.kind == "assistant/message" && raw.is_some() {
        return Err(
            "assistant/message embeds its source stream and cannot carry sourceEventSeqs".into(),
        );
    }
    let mut sources = HashSet::new();
    if let Some(raw) = raw {
        let list = raw.as_array().ok_or_else(|| {
            format!(
                "sourceEventSeqs on event at seq {} must be an array",
                event.seq
            )
        })?;
        if list.is_empty() {
            return Err("sourceEventSeqs must not be empty".into());
        }
        for source in list {
            let seq = count(source).ok_or_else(|| {
                format!("session event \"{}\" sourceEventSeqs must densely contain non-negative safe integers", event.kind)
            })?;
            if !sources.insert(seq) {
                return Err("sourceEventSeqs must not contain duplicates".into());
            }
            if seq >= event.seq {
                return Err(format!(
                    "sourceEventSeqs must reference earlier events: {seq} >= current seq {}",
                    event.seq
                ));
            }
        }
    }
    let missing: Vec<String> = shadowed
        .iter()
        .filter(|s| !sources.contains(s))
        .map(u64::to_string)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "surface replace: sourceEventSeqs must include every shadowed surface node; missing {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

/// Indexed view of the log being folded; `base` is the seq of `events[0]`.
#[derive(Clone, Copy)]
struct Window<'a> {
    events: &'a [SessionEvent],
    base: u64,
}

impl<'a> Window<'a> {
    fn get(&self, seq: u64) -> Option<&'a SessionEvent> {
        let event = self
            .events
            .get(usize::try_from(seq.checked_sub(self.base)?).ok()?)?;
        (event.seq == seq).then_some(event)
    }
}

/// A validated transition that has not been applied yet.
enum Plan {
    Append(u64),
    Replace {
        seq: u64,
        start: u64,
        end: u64,
        start_idx: usize,
        end_idx: usize,
    },
    Project(Vec<(u64, Message)>),
}

/// Incremental surface state (dsh `SurfaceFoldState` + `SurfaceManager`).
#[derive(Debug, Clone, Default)]
pub struct SurfaceFold {
    nodes: Vec<u64>,
    projected: HashMap<u64, Message>,
    replacements: Vec<SurfaceReplacement>,
    replace_generation: u64,
    content_generation: u64,
}

impl SurfaceFold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current surface event seqs in model-visible order.
    pub fn nodes(&self) -> &[u64] {
        &self.nodes
    }

    /// Replacement operations in event order.
    pub fn replacements(&self) -> &[SurfaceReplacement] {
        &self.replacements
    }

    /// Count of committed replacements and message projections; a changed
    /// value means previously derived history is stale (prefix cache break).
    pub fn content_generation(&self) -> u64 {
        self.content_generation
    }

    pub fn replace_generation(&self) -> u64 {
        self.replace_generation
    }

    /// Messages rewritten by projections (`image/offload`), keyed by original seq.
    pub fn projected(&self) -> &HashMap<u64, Message> {
        &self.projected
    }

    /// Validates `event` (the next event after `events`, which holds the log
    /// so far from seq `events[0].seq`) and applies it.
    pub fn push(&mut self, event: &SessionEvent, events: &[SessionEvent]) -> Result<(), String> {
        let base = events.first().map(|e| e.seq).unwrap_or(event.seq);
        let expected = base + events.len() as u64;
        if event.seq != expected {
            return Err(format!(
                "session event seq {} is not contiguous; expected {expected}",
                event.seq
            ));
        }
        let plan = self.plan(event, Window { events, base })?;
        self.apply(plan);
        Ok(())
    }

    /// Like [`push`](Self::push) but never fails: a surface event without
    /// `surfaceOp` counts as an append, other invalid transitions are skipped.
    pub fn push_lenient(&mut self, event: &SessionEvent, events: &[SessionEvent]) {
        let base = events.first().map(|e| e.seq).unwrap_or(event.seq);
        self.push_lenient_in(event, Window { events, base });
    }

    fn push_lenient_in(&mut self, event: &SessionEvent, window: Window<'_>) {
        let plan = match self.plan(event, window) {
            Ok(plan) => plan,
            Err(error) => {
                if is_surface_event_type(&event.kind) && !event.extra.contains_key("surfaceOp") {
                    Some(Plan::Append(event.seq))
                } else {
                    tracing::warn!(seq = event.seq, kind = %event.kind, %error, "skipping invalid surface transition");
                    None
                }
            }
        };
        self.apply(plan);
    }

    fn plan(&self, event: &SessionEvent, window: Window<'_>) -> Result<Option<Plan>, String> {
        let op = surface_op_of(event)?;
        developer_header(event, window)?;
        if event.kind == "image/offload" {
            return image_offload(event, &self.nodes, &self.projected, window)
                .map(|m| Some(Plan::Project(m)));
        }
        let Some(op) = op else { return Ok(None) };
        let SurfaceOp::Replace { start, end } = op else {
            return Ok(Some(Plan::Append(event.seq)));
        };
        let start_idx =
            self.nodes.iter().position(|n| *n == start).ok_or_else(|| {
                format!("surface replace: start seq {start} not found in surface")
            })?;
        let end_idx = self
            .nodes
            .iter()
            .position(|n| *n == end)
            .ok_or_else(|| format!("surface replace: end seq {end} not found in surface"))?;
        if start_idx > end_idx {
            return Err(format!(
                "surface replace: start seq {start} (index {start_idx}) is after end seq {end} (index {end_idx})"
            ));
        }
        let shadowed = &self.nodes[start_idx..=end_idx];
        source_references(event, shadowed)?;
        tool_result_rewrite(event, shadowed, window)?;
        if start_idx == 0
            && window
                .get(self.nodes[0])
                .is_some_and(|head| head.kind == "system/message")
            && (event.kind != "system/message" || shadowed.len() != 1)
        {
            return Err("surface replace: node 0 holds the system prompt and may be rewritten only by a system/message over exactly that node".into());
        }
        Ok(Some(Plan::Replace {
            seq: event.seq,
            start,
            end,
            start_idx,
            end_idx,
        }))
    }

    fn apply(&mut self, plan: Option<Plan>) {
        match plan {
            None => {}
            Some(Plan::Append(seq)) => self.nodes.push(seq),
            Some(Plan::Replace {
                seq,
                start,
                end,
                start_idx,
                end_idx,
            }) => {
                let shadowed: Vec<u64> = self.nodes.splice(start_idx..=end_idx, [seq]).collect();
                self.replacements.push(SurfaceReplacement {
                    seq,
                    start,
                    end,
                    shadowed,
                });
                self.replace_generation += 1;
                self.content_generation += 1;
            }
            Some(Plan::Project(messages)) => {
                self.projected.extend(messages);
                self.content_generation += 1;
            }
        }
    }

    /// The message one event derives to with this fold's projections applied.
    pub fn derive_event_message(&self, event: &SessionEvent) -> Option<Message> {
        if let Some(message) = self.projected.get(&event.seq) {
            return Some(message.clone());
        }
        derive_event_message(event)
    }

    /// Model-visible history of `events` (the same log this fold consumed).
    pub fn messages(&self, events: &[SessionEvent]) -> Vec<Message> {
        let window = Window {
            events,
            base: events.first().map(|e| e.seq).unwrap_or(0),
        };
        self.nodes
            .iter()
            .filter_map(|seq| window.get(*seq))
            .filter_map(|e| self.derive_event_message(e))
            .collect()
    }
}

/// The message an event produces on its own, or `None` for log-only events
/// and empty system/developer/assistant messages (dsh `deriveEventMessage`).
pub fn derive_event_message(event: &SessionEvent) -> Option<Message> {
    let value = message_value(event)?;
    if event.kind != "user/message"
        && event.kind != "tool/result"
        && value
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return None;
    }
    match serde_json::from_value::<Message>(value.clone()) {
        Ok(message) => Some(message),
        Err(error) => {
            tracing::warn!(seq = event.seq, kind = %event.kind, %error, "session event message does not decode");
            None
        }
    }
}

/// The raw message JSON of a surface event.
pub fn message_value(event: &SessionEvent) -> Option<&Value> {
    match event.kind.as_str() {
        "user/message" => Some(&event.data),
        "system/message" | "developer/message" | "assistant/message" | "tool/result" => {
            event.data.get("message")
        }
        _ => None,
    }
}

/// `developer/message` tool additions must resolve against an earlier `request/header`.
fn developer_header(event: &SessionEvent, window: Window<'_>) -> Result<(), String> {
    if event.kind != "developer/message" {
        return Ok(());
    }
    let Some(header_seq) = event.data.get("headerSeq") else {
        return Ok(());
    };
    let header_seq = count(header_seq).ok_or("developer/message headerSeq must be a seq")?;
    let header = window
        .get(header_seq)
        .filter(|h| header_seq < event.seq && h.kind == "request/header");
    let Some(header) = header else {
        return Err("developer/message headerSeq must reference an earlier request/header".into());
    };
    let tools = header
        .data
        .pointer("/header/tools")
        .and_then(Value::as_array);
    let blocks = event
        .data
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for block in blocks.filter(|b| b.get("type") == Some(&Value::from("tool-addition"))) {
        let name = block.get("toolName");
        let defs: Vec<&Value> = tools
            .into_iter()
            .flatten()
            .filter(|t| t.get("name") == name)
            .collect();
        if defs.len() != 1 {
            return Err(format!(
                "developer/message tool-addition {name:?} must name exactly one tool in headerSeq {header_seq}"
            ));
        }
    }
    Ok(())
}

/// A `tool/result` replacement may change only the result content of one node.
fn tool_result_rewrite(
    event: &SessionEvent,
    shadowed: &[u64],
    window: Window<'_>,
) -> Result<(), String> {
    if event.kind != "tool/result" {
        return Ok(());
    }
    if shadowed.len() != 1 {
        return Err("tool/result surface replacement must rewrite exactly one current node".into());
    }
    let original = window
        .get(shadowed[0])
        .filter(|o| o.kind == "tool/result")
        .ok_or("tool/result surface replacement must target a current tool/result")?;
    let without_content = |data: &Value| {
        let mut data = data.clone();
        if let Some(message) = data.get_mut("message").and_then(Value::as_object_mut) {
            message.insert("content".into(), Value::Null);
        }
        data
    };
    if without_content(&original.data) != without_content(&event.data) {
        return Err("tool/result surface replacement may change only content".into());
    }
    Ok(())
}

/// The `image/offload` message projection (`compaction-image-offload`).
fn image_offload(
    event: &SessionEvent,
    nodes: &[u64],
    projected: &HashMap<u64, Message>,
    window: Window<'_>,
) -> Result<Vec<(u64, Message)>, String> {
    let targets = event
        .data
        .as_object()
        .filter(|d| d.len() == 1)
        .and_then(|d| d.get("targets"))
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
        .ok_or("image/offload: data must contain a nonempty targets array")?;
    let mut out: Vec<(u64, Message)> = Vec::new();
    for target in targets {
        let bad = || {
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_string()
        };
        let obj = target
            .as_object()
            .filter(|t| t.len() == 2)
            .ok_or_else(bad)?;
        let seq = obj.get("seq").and_then(count).ok_or_else(bad)?;
        let indexes = obj
            .get("imageIndexes")
            .and_then(Value::as_array)
            .filter(|i| !i.is_empty())
            .ok_or_else(bad)?;
        if out.iter().any(|(s, _)| *s == seq) {
            return Err(format!("image/offload: duplicate target seq {seq}"));
        }
        if !nodes.contains(&seq) {
            return Err(format!(
                "image/offload: target seq {seq} is not a current surface node"
            ));
        }
        let source = window
            .get(seq)
            .filter(|s| s.kind == "user/message" || s.kind == "tool/result")
            .ok_or_else(|| {
                format!("image/offload: target seq {seq} must be user/message or tool/result")
            })?;
        let mut wanted = Vec::with_capacity(indexes.len());
        for index in indexes {
            let index = count(index)
                .filter(|i| wanted.last().is_none_or(|prev| i > prev))
                .ok_or("image/offload: imageIndexes must be strictly increasing non-negative safe integers")?;
            wanted.push(index);
        }
        let mut message = match projected.get(&seq) {
            Some(m) => m.clone(),
            None => serde_json::from_value(message_value(source).cloned().unwrap_or(Value::Null))
                .map_err(|e| {
                format!("image/offload: target seq {seq} message does not decode: {e}")
            })?,
        };
        let mut image_index = 0u64;
        let mut selected = 0usize;
        for block in &mut message.content {
            if let ContentBlock::Image { offloaded, .. } = block {
                if wanted.get(selected) == Some(&image_index) {
                    if *offloaded {
                        return Err(format!(
                            "image/offload: image index {image_index} is already offloaded"
                        ));
                    }
                    *offloaded = true;
                    selected += 1;
                }
                image_index += 1;
            }
        }
        if selected != wanted.len() {
            return Err(format!(
                "image/offload: image index {} does not exist",
                wanted[selected]
            ));
        }
        out.push((seq, message));
    }
    Ok(out)
}

/// Strict fold of a complete log from seq 0; fails like dsh on any surface violation.
pub fn fold_surface(events: &[SessionEvent]) -> Result<SurfaceFold, SessionError> {
    let mut fold = SurfaceFold::new();
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64 {
            return Err(SessionError::InvalidEvent(format!(
                "session event seq {} is not contiguous; expected {index}",
                event.seq
            )));
        }
        fold.push(event, &events[..index]).map_err(|e| {
            SessionError::InvalidEvent(format!("invalid event at seq {}: {e}", event.seq))
        })?;
    }
    Ok(fold)
}

/// Folds session events into the model-visible message history, like dsh
/// `Session.deriveMessages()`: surface nodes in order (appends, compaction
/// and pruning replacements applied, offloaded images marked), empty
/// system/developer/assistant messages skipped.
///
/// Lenient where dsh would refuse the log: a surface event without
/// `surfaceOp` counts as an append and any other invalid transition is
/// skipped (with a warning), so a slightly malformed log still yields
/// history. Use [`fold_surface`] to validate.
pub fn derive_messages(events: &[SessionEvent]) -> Vec<Message> {
    fold_lenient(events).messages(events)
}

/// Never-failing fold behind [`derive_messages`].
pub fn fold_lenient(events: &[SessionEvent]) -> SurfaceFold {
    let window = Window {
        events,
        base: events.first().map(|e| e.seq).unwrap_or(0),
    };
    let mut fold = SurfaceFold::new();
    for event in events {
        fold.push_lenient_in(event, window);
    }
    fold
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, kind: &str, data: Value, extra: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 1,
            data,
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    fn user(seq: u64, id: &str, text: &str, extra: Value) -> SessionEvent {
        ev(
            seq,
            "user/message",
            json!({"role":"user","content":[{"type":"text","text":text}],"source":{"kind":"user"},"id":id}),
            extra,
        )
    }

    fn system(seq: u64) -> SessionEvent {
        ev(
            seq,
            "system/message",
            json!({"turn":1,"step":1,"message":{"role":"system","content":[{"type":"text","text":"sys"}],"source":{"kind":"system-prompt"},"id":"s"}}),
            json!({"surfaceOp":"append"}),
        )
    }

    fn assistant(seq: u64, content: Value) -> SessionEvent {
        ev(
            seq,
            "assistant/message",
            json!({"turn":1,"step":1,"message":{"role":"assistant","content":content,"source":{"kind":"model","provider":"p","model":"m"},"id":format!("a{seq}")},"stream":[]}),
            json!({"surfaceOp":"append"}),
        )
    }

    fn tool_result(seq: u64, call: &str, text: &str, extra: Value) -> SessionEvent {
        ev(
            seq,
            "tool/result",
            json!({"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":call},"toolCallId":call,"content":[{"type":"text","text":text}],"isError":false,"id":format!("r{seq}")}}),
            extra,
        )
    }

    fn texts(messages: &[Message]) -> Vec<String> {
        messages.iter().map(|m| m.plain_text()).collect()
    }

    #[test]
    fn appends_and_skips_log_only_and_empty() {
        let events = vec![
            ev(0, "turn/start", json!({"turn":1}), json!({})),
            system(1),
            user(2, "u1", "hi", json!({"surfaceOp":"append"})),
            assistant(3, json!([])),
            assistant(4, json!([{"type":"text","text":"yo"}])),
        ];
        assert_eq!(texts(&derive_messages(&events)), ["sys", "hi", "yo"]);
        assert_eq!(fold_surface(&events).unwrap().nodes(), &[1, 2, 3, 4]);
    }

    #[test]
    fn compaction_replace_shadows_range() {
        let events = vec![
            system(0),
            user(1, "u1", "one", json!({"surfaceOp":"append"})),
            assistant(2, json!([{"type":"text","text":"two"}])),
            user(3, "u2", "three", json!({"surfaceOp":"append"})),
            ev(4, "compaction/summary", json!({"summary":[]}), json!({})),
            user(
                5,
                "c",
                "summary",
                json!({"surfaceOp":{"op":"replace","startSeq":1,"endSeq":2},"sourceEventSeqs":[1,2,4]}),
            ),
        ];
        assert_eq!(
            texts(&derive_messages(&events)),
            ["sys", "summary", "three"]
        );
        let fold = fold_surface(&events).unwrap();
        assert_eq!(fold.nodes(), &[0, 5, 3]);
        assert_eq!(fold.replacements()[0].shadowed, vec![1, 2]);
        assert_eq!(fold.content_generation(), 1);
    }

    #[test]
    fn replace_rules() {
        let base = vec![
            system(0),
            user(1, "u1", "one", json!({"surfaceOp":"append"})),
        ];
        let check = |extra: Value| {
            let mut events = base.clone();
            events.push(user(2, "x", "x", extra));
            fold_surface(&events).map(|_| ())
        };
        // must cite every shadowed node
        assert!(check(json!({"surfaceOp":{"op":"replace","startSeq":1,"endSeq":1}})).is_err());
        // system head is protected
        assert!(check(json!({"surfaceOp":{"op":"replace","startSeq":0,"endSeq":1},"sourceEventSeqs":[0,1]})).is_err());
        // missing marker
        assert!(check(json!({})).is_err());
        // log-only events cannot carry markers
        assert!(
            fold_surface(&[ev(
                0,
                "turn/start",
                json!({}),
                json!({"surfaceOp":"append"})
            )])
            .is_err()
        );
        // unknown ignorable events are opaque
        assert!(
            fold_surface(&[ev(
                0,
                "x/y",
                json!({}),
                json!({"ignorable":true,"surfaceOp":"append"})
            )])
            .is_ok()
        );
        assert!(
            check(
                json!({"surfaceOp":{"op":"replace","startSeq":1,"endSeq":1},"sourceEventSeqs":[1]})
            )
            .is_ok()
        );
    }

    #[test]
    fn tool_result_prune_may_only_change_content() {
        let mut events = vec![
            system(0),
            assistant(
                1,
                json!([{"type":"tool-call","id":"c1","name":"bash","arguments":"{}"}]),
            ),
            tool_result(2, "c1", "long output", json!({"surfaceOp":"append"})),
        ];
        let mut pruned = tool_result(
            3,
            "c1",
            "[pruned]",
            json!({"surfaceOp":{"op":"replace","startSeq":2,"endSeq":2},"sourceEventSeqs":[2]}),
        );
        pruned.data["message"]["id"] = json!("r2");
        events.push(pruned.clone());
        assert_eq!(texts(&derive_messages(&events)), ["sys", "", "[pruned]"]);
        pruned.data["message"]["isError"] = json!(true);
        events[3] = pruned;
        assert!(fold_surface(&events).is_err());
    }

    #[test]
    fn image_offload_marks_images() {
        let image = json!({"type":"image","attachment":{"attachmentId":"sha256:00","mediaType":"image/png","bytes":1,"width":1,"height":1}});
        let mut u = user(0, "u", "look", json!({"surfaceOp":"append"}));
        u.data["content"] = json!([image, {"type":"text","text":"look"}, image]);
        let events = vec![
            u,
            ev(
                1,
                "image/offload",
                json!({"targets":[{"seq":0,"imageIndexes":[1]}]}),
                json!({}),
            ),
        ];
        let messages = derive_messages(&events);
        let offloaded: Vec<bool> = messages[0]
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Image { offloaded, .. } => Some(*offloaded),
                _ => None,
            })
            .collect();
        assert_eq!(offloaded, [false, true]);
        assert_eq!(fold_surface(&events).unwrap().content_generation(), 1);
    }

    #[test]
    fn lenient_derive_treats_missing_marker_as_append() {
        let events = vec![
            user(0, "u", "hi", json!({})),
            assistant(1, json!([{"type":"text","text":"yo"}])),
        ];
        assert_eq!(texts(&derive_messages(&events)), ["hi", "yo"]);
    }
}
