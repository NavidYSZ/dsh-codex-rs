//! Canonical history → Anthropic Messages request body.
use std::collections::{HashMap, HashSet};

use base64::Engine;
use dshx_core::{
    ContentBlock, GenerateRequest, LlmFailure, Message, MessageSource, Role, ToolSchema,
};
use serde_json::{Map, Value, json};

use super::LABEL;
use super::replay::{Replay, read_replay};
use crate::images::{
    ImagePathResolver, RequestImage, base64_len, offloaded_image_prefix_count,
    request_image_handle_text,
};
use crate::provider::{DEFAULT_REQUEST_MAX_TOKENS, ProviderModel, ProviderSpec, Thinking};
use crate::replay::block_type;

/// Images per request before the oldest are offloaded: above 20 the API
/// applies a stricter per-image size limit (claude-api vision docs).
pub const MAX_IMAGES_PER_REQUEST: u64 = 20;
pub const IMAGE_OFFLOAD_COUNT_QUANTUM: u64 = 5;
/// Base64 image budget of one request, well under the 32 MB request limit.
pub const MAX_INLINE_REQUEST_IMAGE_BYTES: u64 = 24 * 1024 * 1024;
pub const INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM: u64 = 8 * 1024 * 1024;

/// Request-local inputs besides the [`GenerateRequest`].
pub struct Build<'a> {
    pub spec: &'a ProviderSpec,
    /// Prepared bytes of every retained image, keyed by attachment id.
    pub images: &'a HashMap<String, RequestImage>,
    pub image_path: Option<&'a ImagePathResolver>,
}

fn unsupported(what: impl std::fmt::Display) -> LlmFailure {
    LlmFailure::new(
        "UNSUPPORTED_CONTENT",
        format!("{LABEL} cannot represent {what}"),
    )
}

fn invalid(message: &str) -> LlmFailure {
    LlmFailure::new("INVALID_REQUEST", format!("{LABEL} {message}"))
}

/// Historical arguments the API cannot take are sent as `{}`.
pub(crate) fn tool_input(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

struct WireMessage {
    role: &'static str,
    content: Vec<Value>,
}

/// Serializes one request from projected history (see
/// [`crate::serialize::project_messages`]) and prepared image bytes.
pub fn serialize(
    request: &GenerateRequest,
    history: &[Message],
    ctx: &Build<'_>,
) -> Result<Value, LlmFailure> {
    let model = ctx.spec.model(&request.model);
    let across_models = ctx.spec.official_anthropic();
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
                    let Some(image) = ctx.images.get(&attachment.attachment_id) else {
                        return Err(invalid("request image is missing"));
                    };
                    let path = ctx.image_path.and_then(|f| f(attachment));
                    out.push(json!({
                        "type": "text",
                        "text": request_image_handle_text(attachment, image.width, image.height, path.as_deref()),
                    }));
                    out.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": base64::engine::general_purpose::STANDARD.encode(&image.data),
                        },
                    }));
                }
                other => {
                    return Err(unsupported(format!("user content {}", block_type(other))));
                }
            }
        }
        Ok(out)
    };

    let mut system: Option<String> = None;
    let mut messages: Vec<WireMessage> = vec![];
    for message in history {
        let content = match message.role {
            // Tool changes reach this route as the full tool list.
            Role::Developer => continue,
            Role::System => {
                // The agent keeps the current prompt in the first system node and blanks the rest.
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    system = Some(text);
                }
                continue;
            }
            Role::Assistant => assistant_blocks(message, &request.model, ctx.spec, across_models)?,
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
                if message.is_error == Some(true) {
                    result.insert("is_error".into(), true.into());
                }
                vec![Value::Object(result)]
            }
            _ => input(&message.content)?,
        };
        if content.is_empty() {
            continue;
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

    // Every tool call needs its result in the next user turn; results go first.
    let mut pending: HashSet<String> = HashSet::new();
    for message in &mut messages {
        let id_of =
            |block: &Value, key: &str| block.get(key).and_then(Value::as_str).map(str::to_owned);
        if message.role == "assistant" {
            if !pending.is_empty() {
                return Err(invalid("tool calls need immediate results"));
            }
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
        } else {
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

    // Inline images must fit the request; the agent offloads the oldest.
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
        MAX_IMAGES_PER_REQUEST,
        MAX_INLINE_REQUEST_IMAGE_BYTES,
        IMAGE_OFFLOAD_COUNT_QUANTUM,
        INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM,
    );
    if offload > 0 {
        return Err(offload_required(offload));
    }

    let cache = ctx.spec.compat.cache_control;
    if cache
        && let Some(last) = messages.last_mut().and_then(|m| m.content.last_mut())
        && let Some(block) = last.as_object_mut()
    {
        block.insert("cache_control".into(), ephemeral());
    }

    let max_tokens = max_tokens(request, model);
    let mut body = Map::new();
    body.insert("model".into(), request.model.clone().into());
    body.insert("max_tokens".into(), max_tokens.into());
    body.insert("stream".into(), true.into());
    if let Some(system) = system {
        let mut block = Map::new();
        block.insert("type".into(), "text".into());
        block.insert("text".into(), system.into());
        if cache {
            block.insert("cache_control".into(), ephemeral());
        }
        body.insert("system".into(), json!([Value::Object(block)]));
    }
    body.insert(
        "messages".into(),
        messages
            .into_iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect::<Vec<_>>()
            .into(),
    );
    if !request.tools.is_empty() {
        let last = request.tools.len() - 1;
        let tools: Vec<Value> = request
            .tools
            .iter()
            .enumerate()
            .map(|(i, t)| tool_declaration(t, cache && i == last))
            .collect();
        body.insert("tools".into(), tools.into());
    }
    if let Some(model) = model {
        thinking(
            &mut body,
            model,
            request.reasoning_effort.as_deref(),
            max_tokens,
        );
    }
    Ok(Value::Object(body))
}

/// `IMAGE_OFFLOAD_REQUIRED`: the agent offloads `count` more of the oldest images.
pub(crate) fn offload_required(count: u64) -> LlmFailure {
    LlmFailure {
        offload_images: Some(count),
        ..LlmFailure::new(
            "IMAGE_OFFLOAD_REQUIRED",
            format!(
                "{LABEL} request images exceed the route budget; {count} more oldest occurrence(s) must be offloaded."
            ),
        )
    }
}

/// The request's cap (or this route's default), never above the model's.
pub(crate) fn max_tokens(request: &GenerateRequest, model: Option<&ProviderModel>) -> u32 {
    let cap = model
        .map(|m| m.max_tokens)
        .unwrap_or(DEFAULT_REQUEST_MAX_TOKENS);
    request
        .max_tokens
        .unwrap_or(DEFAULT_REQUEST_MAX_TOKENS)
        .min(cap)
        .max(1)
}

fn tool_declaration(tool: &ToolSchema, cache: bool) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), tool.name.clone().into());
    map.insert("description".into(), tool.description.clone().into());
    map.insert("input_schema".into(), tool.parameters.clone());
    if cache {
        map.insert("cache_control".into(), ephemeral());
    }
    Value::Object(map)
}

/// Thinking budget of a level for models without an effort parameter.
fn budget_of(effort: &str) -> u32 {
    match effort {
        "minimal" => 1024,
        "low" => 4096,
        "medium" => 8192,
        "high" => 16_384,
        "xhigh" => 24_576,
        _ => 32_768,
    }
}

/// `thinking` and `output_config.effort` for the requested effort; levels
/// the model lacks map to its nearest one ([`ProviderModel::effort`]).
fn thinking(
    body: &mut Map<String, Value>,
    model: &ProviderModel,
    requested: Option<&str>,
    max_tokens: u32,
) {
    let effort = model.effort(requested);
    let off = effort.as_deref() == Some("off");
    match model.thinking {
        Thinking::None => {}
        Thinking::Adaptive | Thinking::Effort if off => {
            body.insert("thinking".into(), json!({"type": "disabled"}));
        }
        Thinking::Adaptive => {
            // Readable summaries instead of the newer models' empty default.
            body.insert(
                "thinking".into(),
                json!({"type": "adaptive", "display": "summarized"}),
            );
            if let Some(effort) = effort {
                body.insert("output_config".into(), json!({"effort": effort}));
            }
        }
        Thinking::Effort => {
            body.insert("thinking".into(), json!({"type": "enabled"}));
            if let Some(effort) = effort {
                body.insert("output_config".into(), json!({"effort": effort}));
            }
        }
        Thinking::Budget => {
            let Some(effort) = effort.filter(|_| !off) else {
                return;
            };
            // The budget must be at least 1024 and below max_tokens.
            let budget = budget_of(&effort).min(max_tokens.saturating_sub(1024));
            if budget >= 1024 {
                body.insert(
                    "thinking".into(),
                    json!({"type": "enabled", "budget_tokens": budget}),
                );
            }
        }
    }
}

fn assistant_blocks(
    message: &Message,
    model: &str,
    spec: &ProviderSpec,
    across_models: bool,
) -> Result<Vec<Value>, LlmFailure> {
    let replay = read_replay(message, &spec.id, model, across_models);
    let mut out = vec![];
    for (index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    out.push(json!({"type": "text", "text": text}));
                }
            }
            // Thinking without this route's signature cannot go back: other
            // providers' reasoning is dropped.
            ContentBlock::Reasoning { text } => match replay.as_ref().and_then(|r| r.get(index)) {
                Some(Replay::Signed(signature)) => out.push(json!({
                    "type": "thinking", "thinking": text, "signature": signature,
                })),
                Some(Replay::Redacted(data)) => {
                    out.push(json!({"type": "redacted_thinking", "data": data}))
                }
                _ => {}
            },
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
    // An interrupted turn that only thought has nothing to answer to.
    if out
        .iter()
        .all(|b| b["type"] == "thinking" || b["type"] == "redacted_thinking")
    {
        out.clear();
    }
    Ok(out)
}
