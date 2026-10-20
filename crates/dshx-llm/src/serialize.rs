//! Canonical history → DeepSeek Messages request body
//! (dsh `llm-deepseek/src/serialize.ts`, plus the route projections dsh's
//! `LlmRuntime` applies before the adapter).
use std::collections::{HashMap, HashSet};

use base64::Engine;
use dshx_core::{
    ContentBlock, GenerateRequest, ImageAttachmentRef, LlmFailure, Message, MessageSource, Role,
    ToolSchema,
};
use serde_json::{Map, Value, json};

use crate::config::DeepSeekConfig;
use crate::images::{
    ImagePathResolver, RequestImage, base64_len, image_offload_required,
    offloaded_image_prefix_count, offloaded_image_text, request_image_handle_text,
    text_only_image_text,
};
use crate::models::{CatalogModel, REASONING_EFFORTS, SystemPromptUpdate, ToolUpdate};
use crate::replay::{block_type, read_replay};

/// Beta flag required when history carries `tool_addition`/`tool_removal` blocks.
pub const TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";

/// A serialized request.
#[derive(Debug, Clone, PartialEq)]
pub struct WireRequest {
    /// JSON body; write it with [`crate::json::to_vec`] for dsh-identical bytes.
    pub body: Value,
    /// History contains tool changes: send `anthropic-beta: TOOL_CHANGES_BETA`.
    pub tool_changes: bool,
}

/// Request-local inputs besides the [`GenerateRequest`].
pub struct BuildContext<'a> {
    pub config: &'a DeepSeekConfig,
    /// Our provider route; replay metadata of other providers is ignored.
    pub provider: &'a str,
    /// Prepared bytes of every retained image, keyed by attachment id.
    pub images: &'a HashMap<String, RequestImage>,
    pub image_path: Option<&'a ImagePathResolver>,
}

fn unsupported(what: impl std::fmt::Display) -> LlmFailure {
    LlmFailure::new(
        "UNSUPPORTED_CONTENT",
        format!("DeepSeek Messages cannot represent {what}"),
    )
}

fn invalid(message: &str) -> LlmFailure {
    LlmFailure::new("INVALID_REQUEST", format!("DeepSeek Messages {message}"))
}

/// Historical arguments Messages cannot represent are sent as `{}`;
/// the durable content stays unchanged.
fn tool_input(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

/// The history as this route receives it: images projected for text-only
/// models, offloaded images as placeholders, developer updates filtered by
/// the route's tool-update mode, and foreign replay metadata dropped.
pub fn project_messages(
    messages: &[Message],
    model: Option<&CatalogModel>,
    provider: &str,
    image_path: Option<&ImagePathResolver>,
) -> Vec<Message> {
    let accepts_images = model.is_some_and(CatalogModel::accepts_images);
    let tool_update = model.and_then(|m| m.tool_update);
    let path = |r: &ImageAttachmentRef| image_path.and_then(|f| f(r));
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == Role::Developer {
            match tool_update {
                // Routes without tool updates get the complete current tool list instead.
                None => continue,
                Some(ToolUpdate::AdditionOnly) => {
                    let content: Vec<ContentBlock> = message
                        .content
                        .iter()
                        .filter(|b| !matches!(b, ContentBlock::ToolRemoval { .. }))
                        .cloned()
                        .collect();
                    if content.is_empty() {
                        continue;
                    }
                    out.push(Message {
                        content,
                        ..message.clone()
                    });
                    continue;
                }
                Some(ToolUpdate::InHistory) => {}
            }
        }
        let mut message = message.clone();
        if message
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }))
        {
            message.content = message
                .content
                .into_iter()
                .map(|block| match block {
                    ContentBlock::Image { attachment, .. } if !accepts_images => {
                        ContentBlock::Text {
                            text: text_only_image_text(&attachment),
                        }
                    }
                    ContentBlock::Image {
                        attachment,
                        offloaded: true,
                    } => ContentBlock::Text {
                        text: offloaded_image_text(&attachment, path(&attachment).as_deref()),
                    },
                    other => other,
                })
                .collect();
        }
        if let MessageSource::Model {
            provider: source,
            replay_state,
            ..
        } = &mut message.source
            && source != provider
        {
            *replay_state = None;
        }
        out.push(message);
    }
    out
}

/// Attachment ids of retained images, in first-seen order. Only user and
/// tool messages may carry images.
pub fn request_image_refs(messages: &[Message]) -> Result<Vec<ImageAttachmentRef>, LlmFailure> {
    let mut seen = HashSet::new();
    let mut refs = vec![];
    for message in messages {
        for block in &message.content {
            if let ContentBlock::Image { attachment, .. } = block {
                if !matches!(message.role, Role::User | Role::Tool) {
                    return Err(LlmFailure::new(
                        "UNSUPPORTED_CONTENT",
                        "DeepSeek Messages supports images only in user messages and tool results",
                    ));
                }
                if seen.insert(attachment.attachment_id.clone()) {
                    refs.push(attachment.clone());
                }
            }
        }
    }
    Ok(refs)
}

struct WireMessage {
    role: &'static str,
    content: Vec<Value>,
}

/// Serializes one complete request from already projected history
/// (see [`project_messages`]) and prepared image bytes.
pub fn serialize(
    request: &GenerateRequest,
    history: &[Message],
    ctx: &BuildContext<'_>,
) -> Result<WireRequest, LlmFailure> {
    let model = ctx.config.model(&request.model);
    let in_history =
        model.and_then(|m| m.system_prompt_update) == Some(SystemPromptUpdate::InHistory);

    let input = |blocks: &[ContentBlock]| -> Result<Vec<Value>, LlmFailure> {
        let mut out = vec![];
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    if !text.is_empty() {
                        out.push(json!({"type": "text", "text": text}));
                    }
                }
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {}
                ContentBlock::Image { attachment, .. } => {
                    let Some(version) = ctx.images.get(&attachment.attachment_id) else {
                        return Err(invalid("request image is missing"));
                    };
                    let path = ctx.image_path.and_then(|f| f(attachment));
                    out.push(json!({
                        "type": "text",
                        "text": request_image_handle_text(attachment, version.width, version.height, path.as_deref()),
                    }));
                    out.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": version.media_type,
                            "data": base64::engine::general_purpose::STANDARD.encode(&version.data),
                        },
                    }));
                }
                other => {
                    return Err(unsupported(format!(
                        "user/tool-result content {}",
                        block_type(other)
                    )));
                }
            }
        }
        Ok(out)
    };

    let mut messages: Vec<WireMessage> = vec![];
    let mut history_system: Option<String> = None;
    let mut system_updates: Vec<WireMessage> = vec![];
    // Harness admits system updates before user input; Messages places the
    // update after that user/tool-result turn and before the next assistant.
    let flush = |messages: &mut Vec<WireMessage>,
                 updates: &mut Vec<WireMessage>|
     -> Result<(), LlmFailure> {
        if updates.is_empty() {
            return Ok(());
        }
        if messages.last().map(|m| m.role) != Some("user") {
            return Err(unsupported(
                "system update without a preceding user or tool-result turn",
            ));
        }
        messages.append(updates);
        Ok(())
    };

    for message in history {
        if message.role == Role::Developer {
            let mut content = vec![];
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text.is_empty() {
                            content.push(json!({"type": "text", "text": text}));
                        }
                    }
                    ContentBlock::ToolAddition { tool_name } => content
                        .push(json!({"type": "tool_addition", "tool": {"type": "tool_reference", "name": tool_name}})),
                    ContentBlock::ToolRemoval { tool_name } => content
                        .push(json!({"type": "tool_removal", "tool": {"type": "tool_reference", "name": tool_name}})),
                    other => return Err(unsupported(format!("developer content {}", block_type(other)))),
                }
            }
            if !content.is_empty() {
                system_updates.push(WireMessage {
                    role: "system",
                    content,
                });
            }
            continue;
        }
        if message.content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::ToolAddition { .. } | ContentBlock::ToolRemoval { .. }
            )
        }) {
            return Err(unsupported("tool-change blocks outside developer messages"));
        }
        if message.role == Role::System {
            let mut text = String::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text: part } => text.push_str(part),
                    _ => return Err(unsupported("non-text system message")),
                }
            }
            if in_history && !messages.is_empty() {
                if text.is_empty() {
                    return Err(unsupported("empty in-history system update"));
                }
                system_updates.push(WireMessage {
                    role: "system",
                    content: vec![json!({"type": "text", "text": text})],
                });
            } else {
                history_system = Some(text);
            }
            continue;
        }
        let content = match message.role {
            Role::Assistant => assistant_blocks(message, &request.model)?,
            Role::Tool => {
                let call_id = message
                    .tool_call_id
                    .clone()
                    .or_else(|| match &message.source {
                        MessageSource::Tool { call_id } => Some(call_id.clone()),
                        _ => None,
                    });
                let Some(call_id) = call_id else {
                    return Err(invalid("tool result has no matching call"));
                };
                let mut result = Map::new();
                result.insert("type".into(), "tool_result".into());
                result.insert("tool_use_id".into(), call_id.into());
                result.insert("content".into(), input(&message.content)?.into());
                if let Some(is_error) = message.is_error {
                    result.insert("is_error".into(), is_error.into());
                }
                vec![Value::Object(result)]
            }
            _ => input(&message.content)?,
        };
        if matches!(message.role, Role::User | Role::Assistant) && content.is_empty() {
            continue;
        }
        if message.role == Role::Assistant {
            flush(&mut messages, &mut system_updates)?;
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        match messages.last_mut() {
            Some(previous) if previous.role == role => previous.content.extend(content),
            _ => messages.push(WireMessage { role, content }),
        }
    }
    flush(&mut messages, &mut system_updates)?;

    // Every tool call needs its result in the next user turn; results go first.
    let mut pending: HashSet<String> = HashSet::new();
    for message in &mut messages {
        let id_of =
            |block: &Value, key: &str| block.get(key).and_then(Value::as_str).map(str::to_owned);
        if message.role == "assistant" {
            let calls: Vec<String> = message
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| id_of(b, "id"))
                .collect();
            pending = calls.iter().cloned().collect();
            if pending.len() != calls.len() {
                return Err(invalid("duplicate tool call id"));
            }
        } else if message.role == "user" {
            let (results, rest): (Vec<Value>, Vec<Value>) = std::mem::take(&mut message.content)
                .into_iter()
                .partition(|b| b["type"] == "tool_result");
            for result in &results {
                if !id_of(result, "tool_use_id").is_some_and(|id| pending.remove(&id)) {
                    return Err(invalid("tool result has no matching call"));
                }
            }
            if !pending.is_empty() {
                return Err(invalid("tool calls need immediate results"));
            }
            message.content = results.into_iter().chain(rest).collect();
        }
    }
    if !pending.is_empty() {
        return Err(invalid("history ends with unresolved tools"));
    }

    // Inline images must fit the base64 budget; the agent offloads the oldest.
    let lengths: Vec<u64> = history
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Image { attachment, .. } => ctx.images.get(&attachment.attachment_id),
            _ => None,
        })
        .map(|image| base64_len(image.data.len() as u64))
        .collect();
    let offload = offloaded_image_prefix_count(
        &lengths,
        ctx.config.max_images_per_request,
        ctx.config.max_inline_request_image_bytes,
        ctx.config.image_offload_count_quantum,
        ctx.config.inline_image_offload_byte_quantum,
    );
    if offload > 0 {
        return Err(image_offload_required(offload));
    }

    let effort = request
        .reasoning_effort
        .clone()
        .or_else(|| ctx.config.reasoning_effort.clone())
        .unwrap_or_else(|| {
            if ctx.config.thinking_disabled {
                "off".into()
            } else {
                "high".into()
            }
        });
    if !REASONING_EFFORTS.contains(&effort.as_str())
        || (ctx.config.thinking_disabled && effort != "off")
    {
        return Err(LlmFailure::new(
            "UNSUPPORTED_REASONING_EFFORT",
            format!("DeepSeek Messages does not support reasoning effort {effort}"),
        ));
    }

    let tool_changes = messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| b["type"] == "tool_addition" || b["type"] == "tool_removal")
    });
    let deferred: HashSet<&str> = history
        .iter()
        .filter(|m| m.role == Role::Developer)
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolAddition { tool_name } => Some(tool_name.as_str()),
            _ => None,
        })
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), request.model.clone().into());
    body.insert("stream".into(), true.into());
    body.insert(
        "messages".into(),
        messages
            .into_iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect::<Vec<_>>()
            .into(),
    );
    let max_tokens = request
        .max_tokens
        .or_else(|| model.and_then(|m| m.max_tokens))
        .unwrap_or(ctx.config.max_tokens);
    body.insert("max_tokens".into(), max_tokens.into());
    body.insert(
        "thinking".into(),
        json!({"type": if effort == "off" { "disabled" } else { "enabled" }}),
    );
    if effort != "off" {
        body.insert("output_config".into(), json!({"effort": effort}));
    }
    if let Some(system) = history_system.filter(|s| !s.is_empty()) {
        body.insert("system".into(), system.into());
    }
    if let Some(temperature) = request.temperature {
        // The shortest f32 text is the number a JS caller would have written.
        let value: f64 = temperature
            .to_string()
            .parse()
            .unwrap_or(temperature as f64);
        body.insert("temperature".into(), json!(value));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            request
                .tools
                .iter()
                .map(|t| tool_declaration(t, &deferred))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    Ok(WireRequest {
        body: Value::Object(body),
        tool_changes,
    })
}

fn tool_declaration(tool: &ToolSchema, deferred: &HashSet<&str>) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), tool.name.clone().into());
    map.insert("description".into(), tool.description.clone().into());
    map.insert("input_schema".into(), tool.parameters.clone());
    // A tool activated later by `tool_addition` is declared deferred.
    if deferred.contains(tool.name.as_str()) {
        map.insert("defer_loading".into(), true.into());
    }
    Value::Object(map)
}

fn assistant_blocks(message: &Message, model: &str) -> Result<Vec<Value>, LlmFailure> {
    let replay = read_replay(message, model, &mut |reason| {
        tracing::warn!(
            model,
            reason,
            "discarding unusable DeepSeek replay metadata"
        );
    });
    let mut out = vec![];
    for (index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    out.push(json!({"type": "text", "text": text}));
                }
            }
            ContentBlock::Reasoning { text } => {
                let mut thinking = Map::new();
                thinking.insert("type".into(), "thinking".into());
                thinking.insert("thinking".into(), text.clone().into());
                if let Some(Some(signature)) = replay.as_ref().and_then(|r| r.get(index)) {
                    thinking.insert("signature".into(), signature.clone().into());
                }
                out.push(Value::Object(thinking));
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                out.push(json!({"type": "tool_use", "id": id, "name": name, "input": tool_input(arguments)}));
            }
            other => {
                return Err(unsupported(format!(
                    "assistant content {}",
                    block_type(other)
                )));
            }
        }
    }
    Ok(out)
}

/// Projects `request.messages` for its route and serializes them, given the
/// image bytes for every retained image ([`request_image_refs`] of the
/// projected history). This is everything the client sends, minus headers.
pub fn build_request(
    request: &GenerateRequest,
    ctx: &BuildContext<'_>,
) -> Result<WireRequest, LlmFailure> {
    let history = project_messages(
        &request.messages,
        ctx.config.model(&request.model),
        ctx.provider,
        ctx.image_path,
    );
    serialize(request, &history, ctx)
}
