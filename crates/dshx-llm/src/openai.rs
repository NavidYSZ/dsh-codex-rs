//! OpenAI Chat Completions for OpenAI-compatible endpoints (custom
//! providers only): request body and streaming translation. Reasoning
//! arrives as `reasoning_content` or `reasoning` deltas, images go out as
//! data-URI `image_url` parts, cached input comes from
//! `usage.prompt_tokens_details.cached_tokens`.
use std::collections::{HashMap, HashSet};

use base64::Engine;
use dshx_core::{
    ContentBlock, FinishReason, GenerateRequest, LlmFailure, Message, MessageSource, Role,
    StreamChunk, TokenUsage,
};
use serde_json::{Map, Value, json};

use crate::images::{
    ImagePathResolver, RequestImage, base64_len, offloaded_image_prefix_count,
    request_image_handle_text,
};
use crate::provider::{DEFAULT_REQUEST_MAX_TOKENS, ProviderSpec, ThinkingFormat};
use crate::replay::block_type;
use crate::runner::Translate;
use crate::transport::labeled_error;

/// Protocol name in failure messages.
pub(crate) const LABEL: &str = "OpenAI Chat Completions";
pub const MAX_IMAGES_PER_REQUEST: u64 = 20;
pub const IMAGE_OFFLOAD_COUNT_QUANTUM: u64 = 5;
pub const MAX_INLINE_REQUEST_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
pub const INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM: u64 = 8 * 1024 * 1024;

pub fn completions_url(base: &str) -> String {
    format!("{}/chat/completions", base.trim_end_matches('/'))
}

pub fn models_url(base: &str) -> String {
    format!("{}/models", base.trim_end_matches('/'))
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

/// Request-local inputs besides the [`GenerateRequest`].
pub struct Build<'a> {
    pub spec: &'a ProviderSpec,
    pub images: &'a HashMap<String, RequestImage>,
    pub image_path: Option<&'a ImagePathResolver>,
}

/// Parts of one user turn: text and data-URI images.
struct Parts {
    parts: Vec<Value>,
    images: bool,
}

impl Parts {
    /// A plain string when the turn is text only (what every server takes).
    fn content(self) -> Value {
        if self.images {
            return Value::Array(self.parts);
        }
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect();
        Value::String(texts.join("\n\n"))
    }
}

/// Serializes one request from projected history and prepared image bytes.
pub fn serialize(
    request: &GenerateRequest,
    history: &[Message],
    ctx: &Build<'_>,
) -> Result<Value, LlmFailure> {
    let model = ctx.spec.model(&request.model);
    let parts = |blocks: &[ContentBlock]| -> Result<Parts, LlmFailure> {
        let mut out = Parts {
            parts: vec![],
            images: false,
        };
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    if !text.is_empty() {
                        out.parts.push(json!({"type": "text", "text": text}));
                    }
                }
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {}
                ContentBlock::Image { attachment, .. } => {
                    let Some(image) = ctx.images.get(&attachment.attachment_id) else {
                        return Err(invalid("request image is missing"));
                    };
                    let path = ctx.image_path.and_then(|f| f(attachment));
                    out.parts.push(json!({
                        "type": "text",
                        "text": request_image_handle_text(attachment, image.width, image.height, path.as_deref()),
                    }));
                    let data = base64::engine::general_purpose::STANDARD.encode(&image.data);
                    out.parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{};base64,{data}", image.media_type)},
                    }));
                    out.images = true;
                }
                other => return Err(unsupported(format!("user content {}", block_type(other)))),
            }
        }
        Ok(out)
    };

    let mut system: Option<String> = None;
    let mut messages: Vec<Value> = vec![];
    // Tool messages must follow their assistant message directly; user
    // input of the same turn waits behind them.
    let mut pending: Vec<String> = vec![];
    let mut answered: HashSet<String> = HashSet::new();
    let mut held: Vec<Value> = vec![];
    let mut tool_images: Vec<Value> = vec![];
    let flush = |messages: &mut Vec<Value>, held: &mut Vec<Value>, tool_images: &mut Vec<Value>| {
        if !tool_images.is_empty() {
            let mut content =
                vec![json!({"type": "text", "text": "Images from the tool results above:"})];
            content.append(tool_images);
            messages.push(json!({"role": "user", "content": content}));
        }
        messages.append(held);
    };
    for message in history {
        match message.role {
            Role::Developer => {}
            Role::System => {
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
            }
            Role::Assistant => {
                if pending.iter().any(|id| !answered.contains(id)) {
                    return Err(invalid("tool calls need immediate results"));
                }
                flush(&mut messages, &mut held, &mut tool_images);
                let Some(assistant) = assistant_message(message, request, ctx.spec) else {
                    continue;
                };
                pending = assistant["tool_calls"]
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .filter_map(|c| c["id"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if pending.iter().collect::<HashSet<_>>().len() != pending.len() {
                    return Err(invalid("duplicate tool call id"));
                }
                answered.clear();
                messages.push(assistant);
            }
            Role::Tool => {
                let call_id = message
                    .tool_call_id
                    .clone()
                    .or_else(|| match &message.source {
                        MessageSource::Tool { call_id } => Some(call_id.clone()),
                        _ => None,
                    });
                let Some(call_id) =
                    call_id.filter(|id| pending.contains(id) && !answered.contains(id))
                else {
                    return Err(invalid("tool result has no matching call"));
                };
                answered.insert(call_id.clone());
                let Parts { parts, .. } = parts(&message.content)?;
                let mut texts = vec![];
                for part in parts {
                    if part["type"] == "image_url" {
                        tool_images.push(part);
                    } else if let Some(text) = part["text"].as_str() {
                        texts.push(text.to_owned());
                    }
                }
                messages.push(
                    json!({"role": "tool", "tool_call_id": call_id, "content": texts.join("\n\n")}),
                );
            }
            _ => {
                let parts = parts(&message.content)?;
                if parts.parts.is_empty() {
                    continue;
                }
                held.push(json!({"role": "user", "content": parts.content()}));
                if pending.iter().all(|id| answered.contains(id)) {
                    flush(&mut messages, &mut held, &mut tool_images);
                }
            }
        }
    }
    if pending.iter().any(|id| !answered.contains(id)) {
        return Err(invalid("history ends with unresolved tools"));
    }
    flush(&mut messages, &mut held, &mut tool_images);
    if let Some(system) = system {
        messages.insert(0, json!({"role": "system", "content": system}));
    }

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
        return Err(LlmFailure {
            offload_images: Some(offload),
            ..LlmFailure::new(
                "IMAGE_OFFLOAD_REQUIRED",
                format!(
                    "{LABEL} request images exceed the route budget; {offload} more oldest occurrence(s) must be offloaded."
                ),
            )
        });
    }

    let cap = model
        .map(|m| m.max_tokens)
        .unwrap_or(DEFAULT_REQUEST_MAX_TOKENS);
    let max_tokens = request
        .max_tokens
        .unwrap_or(DEFAULT_REQUEST_MAX_TOKENS)
        .min(cap)
        .max(1);
    let mut body = Map::new();
    body.insert("model".into(), request.model.clone().into());
    body.insert("messages".into(), messages.into());
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
        body.insert("tools".into(), tools.into());
        if ctx.spec.compat.tool_stream {
            body.insert("tool_stream".into(), true.into());
        }
    }
    body.insert("stream".into(), true.into());
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert(ctx.spec.compat.max_tokens_field.clone(), max_tokens.into());
    if let Some(effort) = model.and_then(|m| m.effort(request.reasoning_effort.as_deref())) {
        let format = model.map(|m| m.thinking_format).unwrap_or_default();
        let off = effort == "off";
        match format {
            ThinkingFormat::ReasoningEffort => {}
            ThinkingFormat::DeepSeek => {
                body.insert(
                    "thinking".into(),
                    json!({"type": if off { "disabled" } else { "enabled" }}),
                );
            }
            ThinkingFormat::Zai => {
                body.insert(
                    "thinking".into(),
                    if off {
                        json!({"type": "disabled"})
                    } else {
                        json!({"type": "enabled", "clear_thinking": false})
                    },
                );
            }
        }
        if !off || format == ThinkingFormat::ReasoningEffort {
            body.insert("reasoning_effort".into(), effort.into());
        }
    }
    if let Some(temperature) = request.temperature {
        let value: f64 = temperature
            .to_string()
            .parse()
            .unwrap_or(temperature as f64);
        body.insert("temperature".into(), json!(value));
    }
    Ok(Value::Object(body))
}

/// One assistant message; `None` when nothing of it can be sent.
fn assistant_message(
    message: &Message,
    request: &GenerateRequest,
    spec: &ProviderSpec,
) -> Option<Value> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = vec![];
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::Reasoning { text: t } => reasoning.push_str(t),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                let arguments = match serde_json::from_str::<Value>(arguments) {
                    Ok(Value::Object(_)) => arguments.clone(),
                    _ => "{}".to_owned(),
                };
                calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}}));
            }
            _ => {}
        }
    }
    if text.is_empty() && calls.is_empty() {
        return None;
    }
    let mut out = Map::new();
    out.insert("role".into(), "assistant".into());
    out.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            text.into()
        },
    );
    if !calls.is_empty() {
        out.insert("tool_calls".into(), calls.into());
    }
    // Reasoning only goes back to the model that wrote it, and only where the server wants it.
    let own = matches!(&message.source, MessageSource::Model { provider, model, .. }
        if *provider == spec.id && *model == request.model);
    if spec.compat.reasoning_content {
        // DeepSeek/MiMo need the field even after a model switch. Never
        // replay another model's private reasoning into this route.
        out.insert(
            "reasoning_content".into(),
            if own { reasoning } else { String::new() }.into(),
        );
    }
    Some(Value::Object(out))
}

// ---- streaming ---------------------------------------------------------------

fn malformed(detail: impl std::fmt::Display) -> LlmFailure {
    LlmFailure::new("MALFORMED_RESPONSE", format!("{LABEL} stream: {detail}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Reasoning,
}

#[derive(Debug)]
struct Call {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

/// Stateful translator for one response. Text and reasoning open a new block
/// whenever the delta kind changes; each tool call index is one block.
#[derive(Debug, Default)]
pub struct Translator {
    next: usize,
    open: Option<(Kind, usize, String)>,
    calls: HashMap<u64, Call>,
    call_order: Vec<u64>,
    usage: Option<TokenUsage>,
    reason: Option<FinishReason>,
    any_content: bool,
    done: bool,
}

impl Translator {
    pub fn new() -> Self {
        Self::default()
    }

    fn close_open(&mut self, out: &mut Vec<StreamChunk>) {
        if let Some((kind, index, text)) = self.open.take() {
            let block = match kind {
                Kind::Text => ContentBlock::Text { text },
                Kind::Reasoning => ContentBlock::Reasoning { text },
            };
            out.push(StreamChunk::BlockEnd { index, block });
        }
    }

    fn text(&mut self, kind: Kind, piece: &str, out: &mut Vec<StreamChunk>) {
        if piece.is_empty() {
            return;
        }
        if self.open.as_ref().is_none_or(|(k, _, _)| *k != kind) {
            self.close_open(out);
            let index = self.next;
            self.next += 1;
            out.push(StreamChunk::BlockStart {
                index,
                block_type: if kind == Kind::Text {
                    "text"
                } else {
                    "reasoning"
                },
            });
            self.open = Some((kind, index, String::new()));
        }
        let Some((_, index, text)) = self.open.as_mut() else {
            return;
        };
        text.push_str(piece);
        self.any_content = true;
        out.push(match kind {
            Kind::Text => StreamChunk::TextDelta {
                index: *index,
                text: piece.to_owned(),
            },
            Kind::Reasoning => StreamChunk::ReasoningDelta {
                index: *index,
                text: piece.to_owned(),
            },
        });
    }

    /// Translates one decoded chunk object.
    pub fn push(&mut self, chunk: &Map<String, Value>) -> Result<Vec<StreamChunk>, LlmFailure> {
        if self.done {
            return Ok(vec![]);
        }
        if chunk.get("error").is_some_and(|e| !e.is_null()) {
            return Err(labeled_error(
                LABEL,
                Some(&Value::Object(chunk.clone())),
                None,
                None,
            ));
        }
        let mut out = vec![];
        if let Some(usage) = chunk.get("usage").and_then(Value::as_object) {
            self.usage = Some(map_usage(usage));
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(out);
        };
        if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
            // Servers that send both carry the same text twice.
            let reasoning = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .or_else(|| delta.get("reasoning").and_then(Value::as_str));
            if let Some(piece) = reasoning {
                self.text(Kind::Reasoning, piece, &mut out);
            }
            if let Some(piece) = delta.get("content").and_then(Value::as_str) {
                self.text(Kind::Text, piece, &mut out);
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                self.tool_call(call, &mut out)?;
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.reason = Some(match reason {
                "length" => FinishReason::MaxTokens,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                "content_filter" => FinishReason::Error {
                    failure: LlmFailure::new(
                        "REFUSAL",
                        "The provider's content filter stopped this response",
                    ),
                },
                _ => FinishReason::Stop,
            });
        }
        Ok(out)
    }

    fn tool_call(&mut self, call: &Value, out: &mut Vec<StreamChunk>) -> Result<(), LlmFailure> {
        let key = call
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(self.call_order.len() as u64);
        let function = call.get("function");
        let name = function.and_then(|f| f.get("name")).and_then(Value::as_str);
        let arguments = function
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !self.calls.contains_key(&key) {
            self.close_open(out);
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", dshx_core::new_id()));
            let name = name
                .filter(|n| !n.is_empty())
                .ok_or_else(|| malformed("tool call without a name"))?;
            let index = self.next;
            self.next += 1;
            out.push(StreamChunk::BlockStart {
                index,
                block_type: "tool-call",
            });
            out.push(StreamChunk::ToolCallDelta {
                index,
                id: id.clone(),
                name: Some(name.to_owned()),
                arguments_delta: String::new(),
            });
            self.calls.insert(
                key,
                Call {
                    index,
                    id,
                    name: name.to_owned(),
                    arguments: String::new(),
                },
            );
            self.call_order.push(key);
        }
        let entry = self.calls.get_mut(&key).expect("call exists");
        if !arguments.is_empty() {
            entry.arguments.push_str(arguments);
            out.push(StreamChunk::ToolCallDelta {
                index: entry.index,
                id: entry.id.clone(),
                name: None,
                arguments_delta: arguments.to_owned(),
            });
        }
        self.any_content = true;
        Ok(())
    }

    /// Closes every block and emits usage and the finish.
    fn settle(&mut self) -> Result<Vec<StreamChunk>, LlmFailure> {
        let Some(mut reason) = self.reason.clone() else {
            return Err(LlmFailure::new(
                "STREAM_CLOSED",
                format!("{LABEL} stream ended without a finish reason"),
            ));
        };
        self.done = true;
        if let FinishReason::Error { failure } = reason {
            return Err(failure);
        }
        // Some servers say `stop` after tool calls.
        if reason == FinishReason::Stop && !self.calls.is_empty() {
            reason = FinishReason::ToolCalls;
        }
        let mut out = vec![];
        self.close_open(&mut out);
        for key in &self.call_order {
            let call = &self.calls[key];
            let arguments = if call.arguments.trim().is_empty() {
                "{}".to_owned()
            } else {
                call.arguments.clone()
            };
            if reason != FinishReason::MaxTokens
                && !matches!(
                    serde_json::from_str::<Value>(&arguments),
                    Ok(Value::Object(_))
                )
            {
                return Err(malformed("tool arguments are not a JSON object"));
            }
            out.push(StreamChunk::BlockEnd {
                index: call.index,
                block: ContentBlock::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments,
                },
            });
        }
        if !self.any_content && reason == FinishReason::Stop {
            return Err(LlmFailure::new(
                "EMPTY_RESPONSE",
                format!("{LABEL} returned no content"),
            ));
        }
        let mut usage = self.usage.clone().unwrap_or_default();
        usage.total_tokens =
            Some(usage.input_tokens + usage.output_tokens + usage.cache_read_tokens.unwrap_or(0));
        out.push(StreamChunk::Usage(usage));
        out.push(StreamChunk::Finish {
            reason,
            replay_state: None,
        });
        Ok(out)
    }
}

impl Translate for Translator {
    fn frame(&mut self, _event: Option<&str>, data: &str) -> Result<Vec<StreamChunk>, LlmFailure> {
        if data.trim() == "[DONE]" {
            return self.settle();
        }
        let raw: Value =
            serde_json::from_str(data).map_err(|_| malformed("SSE data is not JSON"))?;
        let Value::Object(object) = raw else {
            return Err(malformed("SSE data is not a JSON object"));
        };
        self.push(&object)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn end(&mut self) -> Result<Vec<StreamChunk>, LlmFailure> {
        if self.done {
            return Ok(vec![]);
        }
        self.settle()
    }
}

/// `prompt_tokens` includes cached input; ours is disjoint. DeepSeek-style
/// servers report `prompt_cache_hit_tokens`, some gateways a top-level
/// `cached_tokens`.
fn map_usage(usage: &Map<String, Value>) -> TokenUsage {
    let count = |v: Option<&Value>| v.and_then(Value::as_u64);
    let prompt = count(usage.get("prompt_tokens")).unwrap_or(0);
    let cached = count(
        usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens")),
    )
    .or_else(|| count(usage.get("prompt_cache_hit_tokens")))
    .or_else(|| count(usage.get("cached_tokens")))
    .unwrap_or(0)
    .min(prompt);
    let reasoning = count(
        usage
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens")),
    );
    TokenUsage {
        input_tokens: prompt - cached,
        output_tokens: count(usage.get("completion_tokens")).unwrap_or(0),
        total_tokens: None,
        cache_read_tokens: Some(cached),
        cache_write_tokens: None,
        reasoning_tokens: reasoning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_split_into_uncached_and_cached() {
        let usage = map_usage(
            json!({"prompt_tokens": 1000, "completion_tokens": 50,
                   "prompt_tokens_details": {"cached_tokens": 800},
                   "completion_tokens_details": {"reasoning_tokens": 20}})
            .as_object()
            .unwrap(),
        );
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens,
                usage.reasoning_tokens
            ),
            (200, Some(800), 50, Some(20))
        );
        let deepseek = map_usage(
            json!({"prompt_tokens": 10, "completion_tokens": 1, "prompt_cache_hit_tokens": 4})
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            (deepseek.input_tokens, deepseek.cache_read_tokens),
            (6, Some(4))
        );
    }

    #[test]
    fn urls() {
        assert_eq!(
            completions_url("https://x.example/v1/"),
            "https://x.example/v1/chat/completions"
        );
        assert_eq!(
            models_url("https://x.example/v1"),
            "https://x.example/v1/models"
        );
    }
}
