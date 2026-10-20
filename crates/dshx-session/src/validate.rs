//! "Would the official dsh load this log?" — the semantic admission checks
//! dsh applies when it opens a current v4 session, ported from
//! `session-format-v3-to-v4/src/{codec,validation,relationships,developer,
//! message-sources,retired-syntax,system-message,tool-role,fork-result}.ts`
//! and the restore checks of `packages/core/session/src/index.ts`.
//!
//! The [`crate::reader`] only applies the format-level rules (frames, header,
//! envelope, density, vocabulary) so that a slightly-off log stays readable
//! for us; [`validate_log`] is the strict check for writers and tests.
use std::collections::{HashMap, HashSet};

use dshx_core::{SessionEvent, SessionHeader};
use serde_json::{Map, Value};

use crate::error::{Result, SessionError};
use crate::format::{encode_header_line, is_known_event_type};
use crate::surface::fold_surface;

type Obj = Map<String, Value>;

fn fail<T>(message: impl Into<String>) -> Result<T, String> {
    Err(message.into())
}

fn count(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|v| *v < (1u64 << 53))
}

fn nonempty_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn ignorable(event: &SessionEvent) -> bool {
    event.extra.get("ignorable") == Some(&Value::Bool(true))
}

/// Validates a complete log (header + events from seq 0) like dsh's open path.
pub fn validate_log(header: &SessionHeader, events: &[SessionEvent]) -> Result<()> {
    let wrap = |seq: Option<u64>, e: String| {
        SessionError::InvalidEvent(match seq {
            Some(seq) => format!("seq {seq}: {e}"),
            None => e,
        })
    };
    encode_header_line(header)?;
    let mut last_inherited = None;
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64 {
            return Err(wrap(None, format!("format v4 event {index} is not dense")));
        }
        if !is_known_event_type(&event.kind) && !ignorable(event) {
            return Err(SessionError::Unsupported(format!(
                "format v4 contains unknown event type {:?} at seq {index}",
                event.kind
            )));
        }
        row_admission(event).map_err(|e| wrap(Some(event.seq), e))?;
        if is_known_event_type(&event.kind) {
            message_sources(event).map_err(|e| wrap(Some(event.seq), e))?;
            envelope(event).map_err(|e| wrap(Some(event.seq), e))?;
        }
        if event.kind == "session/end-seed"
            && event.data.get("inherited") == Some(&Value::Bool(true))
        {
            last_inherited = Some(event.seq);
        }
        if event.kind == "session-log-deepseek/delivery-accepted" {
            delivery(event, header).map_err(|e| wrap(Some(event.seq), e))?;
        }
    }
    if header.is_seeded != last_inherited.is_some() {
        return Err(wrap(
            None,
            "format v4 seeded header disagrees with its inherited end-seed marker".into(),
        ));
    }
    let mut relationships = Relationships::new(events);
    for event in events {
        relationships
            .accept(event)
            .map_err(|e| wrap(Some(event.seq), e))?;
    }
    fold_surface(events)?;
    Ok(())
}

// --- row admission (codec.ts assertV4RowAdmission) ---

fn row_admission(event: &SessionEvent) -> Result<(), String> {
    if !(event.kind == "developer/message" && ignorable(event) && !is_known_event_type(&event.kind))
    {
        developer_data(event)?;
    }
    plugin_sources(event)?;
    retired_syntax(event)?;
    system_message_fields(event)?;
    if event.kind == "tool/result" {
        tool_result_message(event)?;
        fork_result(event)?;
    }
    Ok(())
}

fn tool_change_block(block: &Value, developer: bool) -> Result<(), String> {
    let kind = block.get("type").and_then(Value::as_str);
    if kind != Some("tool-addition") && kind != Some("tool-removal") {
        return Ok(());
    }
    let kind = kind.unwrap_or_default();
    if !developer {
        return fail(format!("format v4 {kind} requires developer role"));
    }
    if nonempty_str(block.get("toolName")).is_none() {
        return fail(format!("format v4 {kind} requires a nonempty toolName"));
    }
    if kind == "tool-addition" && block.get("tool").is_some() {
        return fail("format v4 tool-addition must omit inline tool definitions");
    }
    Ok(())
}

fn content_blocks(value: Option<&Value>) -> impl Iterator<Item = &Value> {
    value.and_then(Value::as_array).into_iter().flatten()
}

fn ordinary_message(message: Option<&Value>) -> Result<(), String> {
    let Some(message) = message.and_then(Value::as_object) else {
        return Ok(());
    };
    if message.get("role").and_then(Value::as_str) == Some("developer") {
        return fail("format v4 developer/message and developer role must occur together");
    }
    content_blocks(message.get("content")).try_for_each(|b| tool_change_block(b, false))
}

fn developer_data(event: &SessionEvent) -> Result<(), String> {
    let Some(data) = event.data.as_object() else {
        return if event.kind == "developer/message" {
            fail("format v4 developer/message data must be an object")
        } else {
            Ok(())
        };
    };
    match event.kind.as_str() {
        "developer/message" => {
            let message = data
                .get("message")
                .and_then(Value::as_object)
                .filter(|m| m.get("role").and_then(Value::as_str) == Some("developer"))
                .ok_or(
                    "format v4 developer/message requires turn, step, and a developer message",
                )?;
            for field in ["turn", "step"] {
                if count(data.get(field)).unwrap_or(0) == 0 {
                    return fail(format!("developer/message {field} must be positive"));
                }
            }
            content_blocks(message.get("content")).try_for_each(|b| tool_change_block(b, true))?;
            let source_ok = message
                .get("source")
                .and_then(Value::as_object)
                .and_then(|s| nonempty_str(s.get("kind")))
                .is_some_and(|k| k != "plugin");
            if nonempty_str(message.get("id")).is_none()
                || !message.get("content").is_some_and(Value::is_array)
                || !source_ok
            {
                return fail(
                    "format v4 developer message requires id, role, content, and a producer-owned source",
                );
            }
            let additions = content_blocks(message.get("content"))
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool-addition"));
            if additions && count(data.get("headerSeq")).is_none() {
                return fail("developer/message headerSeq must be a count");
            }
            if !additions && data.contains_key("headerSeq") {
                return fail(
                    "format v4 developer/message must omit headerSeq without tool additions",
                );
            }
        }
        "user/message" => ordinary_message(Some(&event.data))?,
        "system/message" | "assistant/message" | "tool/result" => {
            ordinary_message(data.get("message"))?
        }
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let key = if event.kind == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            content_blocks(data.get(key)).try_for_each(|m| ordinary_message(Some(m)))?;
        }
        _ => {}
    }
    if event.kind == "compaction/summary" {
        content_blocks(data.get("summary")).try_for_each(|b| tool_change_block(b, false))?;
        content_blocks(data.get("rawOutput")).try_for_each(|b| tool_change_block(b, false))?;
    }
    if event.kind == "assistant/message" || event.kind == "assistant/attempt" {
        for entry in content_blocks(data.get("stream")) {
            if entry.get("type").and_then(Value::as_str) != Some("chunk") {
                continue;
            }
            let Some(chunk) = entry.get("chunk").and_then(Value::as_object) else {
                continue;
            };
            match chunk.get("type").and_then(Value::as_str) {
                Some("block-end") => {
                    tool_change_block(chunk.get("block").unwrap_or(&Value::Null), false)?
                }
                Some("block-start")
                    if matches!(
                        chunk.get("blockType").and_then(Value::as_str),
                        Some("tool-addition" | "tool-removal")
                    ) =>
                {
                    return fail("format v4 tool-change blocks require developer role");
                }
                _ => {}
            }
        }
    }
    if event.kind == "request/header" {
        for tool in content_blocks(event.data.pointer("/header/tools")) {
            if let Some(defer) = tool.get("deferLoading")
                && defer != &Value::Bool(true)
            {
                return fail("format v4 tool deferLoading must be true when present");
            }
        }
    }
    Ok(())
}

/// The messages an event embeds (`sources.ts` `mapEventMessages`).
fn embedded_messages(event: &SessionEvent) -> Vec<&Value> {
    match event.kind.as_str() {
        "user/message" => vec![&event.data],
        "system/message" | "assistant/message" | "tool/result" | "developer/message" => {
            event.data.get("message").into_iter().collect()
        }
        "agent/inbox/spliced" => content_blocks(event.data.get("inserted")).collect(),
        "session/title-llm-request" => content_blocks(event.data.get("messages")).collect(),
        _ => Vec::new(),
    }
}

fn producer_source(message: &Value) -> Result<(), String> {
    let ok = message
        .get("source")
        .and_then(Value::as_object)
        .and_then(|s| nonempty_str(s.get("kind")))
        .is_some_and(|k| k != "plugin");
    if ok {
        Ok(())
    } else {
        fail("format v4 message requires a producer-owned source kind")
    }
}

fn plugin_sources(event: &SessionEvent) -> Result<(), String> {
    for message in embedded_messages(event) {
        if message.pointer("/source/kind").and_then(Value::as_str) == Some("plugin") {
            producer_source(message)?;
        }
    }
    Ok(())
}

fn message_sources(event: &SessionEvent) -> Result<(), String> {
    embedded_messages(event)
        .into_iter()
        .filter(|m| m.is_object())
        .try_for_each(producer_source)
}

fn retired_syntax(event: &SessionEvent) -> Result<(), String> {
    let no_wrapper = |blocks: Option<&Value>| -> Result<(), String> {
        if content_blocks(blocks)
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool-result"))
        {
            return fail(format!(
                "format v4 {} at seq {} content must not contain a released tool-result wrapper",
                event.kind, event.seq
            ));
        }
        Ok(())
    };
    if event.kind == "request/header" {
        let header = event.data.get("header").and_then(Value::as_object);
        let Some(header) = header else {
            return fail("format v4 request/header requires data and header objects");
        };
        if header.contains_key("system") {
            return fail("format v4 request/header rejects retired header.system");
        }
        return Ok(());
    }
    let Some(data) = event.data.as_object() else {
        return Ok(());
    };
    match event.kind.as_str() {
        "user/message" => no_wrapper(data.get("content"))?,
        "developer/message" | "assistant/message" | "team/message/queued" => {
            no_wrapper(data.get("message").and_then(|m| m.get("content")))?;
        }
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let key = if event.kind == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            for message in content_blocks(data.get(key)) {
                no_wrapper(message.get("content"))?;
            }
        }
        "compaction/summary" => {
            no_wrapper(data.get("summary"))?;
            no_wrapper(data.get("rawOutput"))?;
        }
        "tool/ptc-dispatch" => no_wrapper(data.get("content"))?,
        _ => {}
    }
    if event.kind == "assistant/message" || event.kind == "assistant/attempt" {
        for entry in content_blocks(data.get("stream")) {
            let Some(chunk) = entry
                .get("chunk")
                .filter(|_| entry.get("type") == Some(&Value::from("chunk")))
            else {
                continue;
            };
            let wrapped = match chunk.get("type").and_then(Value::as_str) {
                Some("block-end") => {
                    chunk.pointer("/block/type") == Some(&Value::from("tool-result"))
                }
                Some("block-start") => chunk.get("blockType") == Some(&Value::from("tool-result")),
                _ => false,
            };
            if wrapped {
                return fail("assistant stream must not contain a released tool-result wrapper");
            }
        }
    }
    Ok(())
}

fn system_message_fields(event: &SessionEvent) -> Result<(), String> {
    if event.kind != "system/message" {
        return Ok(());
    }
    let data = event
        .data
        .as_object()
        .ok_or("format v4 system/message data requires an object")?;
    for field in ["turn", "step"] {
        if count(data.get(field)).unwrap_or(0) == 0 {
            return fail(format!("format v4 system/message {field} must be positive"));
        }
    }
    let message = data
        .get("message")
        .and_then(Value::as_object)
        .ok_or("format v4 system message requires an object")?;
    if nonempty_str(message.get("id")).is_none() {
        return fail("format v4 system message id requires a string with content");
    }
    if message.get("role").and_then(Value::as_str) != Some("system") {
        return fail("format v4 system message requires system role");
    }
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or("format v4 system message requires content array")?;
    for block in content {
        let block = block
            .as_object()
            .ok_or("format v4 system content block requires an object")?;
        match nonempty_str(block.get("type"))
            .ok_or("format v4 system content type requires a string")?
        {
            "text" | "reasoning" => {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("format v4 system content text requires a string")?;
            }
            "tool-call" => {
                if nonempty_str(block.get("id")).is_none()
                    || nonempty_str(block.get("name")).is_none()
                    || !block.get("arguments").is_some_and(Value::is_string)
                {
                    return fail("format v4 system tool-call requires id, name and arguments");
                }
            }
            "image" => image_attachment(block.get("attachment"))?,
            "tool-result" => {
                return fail("format v4 system content rejects retired tool-result wrappers");
            }
            _ => {}
        }
    }
    Ok(())
}

fn image_attachment(value: Option<&Value>) -> Result<(), String> {
    let a = value
        .and_then(Value::as_object)
        .ok_or("format v4 system image attachment requires an object")?;
    let positive = |v: Option<&Value>| count(v).is_some_and(|n| n > 0);
    let media_ok = matches!(
        a.get("mediaType").and_then(Value::as_str),
        Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
    );
    let original_ok = a
        .get("originalDimensions")
        .is_none_or(|o| positive(o.get("width")) && positive(o.get("height")));
    if nonempty_str(a.get("attachmentId")).is_none()
        || !media_ok
        || count(a.get("bytes")).is_none()
        || !positive(a.get("width"))
        || !positive(a.get("height"))
        || !a.get("name").is_none_or(Value::is_string)
        || !original_ok
    {
        return fail("format v4 system image attachment is invalid");
    }
    Ok(())
}

fn tool_result_message(event: &SessionEvent) -> Result<(), String> {
    let subject = format!("format v4 tool/result at seq {}", event.seq);
    let data = event
        .data
        .as_object()
        .ok_or(format!("{subject} data must be an object"))?;
    let message = data
        .get("message")
        .and_then(Value::as_object)
        .ok_or(format!("{subject} message must be an object"))?;
    if nonempty_str(message.get("id")).is_none() {
        return fail(format!(
            "{subject} requires a first-class message with a string id"
        ));
    }
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return fail(format!("{subject} requires a tool-role message"));
    }
    let call_id = nonempty_str(message.get("toolCallId"));
    let source = message.get("source").and_then(Value::as_object);
    if call_id.is_none() || source.and_then(|s| s.get("callId")).and_then(Value::as_str) != call_id
    {
        return fail(format!(
            "{subject} requires toolCallId matching its tool source"
        ));
    }
    if source.and_then(|s| s.get("kind")).and_then(Value::as_str) != Some("tool") {
        return fail(format!("{subject} requires a tool source"));
    }
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or(format!("{subject} requires array content"))?;
    if content
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("tool-result"))
    {
        return fail(format!(
            "{subject} content must not contain a released tool-result wrapper"
        ));
    }
    if !message.get("isError").is_none_or(Value::is_boolean) {
        return fail(format!("{subject} isError must be boolean when present"));
    }
    if data.get("error").is_some() && message.get("isError") != Some(&Value::Bool(true)) {
        return fail(format!(
            "{subject} carries error metadata for a non-error tool result"
        ));
    }
    Ok(())
}

fn fork_result(event: &SessionEvent) -> Result<(), String> {
    let data = &event.data;
    let id = data
        .pointer("/message/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if data.pointer("/error/code").and_then(Value::as_str) != Some("TOOL_NOT_STARTED")
        || !id.starts_with("forked-tool-result-")
    {
        return Ok(());
    }
    let call_id = data
        .pointer("/message/source/callId")
        .and_then(Value::as_str);
    let op = event.extra.get("surfaceOp");
    let replacement = op.and_then(|o| o.get("op")).and_then(Value::as_str) == Some("replace");
    let suffix = call_id.and_then(|c| id.strip_prefix(&format!("forked-tool-result-{c}-")));
    let number = suffix
        .filter(|s| s == &"0" || (!s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())))
        .and_then(|s| s.parse::<u64>().ok());
    let sources = event.extra.get("sourceEventSeqs").and_then(Value::as_array);
    let content = data.pointer("/message/content").and_then(Value::as_array);
    let ok = number.is_some_and(|n| {
        if replacement {
            n < event.seq
        } else {
            n == event.seq
        }
    }) && data.pointer("/error/name").and_then(Value::as_str)
        == Some("ToolNotStartedError")
        && (replacement || (sources.is_none() && op == Some(&Value::from("append"))))
        && (!replacement || sources.is_some_and(|s| s.len() == 1 && s[0].as_u64() == number))
        && data.pointer("/message/role").and_then(Value::as_str) == Some("tool")
        && data.pointer("/message/isError") == Some(&Value::Bool(true))
        && data.pointer("/message/toolCallId").and_then(Value::as_str) == call_id
        && data.pointer("/message/source/kind").and_then(Value::as_str) == Some("tool")
        && content.is_some_and(|c| {
            c.len() == 1
                && c[0].get("type") == Some(&Value::from("text"))
                && c[0].get("text").is_some_and(Value::is_string)
        });
    if ok {
        Ok(())
    } else {
        fail("invalid V4 not-started fork result")
    }
}

fn delivery(event: &SessionEvent, header: &SessionHeader) -> Result<(), String> {
    let data = event
        .data
        .as_object()
        .ok_or("delivery-accepted data must be an object")?;
    let version = match data.get("sessionFormatVersion") {
        None => 0,
        Some(v) => count(Some(v)).ok_or("delivery sessionFormatVersion must be a count")?,
    };
    if version != 4 {
        return Ok(());
    }
    let through = count(data.get("throughSeq")).ok_or("delivery throughSeq must be a count")?;
    if through >= event.seq {
        return fail("delivery throughSeq must precede its marker");
    }
    let id =
        nonempty_str(data.get("sessionId")).ok_or("delivery requires a nonempty Session id")?;
    if !header.extra.contains_key("parentSession") && id != header.id {
        return fail("current-generation delivery marker names the wrong Session");
    }
    Ok(())
}

// --- installed Session restore checks (core index.ts) ---

fn envelope(event: &SessionEvent) -> Result<(), String> {
    let data = event.data.as_object();
    let surface = matches!(
        event.kind.as_str(),
        "system/message"
            | "developer/message"
            | "user/message"
            | "assistant/message"
            | "tool/result"
    );
    if surface && let Some(data) = data {
        let message = if event.kind == "user/message" {
            Some(&event.data)
        } else {
            data.get("message")
        };
        if let Some(message) = message.and_then(Value::as_object) {
            let developer_role = message.get("role").and_then(Value::as_str) == Some("developer");
            if (event.kind == "developer/message") != developer_role {
                return fail("developer/message and developer role must occur together");
            }
        }
    }
    match event.kind.as_str() {
        "request/header" => request_header(data.ok_or("request/header data must be an object")?)?,
        "tool/result" => {
            let data = data.ok_or("tool/result data must be an object")?;
            if data.get("error").is_some()
                && event.data.pointer("/message/isError") != Some(&Value::Bool(true))
            {
                return fail("tool/result error requires message.isError === true");
            }
        }
        "assistant/attempt" => settlement(data)?,
        _ => {}
    }
    if surface {
        message_shape(event)?;
        if event.kind == "assistant/message" {
            settlement(data)?;
        }
    }
    Ok(())
}

fn request_header(data: &Obj) -> Result<(), String> {
    let header = data
        .get("header")
        .and_then(Value::as_object)
        .ok_or("request/header header must be an object")?;
    if header.contains_key("system") {
        return fail("request/header must omit header.system; use system/message");
    }
    if header
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return fail("request/header must omit empty tools");
    }
    let config = header.get("config").and_then(Value::as_object);
    let Some(config) = config.filter(|c| {
        nonempty_str(c.get("provider")).is_some() && nonempty_str(c.get("model")).is_some()
    }) else {
        return fail("request/header lacks provider/model");
    };
    if let Some(effort) = config.get("reasoningEffort")
        && nonempty_str(Some(effort)).is_none()
    {
        return fail("request/header has an invalid reasoningEffort");
    }
    if let Some(defaults) = header.get("adapterDefaults") {
        let defaults = defaults
            .as_object()
            .ok_or("request/header has invalid adapterDefaults")?;
        let ok = !defaults.is_empty()
            && defaults.iter().all(|(k, v)| {
                matches!(k.as_str(), "reasoningEffort" | "maxTokens")
                    && v == &Value::Bool(true)
                    && config.contains_key(k)
            });
        if !ok {
            return fail("request/header has invalid adapterDefaults");
        }
    }
    if !matches!(
        data.get("reason").and_then(Value::as_str),
        Some("initial" | "resume" | "change" | "series")
    ) {
        return fail("request/header has an invalid reason");
    }
    if data
        .get("startsSeries")
        .is_some_and(|v| v != &Value::Bool(true))
    {
        return fail("request/header has an invalid startsSeries marker");
    }
    Ok(())
}

fn settlement(data: Option<&Obj>) -> Result<(), String> {
    let data = data.ok_or("assistant event has invalid settlement fields")?;
    if count(data.get("turn")).is_none()
        || count(data.get("step")).is_none()
        || !data.get("stream").is_some_and(Value::is_array)
    {
        return fail("assistant event has invalid settlement fields (turn, step, stream)");
    }
    Ok(())
}

fn message_shape(event: &SessionEvent) -> Result<(), String> {
    let message = if event.kind == "user/message" {
        Some(&event.data)
    } else {
        event.data.get("message")
    };
    let message = message
        .and_then(Value::as_object)
        .filter(|m| nonempty_str(m.get("id")).is_some());
    let Some(message) = message else {
        return fail(format!("{} lacks an identified message", event.kind));
    };
    let role = match event.kind.as_str() {
        "system/message" => "system",
        "developer/message" => "developer",
        "user/message" => "user",
        "assistant/message" => "assistant",
        _ => "tool",
    };
    if message.get("role").and_then(Value::as_str) != Some(role) {
        return fail(format!("{} message must have role \"{role}\"", event.kind));
    }
    let source = message.get("source").and_then(Value::as_object);
    let kind = source.and_then(|s| nonempty_str(s.get("kind")));
    let Some(kind) = kind else {
        return fail(format!("{} message has invalid source", event.kind));
    };
    if !message.get("content").is_some_and(Value::is_array) {
        return fail(format!("{} message has invalid content", event.kind));
    }
    let source = source.expect("checked above");
    match event.kind.as_str() {
        "system/message" if kind != "system-prompt" => {
            fail("system/message must have system-prompt source")
        }
        "assistant/message"
            if kind != "model"
                || nonempty_str(source.get("provider")).is_none()
                || nonempty_str(source.get("model")).is_none() =>
        {
            fail("assistant/message must have model source")
        }
        "tool/result" => {
            let call = nonempty_str(source.get("callId"));
            if kind != "tool" || call.is_none() {
                return fail("tool/result message must have tool source");
            }
            if message.get("toolCallId").and_then(Value::as_str) != call {
                return fail("tool/result message has mismatched tool call ids");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// --- lifecycle relationships (relationships.ts) ---

const RELATIONSHIP_TYPES: &[&str] = &[
    "system/message",
    "user/message",
    "developer/message",
    "assistant/message",
    "tool/result",
    "assistant/attempt",
    "turn/start",
    "turn/end",
    "step/start",
    "step/end",
    "tool/call",
    "request/header",
    "request/context",
    "tool/ptc-dispatch-start",
    "tool/ptc-dispatch",
    "llm/retry",
    "llm/retry-started",
    "session/title",
    "session/title-llm-request",
    "command/run",
    "command/done",
    "compaction/start",
    "compaction/summary",
    "compaction/end",
    "compaction/prune",
    "session/end-seed",
];

struct ToolState {
    name: Value,
    arguments: Value,
    started: bool,
}

struct Compaction {
    id: String,
    command: Option<Value>,
    turn: Option<u64>,
    seq: u64,
    summarized: bool,
}

struct Relationships<'a> {
    events: &'a [SessionEvent],
    turn: Option<u64>,
    step: Option<u64>,
    next_turn: u64,
    next_step: u64,
    provider: Option<Value>,
    surface: Vec<u64>,
    protected_head: Option<u64>,
    compaction: Option<Compaction>,
    tools: Vec<(String, ToolState)>,
    dispatches: HashMap<String, (Obj, bool)>,
    retries: Vec<Obj>,
    started_retries: HashSet<(String, u64)>,
    commands: HashSet<String>,
    orphan_compactions: HashSet<u64>,
}

impl<'a> Relationships<'a> {
    fn new(events: &'a [SessionEvent]) -> Self {
        // A compaction left open before a restore marker belongs to an ended lifecycle.
        let mut orphan_compactions = HashSet::new();
        let mut start = None;
        for event in events.iter().filter(|e| is_known_event_type(&e.kind)) {
            match event.kind.as_str() {
                "compaction/start" => start = Some(event.seq),
                "compaction/end" => start = None,
                "session/end-seed" => {
                    if let Some(s) = start.take() {
                        orphan_compactions.insert(s);
                    }
                }
                _ => {}
            }
        }
        Relationships {
            events,
            turn: None,
            step: None,
            next_turn: 1,
            next_step: 1,
            provider: None,
            surface: Vec::new(),
            protected_head: None,
            compaction: None,
            tools: Vec::new(),
            dispatches: HashMap::new(),
            retries: Vec::new(),
            started_retries: HashSet::new(),
            commands: HashSet::new(),
            orphan_compactions,
        }
    }

    fn require_turn(&self, kind: &str) -> Result<(), String> {
        if self.turn.is_none() {
            fail(format!("{kind} is outside an open turn"))
        } else {
            Ok(())
        }
    }

    fn require_step(&self, kind: &str, data: &Obj) -> Result<(), String> {
        let matches = self.turn.is_some()
            && self.step.is_some()
            && count(data.get("turn")) == self.turn
            && count(data.get("step")) == self.step;
        if matches {
            Ok(())
        } else {
            fail(format!("{kind} does not match an open turn and step"))
        }
    }

    fn close_tools(&mut self, kind: &str) -> Result<(), String> {
        if let Some((id, _)) = self.tools.first() {
            return fail(format!("{kind} leaves unresolved tool call {id}"));
        }
        Ok(())
    }

    fn earlier(value: Option<&Value>, seq: u64, subject: &str) -> Result<u64, String> {
        let coordinate = count(value).ok_or(format!("{subject} must be a count"))?;
        if coordinate >= seq {
            return fail(format!("{subject} must name an earlier event"));
        }
        Ok(coordinate)
    }

    fn fold_surface(&mut self, event: &SessionEvent) -> Result<(), String> {
        if !crate::format::is_surface_event_type(&event.kind) {
            return Ok(());
        }
        if event.kind == "system/message"
            && !self.surface.is_empty()
            && self.protected_head.is_none()
        {
            return fail("system/message requires a protected first surface head");
        }
        let op = event.extra.get("surfaceOp");
        if op == Some(&Value::from("append")) {
            if event.kind == "system/message" && self.surface.is_empty() {
                self.protected_head = Some(event.seq);
            }
            self.surface.push(event.seq);
            return Ok(());
        }
        let op = op
            .and_then(Value::as_object)
            .ok_or(format!("{} surfaceOp requires an object", event.kind))?;
        let start = Self::earlier(op.get("startSeq"), event.seq, "replacement start")?;
        let end = Self::earlier(op.get("endSeq"), event.seq, "replacement end")?;
        let first = self.surface.iter().position(|s| *s == start);
        let last = self.surface.iter().position(|s| *s == end);
        let (Some(first), Some(last)) = (first, last) else {
            return fail(format!(
                "{} replacement range is not on the current surface",
                event.kind
            ));
        };
        if last < first {
            return fail(format!(
                "{} replacement range is not on the current surface",
                event.kind
            ));
        }
        let removed: Vec<u64> = self.surface[first..=last].to_vec();
        let sources: Vec<u64> = event
            .extra
            .get("sourceEventSeqs")
            .and_then(Value::as_array)
            .ok_or("replacement sourceEventSeqs requires an array")?
            .iter()
            .filter_map(Value::as_u64)
            .collect();
        if removed.iter().any(|s| !sources.contains(s)) {
            return fail("replacement sourceEventSeqs omit a shadowed surface node");
        }
        if let Some(head) = self.protected_head
            && removed.contains(&head)
        {
            if event.kind != "system/message" || removed.len() != 1 {
                return fail("surface replacement cannot shadow the protected system head");
            }
            self.protected_head = Some(event.seq);
        }
        self.surface.splice(first..=last, [event.seq]);
        Ok(())
    }

    fn tool(&mut self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        if event.kind == "tool/result"
            && event.extra.get("surfaceOp") != Some(&Value::from("append"))
        {
            return self.require_turn(&event.kind);
        }
        self.require_step(&event.kind, data)?;
        if event.kind == "assistant/message" {
            let content = data
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array);
            for block in content.ok_or("assistant content requires an array")? {
                if block.get("type").and_then(Value::as_str) != Some("tool-call") {
                    continue;
                }
                let id = nonempty_str(block.get("id"))
                    .ok_or("tool call id requires a nonempty string")?;
                if self.tools.iter().any(|(t, _)| t == id) {
                    return fail(format!(
                        "assistant/message repeats advertised tool call {id}"
                    ));
                }
                let state = ToolState {
                    name: block.get("name").cloned().unwrap_or(Value::Null),
                    arguments: block.get("arguments").cloned().unwrap_or(Value::Null),
                    started: false,
                };
                self.tools.push((id.to_owned(), state));
            }
            return Ok(());
        }
        let message = (event.kind == "tool/result")
            .then(|| data.get("message"))
            .flatten();
        let id = if event.kind == "tool/result" {
            nonempty_str(message.and_then(|m| m.get("toolCallId")))
        } else {
            nonempty_str(data.get("callId"))
        }
        .ok_or("tool call id requires a nonempty string")?
        .to_owned();
        let Some(index) = self.tools.iter().position(|(t, _)| *t == id) else {
            return fail(format!(
                "{} {id} has no advertised tool lifecycle",
                event.kind
            ));
        };
        if event.kind == "tool/call" {
            let pending = &mut self.tools[index].1;
            if pending.started
                || Some(&pending.name) != data.get("name")
                || Some(&pending.arguments) != data.get("arguments")
            {
                return fail(format!(
                    "tool/call {id} does not match one advertised tool call"
                ));
            }
            pending.started = true;
        } else {
            if !self.tools[index].1.started && !not_started_repair(event, data, &id) {
                return fail(format!(
                    "tool/result {id} is not the exact TOOL_NOT_STARTED repair"
                ));
            }
            self.tools.remove(index);
        }
        Ok(())
    }

    fn dispatch(&mut self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        self.require_turn(&event.kind)?;
        let id = nonempty_str(data.get("subCallId"))
            .ok_or("PTC subCallId requires a nonempty string")?
            .to_owned();
        let root = nonempty_str(data.get("rootCallId"))
            .ok_or("PTC rootCallId requires a nonempty string")?;
        let parent = nonempty_str(data.get("parentCallId"))
            .ok_or("PTC parentCallId requires a nonempty string")?;
        if let Some((existing, _)) = self.dispatches.get(&id)
            && existing.get("rootCallId").and_then(Value::as_str) != Some(root)
        {
            return fail("PTC dispatch changes its rootCallId");
        }
        if parent != root
            && self
                .dispatches
                .get(parent)
                .and_then(|(d, _)| d.get("rootCallId"))
                .and_then(Value::as_str)
                != Some(root)
        {
            return fail("PTC parentCallId does not belong to rootCallId");
        }
        if event.kind == "tool/ptc-dispatch-start" {
            if self.dispatches.contains_key(&id) {
                return fail("PTC dispatch repeats subCallId");
            }
            self.dispatches.insert(id, (data.clone(), false));
            return Ok(());
        }
        let Some((start, settled)) = self
            .dispatches
            .get_mut(&id)
            .filter(|(_, settled)| !*settled)
        else {
            return fail("PTC dispatch has no unique start");
        };
        if ["rootCallId", "parentCallId", "name", "arguments"]
            .iter()
            .any(|k| start.get(*k) != data.get(*k))
        {
            return fail("PTC dispatch does not match its start");
        }
        *settled = true;
        Ok(())
    }

    fn retry(&mut self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        let id = nonempty_str(data.get("retryId"))
            .ok_or("retryId requires a nonempty string")?
            .to_owned();
        let attempt = count(data.get("retry")).ok_or("retry must be a count")?;
        if event.kind == "llm/retry-started" {
            let scheduled = self
                .retries
                .iter()
                .find(|r| {
                    r.get("retryId").and_then(Value::as_str) == Some(&id)
                        && count(r.get("retry")) == Some(attempt)
                })
                .ok_or("llm/retry-started pairs no prior scheduled attempt")?;
            if scheduled.get("turn") != data.get("turn")
                || scheduled.get("step") != data.get("step")
            {
                return fail("llm/retry-started changes scheduled coordinates");
            }
            if !self.started_retries.insert((id, attempt)) {
                return fail("llm/retry-started repeats one scheduled attempt");
            }
            return Ok(());
        }
        let step = self.step.unwrap_or(self.next_step.saturating_sub(1));
        if self.turn.is_none()
            || count(data.get("turn")) != self.turn
            || count(data.get("step")) != Some(step)
        {
            return fail("llm/retry does not match the current turn and step");
        }
        if data.get("provider") != self.provider.as_ref() {
            return fail("llm/retry provider does not match the open request/header");
        }
        let prior = self.retries.iter().rev().find(|r| {
            ["turn", "step", "provider", "policyKey"]
                .iter()
                .all(|k| r.get(*k) == data.get(*k))
        });
        let expected = prior.map_or(1, |p| count(p.get("retry")).unwrap_or(0) + 1);
        if attempt != expected {
            return fail("llm/retry skips its policy attempt sequence");
        }
        let same_id = match prior {
            None => !self
                .retries
                .iter()
                .any(|r| r.get("retryId").and_then(Value::as_str) == Some(&id)),
            Some(p) => p.get("retryId").and_then(Value::as_str) == Some(&id),
        };
        if !same_id {
            return fail("llm/retry must keep one retryId per policy chain");
        }
        self.retries.push(data.clone());
        Ok(())
    }

    fn compact(&mut self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        if event.kind == "compaction/prune" || event.kind == "compaction/summary" {
            self.span(event, data)?;
        }
        if event.kind == "compaction/prune" {
            return Ok(());
        }
        if event.kind == "compaction/start" {
            if self.compaction.is_some() {
                return fail("compaction/start overlaps an open compaction");
            }
            let owner = match data.get("turn") {
                Some(Value::Null) => None,
                other => Some(count(other).ok_or("compaction turn must be a count")?),
            };
            if owner != self.turn {
                return fail("compaction/start does not match the open turn");
            }
            self.compaction = Some(Compaction {
                id: nonempty_str(data.get("compactionId"))
                    .ok_or("compactionId requires a nonempty string")?
                    .to_owned(),
                command: data.get("sourceCommandId").cloned(),
                turn: owner,
                seq: event.seq,
                summarized: false,
            });
            return Ok(());
        }
        let turn = self.turn;
        let current = self.compaction_owner(data, &event.kind)?;
        if current.turn != turn {
            return fail(format!("{} does not match the open turn", event.kind));
        }
        if event.kind == "compaction/summary" {
            if current.summarized {
                return fail("compaction/summary repeats");
            }
            current.summarized = true;
        } else {
            let owner = match data.get("turn") {
                Some(Value::Null) => None,
                other => count(other),
            };
            if owner != current.turn {
                return fail("compaction/end changes its owner turn");
            }
            if data.get("error").is_none() && !current.summarized {
                return fail("successful compaction/end requires one summary");
            }
            self.compaction = None;
        }
        Ok(())
    }

    fn compaction_owner(&mut self, data: &Obj, subject: &str) -> Result<&mut Compaction, String> {
        match self.compaction.as_mut() {
            Some(c)
                if Some(c.id.as_str()) == data.get("compactionId").and_then(Value::as_str)
                    && c.command.as_ref() == data.get("sourceCommandId") =>
            {
                Ok(c)
            }
            _ => fail(format!("{subject} has no matching compaction/start")),
        }
    }

    fn span(&self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        let range = data
            .get("shadowedRange")
            .and_then(Value::as_object)
            .ok_or("compaction shadowedRange requires an object")?;
        let start = Self::earlier(range.get("start"), event.seq, "compaction range start")?;
        let end = Self::earlier(range.get("end"), event.seq, "compaction range end")?;
        let seqs: Vec<Value> = data
            .get("shadowedSeqs")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("compaction shadowedSeqs requires an array")?;
        let first = self.surface.iter().position(|s| *s == start);
        let last = self.surface.iter().position(|s| *s == end);
        let exact = match (first, last) {
            (Some(f), Some(l)) if l >= f => {
                self.surface[f..=l]
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>()
                    == seqs
            }
            _ => false,
        };
        if !exact {
            return fail(format!(
                "{} shadowedSeqs do not name an exact current surface span",
                event.kind
            ));
        }
        if let Some(head) = self.protected_head
            && seqs.contains(&Value::from(head))
        {
            return fail("compaction cannot shadow the protected system head");
        }
        Ok(())
    }

    fn accept(&mut self, event: &SessionEvent) -> Result<(), String> {
        if !is_known_event_type(&event.kind) || !RELATIONSHIP_TYPES.contains(&event.kind.as_str()) {
            return Ok(());
        }
        let data = event
            .data
            .as_object()
            .ok_or(format!("{} requires an object", event.kind))?;
        if matches!(
            event.kind.as_str(),
            "system/message" | "developer/message" | "assistant/attempt"
        ) {
            self.require_step(&event.kind, data)?;
        }
        if event.kind.starts_with("turn/")
            && let Some(c) = &self.compaction
            && !self.orphan_compactions.contains(&c.seq)
        {
            return fail(format!("{} crosses an open compaction", event.kind));
        }
        self.fold_surface(event)?;
        match event.kind.as_str() {
            "turn/start" => {
                if self.turn.is_some() || count(data.get("turn")) != Some(self.next_turn) {
                    return fail("turn/start does not open the expected turn");
                }
                self.turn = Some(self.next_turn);
                self.next_step = 1;
                self.tools.clear();
            }
            "turn/end" => {
                if self.turn.is_none()
                    || count(data.get("turn")) != self.turn
                    || self.step.is_some()
                {
                    return fail("turn/end does not match the open turn with no open step");
                }
                self.close_tools(&event.kind)?;
                self.turn = None;
                self.next_turn += 1;
            }
            "step/start" => {
                if self.turn.is_none()
                    || count(data.get("turn")) != self.turn
                    || self.step.is_some()
                    || count(data.get("step")) != Some(self.next_step)
                {
                    return fail("step/start does not match the open turn and next step");
                }
                self.step = Some(self.next_step);
            }
            "step/end" => {
                self.require_step(&event.kind, data)?;
                self.close_tools(&event.kind)?;
                self.step = None;
                self.next_step += 1;
            }
            "assistant/message" | "tool/call" | "tool/result" => self.tool(event, data)?,
            "developer/message" => {
                if let Some(seq) = data.get("headerSeq") {
                    let header_seq =
                        Self::earlier(Some(seq), event.seq, "developer/message headerSeq")?;
                    if self
                        .events
                        .get(header_seq as usize)
                        .map(|e| e.kind.as_str())
                        != Some("request/header")
                    {
                        return fail(
                            "developer/message headerSeq must reference an earlier request/header",
                        );
                    }
                }
            }
            "request/header" => {
                self.require_turn(&event.kind)?;
                self.provider = event.data.pointer("/header/config/provider").cloned();
            }
            "request/context" => self.require_turn(&event.kind)?,
            "tool/ptc-dispatch-start" | "tool/ptc-dispatch" => self.dispatch(event, data)?,
            "llm/retry" | "llm/retry-started" => self.retry(event, data)?,
            "session/title" | "session/title-llm-request" => self.title_sources(event, data)?,
            "command/run" => {
                let id = nonempty_str(data.get("commandId"))
                    .ok_or("commandId requires a nonempty string")?;
                if !self.commands.insert(id.to_owned()) {
                    return fail(format!("command/run repeats commandId {id}"));
                }
            }
            "command/done" => {
                let id = nonempty_str(data.get("commandId"))
                    .ok_or("commandId requires a nonempty string")?;
                if !self.commands.contains(id) {
                    return fail("command/done has no prior command/run");
                }
                if let Some(seq) = data.get("sourceEventSeq") {
                    let source = &self.events
                        [Self::earlier(Some(seq), event.seq, "command sourceEventSeq")? as usize];
                    if data.get("kind").and_then(Value::as_str) != Some("success")
                        || source.kind == "command/run"
                        || source.kind == "command/done"
                    {
                        return fail("command/done has invalid sourceEventSeq");
                    }
                }
            }
            "compaction/start" | "compaction/summary" | "compaction/end" | "compaction/prune" => {
                self.compact(event, data)?
            }
            "user/message" => {
                let source = data
                    .get("source")
                    .and_then(Value::as_object)
                    .ok_or("user message source requires an object")?;
                if event.extra.get("surfaceOp") != Some(&Value::from("append"))
                    && source.get("kind").and_then(Value::as_str) == Some("compact-checkpoint")
                {
                    self.compaction_owner(source, "compaction checkpoint")?;
                }
            }
            "session/end-seed" => self.compaction = None,
            _ => {}
        }
        Ok(())
    }

    fn title_sources(&self, event: &SessionEvent, data: &Obj) -> Result<(), String> {
        let references = data
            .get("messageSeqs")
            .and_then(Value::as_array)
            .ok_or("title messageSeqs requires an array")?;
        if event.kind == "session/title" {
            let user = data
                .get("source")
                .and_then(|s| s.get("kind"))
                .and_then(Value::as_str)
                == Some("user");
            if references.is_empty() != user {
                return fail("session/title messageSeqs must be empty exactly for a user title");
            }
        }
        let mut seen = HashSet::new();
        for reference in references {
            let seq = Self::earlier(Some(reference), event.seq, "title messageSeqs")?;
            let source = &self.events[seq as usize];
            if !seen.insert(seq)
                || source.kind != "user/message"
                || source.data.pointer("/source/kind").and_then(Value::as_str) != Some("user")
            {
                return fail(format!(
                    "{} messageSeqs must cite distinct earlier human user/message events",
                    event.kind
                ));
            }
        }
        if event.kind != "session/title-llm-request" {
            return Ok(());
        }
        let messages = data
            .get("messages")
            .and_then(Value::as_array)
            .ok_or("title messages requires an array")?;
        let message = messages.first();
        let content = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        let ok = !references.is_empty()
            && messages.len() == 1
            && message.and_then(|m| m.get("role")).and_then(Value::as_str) == Some("user")
            && message
                .and_then(|m| m.pointer("/source/kind"))
                .and_then(Value::as_str)
                == Some("dsh-session-title-llm")
            && content
                .is_some_and(|c| c.len() == 1 && c[0].get("type") == Some(&Value::from("text")));
        if ok {
            Ok(())
        } else {
            fail("session/title-llm-request messages do not represent messageSeqs")
        }
    }
}

/// The exact `TOOL_NOT_STARTED` repair shape dsh accepts for an unstarted call.
fn not_started_repair(event: &SessionEvent, data: &Obj, call_id: &str) -> bool {
    let error = data.get("error").and_then(Value::as_object);
    let message = data.get("message");
    let ok_error = error.is_some_and(|e| {
        e.get("name").and_then(Value::as_str) == Some("ToolNotStartedError")
            && e.get("code").and_then(Value::as_str) == Some("TOOL_NOT_STARTED")
    });
    if !ok_error
        || message.and_then(|m| m.get("isError")) != Some(&Value::Bool(true))
        || event.extra.contains_key("sourceEventSeqs")
    {
        return false;
    }
    let id = message
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.starts_with(&format!("forked-tool-result-{call_id}-")) {
        return true;
    }
    let Some(suffix) = id.strip_prefix(&format!("interrupted-tool-result-{call_id}-")) else {
        return false;
    };
    let numeric = suffix == "0"
        || (!suffix.is_empty()
            && !suffix.starts_with('0')
            && suffix.bytes().all(|b| b.is_ascii_digit()));
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    numeric
        && content.is_some_and(|c| {
            c.len() == 1
                && c[0].get("type") == Some(&Value::from("text"))
                && c[0].get("text").and_then(Value::as_str)
                    == Some("The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.")
        })
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

    fn header() -> SessionHeader {
        crate::SessionStore::new_header("/w")
    }

    fn turn() -> Vec<SessionEvent> {
        let call = json!({"type":"tool-call","id":"c1","name":"bash","arguments":"{}"});
        vec![
            ev(0, "turn/start", json!({"turn":1}), json!({})),
            ev(1, "step/start", json!({"turn":1,"step":1}), json!({})),
            ev(
                2,
                "user/message",
                json!({"role":"user","content":[{"type":"text","text":"hi"}],"source":{"kind":"user"},"id":"u"}),
                json!({"surfaceOp":"append"}),
            ),
            ev(
                3,
                "request/header",
                json!({"header":{"config":{"provider":"p","model":"m"}},"reason":"initial"}),
                json!({}),
            ),
            ev(
                4,
                "assistant/message",
                json!({"turn":1,"step":1,"message":{"role":"assistant","content":[call],"source":{"kind":"model","provider":"p","model":"m"},"id":"a"},"stream":[]}),
                json!({"surfaceOp":"append"}),
            ),
            ev(
                5,
                "tool/call",
                json!({"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{}"}),
                json!({}),
            ),
            ev(
                6,
                "tool/result",
                json!({"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"c1"},"toolCallId":"c1","content":[],"isError":false,"id":"r"}}),
                json!({"surfaceOp":"append","sourceEventSeqs":[5]}),
            ),
            ev(7, "step/end", json!({"turn":1,"step":1}), json!({})),
            ev(
                8,
                "session/title",
                json!({"title":"hi","messageSeqs":[2],"source":{"kind":"fallback"}}),
                json!({}),
            ),
            ev(
                9,
                "turn/end",
                json!({"turn":1,"reason":{"kind":"completed"}}),
                json!({}),
            ),
        ]
    }

    #[test]
    fn accepts_a_well_formed_turn() {
        validate_log(&header(), &turn()).unwrap();
    }

    #[test]
    fn refuses_what_dsh_refuses() {
        let check = |f: &dyn Fn(&mut Vec<SessionEvent>)| {
            let mut events = turn();
            f(&mut events);
            validate_log(&header(), &events).is_err()
        };
        assert!(
            check(&|e| e[5].data["arguments"] = json!("{\"x\":1}")),
            "tool/call must match the advertised call"
        );
        assert!(check(&|e| e.swap(7, 6)), "result after step/end");
        assert!(
            check(&|e| {
                e.remove(7);
                e.iter_mut()
                    .enumerate()
                    .for_each(|(i, ev)| ev.seq = i as u64)
            }),
            "turn/end with open step"
        );
        assert!(
            check(&|e| e[8].data["messageSeqs"] = json!([4])),
            "title must cite a human user/message"
        );
        assert!(
            check(&|e| e[3].data["reason"] = json!("whatever")),
            "request/header reason"
        );
        assert!(
            check(&|e| {
                e[4].data.as_object_mut().unwrap().remove("stream");
            }),
            "assistant needs stream"
        );
        assert!(
            check(&|e| e[2].data["source"]["kind"] = json!("plugin")),
            "plugin sources are retired"
        );
        assert!(
            check(&|e| {
                e[6].extra.remove("surfaceOp");
            }),
            "surface marker required"
        );
        assert!(
            check(&|e| e[0].kind = "future/event".into()),
            "unknown required event"
        );
    }
}
