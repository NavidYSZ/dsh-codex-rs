//! Anthropic Messages events → harness stream chunks. Same rules as the
//! DeepSeek translator (dense block indexes, cumulative disjoint usage, one
//! `Usage` + `Finish`), plus what the official API adds: `redacted_thinking`
//! blocks, the `refusal` and `model_context_window_exceeded` stop reasons,
//! and block types this route never asks for, which are skipped.
use std::collections::HashMap;

use dshx_core::{ContentBlock, FinishReason, LlmFailure, StreamChunk, TokenUsage};
use serde_json::{Map, Value};

use super::LABEL;
use super::replay::{Replay, replay_state};
use crate::runner::Translate;
use crate::transport::labeled_error;

fn malformed(detail: impl std::fmt::Display) -> LlmFailure {
    LlmFailure::new("MALFORMED_RESPONSE", format!("{LABEL} stream: {detail}"))
}

fn string(value: Option<&Value>, what: &str) -> Result<String, LlmFailure> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("{what} is not a string")))
}

#[derive(Debug)]
enum Content {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        json: String,
    },
}

#[derive(Debug)]
struct Block {
    index: usize,
    content: Content,
    replay: Replay,
    closed: bool,
}

impl Block {
    fn assembled(&self) -> ContentBlock {
        match &self.content {
            Content::Text(text) => ContentBlock::Text { text: text.clone() },
            Content::Reasoning(text) => ContentBlock::Reasoning { text: text.clone() },
            Content::ToolCall { id, name, json } => ContentBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: if json.is_empty() {
                    "{}".into()
                } else {
                    json.clone()
                },
            },
        }
    }
}

/// Stateful translator for one response of `provider`/`model`.
#[derive(Debug)]
pub struct Translator {
    provider: String,
    model: String,
    blocks: HashMap<u64, Block>,
    /// Wire indexes in first-seen order; skipped block types stay out.
    order: Vec<u64>,
    skipped: Vec<u64>,
    usage: TokenUsage,
    started: bool,
    reason: Option<FinishReason>,
    done: bool,
}

impl Translator {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Translator {
            provider: provider.into(),
            model: model.into(),
            blocks: HashMap::new(),
            order: vec![],
            skipped: vec![],
            usage: TokenUsage::default(),
            started: false,
            reason: None,
            done: false,
        }
    }

    /// Translates one decoded event. Errors are terminal for the stream.
    pub fn push(&mut self, event: &Map<String, Value>) -> Result<Vec<StreamChunk>, LlmFailure> {
        if self.done {
            return Ok(vec![]);
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let mut out = vec![];
        match kind {
            "message_start" => {
                if self.started {
                    return Err(malformed("duplicate message_start"));
                }
                let message = event.get("message").and_then(Value::as_object);
                update_usage(&mut self.usage, message.and_then(|m| m.get("usage")));
                self.started = true;
            }
            "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "message_delta"
            | "message_stop"
                if !self.started =>
            {
                return Err(malformed("event precedes message_start"));
            }
            "content_block_start" => {
                let wire = index_of(event)?;
                if self.blocks.contains_key(&wire) || self.skipped.contains(&wire) {
                    return Err(malformed("block index repeats"));
                }
                let Some(block) = start_block(event, self.blocks.len())? else {
                    self.skipped.push(wire);
                    return Ok(vec![]);
                };
                let index = block.index;
                match &block.content {
                    Content::Text(text) => {
                        out.push(StreamChunk::BlockStart {
                            index,
                            block_type: "text",
                        });
                        if !text.is_empty() {
                            out.push(StreamChunk::TextDelta {
                                index,
                                text: text.clone(),
                            });
                        }
                    }
                    Content::Reasoning(text) => {
                        out.push(StreamChunk::BlockStart {
                            index,
                            block_type: "reasoning",
                        });
                        if !text.is_empty() {
                            out.push(StreamChunk::ReasoningDelta {
                                index,
                                text: text.clone(),
                            });
                        }
                    }
                    Content::ToolCall { id, name, .. } => {
                        out.push(StreamChunk::BlockStart {
                            index,
                            block_type: "tool-call",
                        });
                        out.push(StreamChunk::ToolCallDelta {
                            index,
                            id: id.clone(),
                            name: Some(name.clone()),
                            arguments_delta: String::new(),
                        });
                    }
                }
                self.order.push(wire);
                self.blocks.insert(wire, block);
            }
            "content_block_delta" | "content_block_stop" => {
                let wire = index_of(event)?;
                if self.skipped.contains(&wire) {
                    return Ok(vec![]);
                }
                let Some(block) = self.blocks.get_mut(&wire).filter(|b| !b.closed) else {
                    return Err(malformed("delta or stop without an open block"));
                };
                if kind == "content_block_delta" {
                    if let Some(chunk) = delta_chunk(block, event.get("delta"))? {
                        out.push(chunk);
                    }
                } else {
                    block.closed = true;
                    out.push(StreamChunk::BlockEnd {
                        index: block.index,
                        block: block.assembled(),
                    });
                }
            }
            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .filter(|r| !r.is_null())
                {
                    self.reason = Some(stop_reason(reason, event)?);
                }
                update_usage(&mut self.usage, event.get("usage"));
            }
            "message_stop" => {
                let Some(reason) = self.reason.clone() else {
                    return Err(malformed("message_stop without a stop reason"));
                };
                if let FinishReason::Error { failure } = reason {
                    // Partial output of a declined or overflowing request is discarded.
                    self.done = true;
                    return Err(failure);
                }
                if self.blocks.values().any(|b| !b.closed) {
                    return Err(malformed("message_stop with open blocks"));
                }
                if self.blocks.is_empty() && reason == FinishReason::Stop {
                    return Err(LlmFailure::new(
                        "EMPTY_RESPONSE",
                        format!("{LABEL} returned no content"),
                    ));
                }
                // Truncated tool JSON is kept here and pruned by the assembler.
                if reason != FinishReason::MaxTokens {
                    for block in self.blocks.values() {
                        if let ContentBlock::ToolCall { arguments, .. } = block.assembled() {
                            match serde_json::from_str::<Value>(&arguments) {
                                Ok(Value::Object(_)) => {}
                                _ => return Err(malformed("tool input is not a JSON object")),
                            }
                        }
                    }
                }
                let mut usage = self.usage.clone();
                usage.total_tokens = Some(
                    usage.input_tokens
                        + usage.output_tokens
                        + usage.cache_read_tokens.unwrap_or(0)
                        + usage.cache_write_tokens.unwrap_or(0),
                );
                let replay: Vec<Replay> = self
                    .order
                    .iter()
                    .map(|w| self.blocks[w].replay.clone())
                    .collect();
                out.push(StreamChunk::Usage(usage));
                out.push(StreamChunk::Finish {
                    reason,
                    replay_state: Some(replay_state(&self.provider, &self.model, &replay)),
                });
                self.done = true;
            }
            "error" => {
                return Err(labeled_error(
                    LABEL,
                    Some(&Value::Object(event.clone())),
                    None,
                    None,
                ));
            }
            // ping and future event types
            _ => {}
        }
        Ok(out)
    }
}

impl Translate for Translator {
    fn frame(&mut self, event: Option<&str>, data: &str) -> Result<Vec<StreamChunk>, LlmFailure> {
        let raw: Value =
            serde_json::from_str(data).map_err(|_| malformed("SSE data is not JSON"))?;
        let Value::Object(object) = raw else {
            return Err(malformed("SSE data is not a JSON object"));
        };
        let kind = object.get("type").and_then(Value::as_str);
        if kind.is_none() || event.is_some_and(|e| Some(e) != kind) {
            return Err(malformed("SSE event type mismatch"));
        }
        self.push(&object)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn end(&mut self) -> Result<Vec<StreamChunk>, LlmFailure> {
        if self.done {
            Ok(vec![])
        } else {
            Err(LlmFailure::new(
                "STREAM_CLOSED",
                format!("{LABEL} stream ended before message_stop"),
            ))
        }
    }
}

fn index_of(event: &Map<String, Value>) -> Result<u64, LlmFailure> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .filter(|i| *i < (1 << 53))
        .ok_or_else(|| malformed("invalid block index"))
}

/// The block of a `content_block_start`; `None` for types this route skips
/// (server tools and their results, which it never declares).
fn start_block(event: &Map<String, Value>, index: usize) -> Result<Option<Block>, LlmFailure> {
    let native = event
        .get("content_block")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("content_block is not an object"))?;
    let (content, replay) = match native.get("type").and_then(Value::as_str) {
        Some("text") => (
            Content::Text(string(native.get("text"), "text")?),
            Replay::Plain("text"),
        ),
        Some("thinking") => {
            let text = native
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let replay = match native.get("signature").and_then(Value::as_str) {
                Some(signature) if !signature.is_empty() => Replay::Signed(signature.to_owned()),
                _ => Replay::Unsigned,
            };
            (Content::Reasoning(text), replay)
        }
        Some("redacted_thinking") => (
            Content::Reasoning(String::new()),
            Replay::Redacted(string(native.get("data"), "redacted_thinking data")?),
        ),
        Some("tool_use") => {
            let id = string(native.get("id"), "tool_use id")?;
            let name = string(native.get("name"), "tool_use name")?;
            if id.is_empty() || name.is_empty() {
                return Err(malformed("empty tool identity"));
            }
            // A complete input may arrive in the start event instead of deltas.
            let json = match native.get("input") {
                Some(Value::Object(input)) if !input.is_empty() => {
                    crate::json::to_string(&Value::Object(input.clone()))
                }
                _ => String::new(),
            };
            (
                Content::ToolCall { id, name, json },
                Replay::Plain("tool-call"),
            )
        }
        other => {
            tracing::debug!(kind = ?other, "skipping an Anthropic content block");
            return Ok(None);
        }
    };
    Ok(Some(Block {
        index,
        content,
        replay,
        closed: false,
    }))
}

fn delta_chunk(block: &mut Block, raw: Option<&Value>) -> Result<Option<StreamChunk>, LlmFailure> {
    let delta = raw
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("delta is not an object"))?;
    let kind = delta.get("type").and_then(Value::as_str).unwrap_or("");
    match (kind, &mut block.content) {
        ("text_delta", Content::Text(text)) => {
            let piece = string(delta.get("text"), "text_delta")?;
            text.push_str(&piece);
            Ok(Some(StreamChunk::TextDelta {
                index: block.index,
                text: piece,
            }))
        }
        ("thinking_delta", Content::Reasoning(text)) => {
            let piece = string(delta.get("thinking"), "thinking_delta")?;
            text.push_str(&piece);
            Ok(Some(StreamChunk::ReasoningDelta {
                index: block.index,
                text: piece,
            }))
        }
        ("signature_delta", Content::Reasoning(_)) => {
            let piece = string(delta.get("signature"), "signature_delta")?;
            block.replay = match std::mem::replace(&mut block.replay, Replay::Unsigned) {
                Replay::Signed(mut s) => {
                    s.push_str(&piece);
                    Replay::Signed(s)
                }
                _ => Replay::Signed(piece),
            };
            Ok(None)
        }
        ("input_json_delta", Content::ToolCall { id, json, .. }) => {
            let piece = string(delta.get("partial_json"), "input_json_delta")?;
            json.push_str(&piece);
            Ok(Some(StreamChunk::ToolCallDelta {
                index: block.index,
                id: id.clone(),
                name: None,
                arguments_delta: piece,
            }))
        }
        // Citations of text this route never asks for.
        ("citations_delta", Content::Text(_)) => Ok(None),
        (_, content) => {
            let target = match content {
                Content::Text(_) => "text",
                Content::Reasoning(_) => "reasoning",
                Content::ToolCall { .. } => "tool-call",
            };
            Err(malformed(format!("unsupported delta {kind} for {target}")))
        }
    }
}

fn stop_reason(raw: &Value, event: &Map<String, Value>) -> Result<FinishReason, LlmFailure> {
    match raw.as_str() {
        // `pause_turn` only follows server tools, which this route never declares.
        Some("end_turn" | "stop_sequence" | "pause_turn") => Ok(FinishReason::Stop),
        Some("tool_use") => Ok(FinishReason::ToolCalls),
        Some("max_tokens") => Ok(FinishReason::MaxTokens),
        Some("model_context_window_exceeded") => Ok(FinishReason::Error {
            failure: LlmFailure::new(
                crate::CONTEXT_WINDOW_EXCEEDED,
                format!("{LABEL}: the request exceeds the model's context window"),
            ),
        }),
        Some("refusal") => {
            let details = event
                .get("delta")
                .and_then(|d| d.get("stop_details"))
                .or_else(|| event.get("stop_details"));
            let category = details
                .and_then(|d| d.get("category"))
                .and_then(Value::as_str);
            let mut message = "The model declined to continue this request".to_owned();
            if let Some(category) = category {
                message.push_str(&format!(" (category: {category})"));
            }
            Ok(FinishReason::Error {
                failure: LlmFailure::new("REFUSAL", message),
            })
        }
        _ => Err(malformed(format!("unsupported stop reason {raw}"))),
    }
}

/// Merges counters that are present; providers omit or null some of them.
fn update_usage(usage: &mut TokenUsage, raw: Option<&Value>) {
    let Some(fields) = raw.and_then(Value::as_object) else {
        return;
    };
    let count = |name: &str| fields.get(name).and_then(Value::as_u64);
    if let Some(n) = count("input_tokens") {
        usage.input_tokens = n;
    }
    if let Some(n) = count("output_tokens") {
        usage.output_tokens = n;
    }
    if let Some(n) = count("cache_read_input_tokens") {
        usage.cache_read_tokens = Some(n);
    }
    if let Some(n) = count("cache_creation_input_tokens") {
        usage.cache_write_tokens = Some(n);
    }
}
