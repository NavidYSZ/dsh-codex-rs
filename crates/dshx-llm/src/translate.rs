//! Messages events → harness stream chunks (dsh `llm-deepseek/src/translate.ts`).
//!
//! Blocks are renumbered densely in first-seen order, deltas stream as they
//! arrive, usage is cumulative and disjoint (`input_tokens` excludes cache
//! reads/writes), and exactly one `Usage` + `Finish` ends a good stream.
use std::collections::HashMap;

use dshx_core::{ContentBlock, FinishReason, LlmFailure, StreamChunk, TokenUsage};
use serde_json::{Map, Value};

use crate::replay::{ReplayBlock, replay_state};

fn malformed(detail: impl std::fmt::Display) -> LlmFailure {
    LlmFailure::new(
        "MALFORMED_RESPONSE",
        format!("DeepSeek Messages stream: {detail}"),
    )
}

fn not_object() -> LlmFailure {
    LlmFailure::new(
        "MALFORMED_RESPONSE",
        "DeepSeek Messages expected a JSON object",
    )
}

fn object(value: Option<&Value>) -> Result<&Map<String, Value>, LlmFailure> {
    value.and_then(Value::as_object).ok_or_else(not_object)
}

fn string(value: Option<&Value>) -> Result<String, LlmFailure> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            LlmFailure::new(
                "MALFORMED_RESPONSE",
                "DeepSeek Messages expected a string field",
            )
        })
}

#[derive(Debug)]
enum Content {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Debug)]
struct Block {
    index: usize,
    content: Content,
    replay: ReplayBlock,
    closed: bool,
    json: String,
}

impl Block {
    fn assembled(&self) -> ContentBlock {
        match &self.content {
            Content::Text(text) => ContentBlock::Text { text: text.clone() },
            Content::Reasoning(text) => ContentBlock::Reasoning { text: text.clone() },
            Content::ToolCall {
                id,
                name,
                arguments,
            } => ContentBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            },
        }
    }
}

/// Stateful translator for one response. Feed decoded events in order;
/// after `message_stop` it reports [`Translator::is_done`].
#[derive(Debug)]
pub struct Translator {
    model: String,
    blocks: HashMap<u64, Block>,
    order: Vec<u64>,
    usage: TokenUsage,
    started: bool,
    reason: Option<FinishReason>,
    done: bool,
}

const CONTENT_EVENTS: [&str; 5] = [
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
];

impl Translator {
    /// `model` is the requested model id, stored in the replay state.
    pub fn new(model: impl Into<String>) -> Self {
        Translator {
            model: model.into(),
            blocks: HashMap::new(),
            order: vec![],
            usage: TokenUsage::default(),
            started: false,
            reason: None,
            done: false,
        }
    }

    /// `message_stop` was translated; ignore anything after it.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Translates one event. Errors are terminal for the stream.
    pub fn push(&mut self, event: &Map<String, Value>) -> Result<Vec<StreamChunk>, LlmFailure> {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if self.done {
            return Ok(vec![]);
        }
        if kind == "message_start" {
            if self.started {
                return Err(malformed("duplicate message_start"));
            }
            let usage = object(event.get("message"))?.get("usage");
            update_usage(&mut self.usage, usage)?;
            self.started = true;
            return Ok(vec![]);
        }
        // Anthropic permits additional event types (ping, future ones).
        if !CONTENT_EVENTS.contains(&kind) {
            return Ok(vec![]);
        }
        if !self.started {
            return Err(malformed("event precedes message_start"));
        }
        let mut out = vec![];
        match kind {
            "content_block_start" => {
                let wire = index_of(event)?;
                if self.blocks.contains_key(&wire) || self.reason.is_some() {
                    return Err(malformed(
                        "block starts after settlement or repeats an index",
                    ));
                }
                let block = start_block(event, self.blocks.len())?;
                match &block.content {
                    Content::Text(text) => {
                        out.push(StreamChunk::BlockStart {
                            index: block.index,
                            block_type: "text",
                        });
                        if !text.is_empty() {
                            out.push(StreamChunk::TextDelta {
                                index: block.index,
                                text: text.clone(),
                            });
                        }
                    }
                    Content::Reasoning(text) => {
                        out.push(StreamChunk::BlockStart {
                            index: block.index,
                            block_type: "reasoning",
                        });
                        if !text.is_empty() {
                            out.push(StreamChunk::ReasoningDelta {
                                index: block.index,
                                text: text.clone(),
                            });
                        }
                    }
                    Content::ToolCall { id, name, .. } => {
                        out.push(StreamChunk::BlockStart {
                            index: block.index,
                            block_type: "tool-call",
                        });
                        out.push(StreamChunk::ToolCallDelta {
                            index: block.index,
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
                let Some(block) = self.blocks.get_mut(&wire).filter(|b| !b.closed) else {
                    return Err(malformed("delta/stop without an open block"));
                };
                if kind == "content_block_delta" {
                    if let Some(chunk) = delta_chunk(block, event.get("delta"))? {
                        out.push(chunk);
                    }
                } else {
                    block.closed = true;
                    if let Content::ToolCall { arguments, .. } = &mut block.content
                        && !block.json.is_empty()
                    {
                        *arguments = block.json.clone();
                    }
                    out.push(StreamChunk::BlockEnd {
                        index: block.index,
                        block: block.assembled(),
                    });
                }
            }
            "message_delta" => {
                let delta = object(event.get("delta"))?;
                match delta.get("stop_reason") {
                    None | Some(Value::Null) => {}
                    Some(reason) => self.reason = Some(stop_reason(reason)?),
                }
                if let Some(usage) = event.get("usage") {
                    update_usage(&mut self.usage, Some(usage))?;
                }
            }
            _ => {
                // message_stop
                let Some(reason) = self.reason.clone() else {
                    return Err(malformed(
                        "message_stop without settled blocks and stop reason",
                    ));
                };
                if self.blocks.values().any(|b| !b.closed) {
                    return Err(malformed(
                        "message_stop without settled blocks and stop reason",
                    ));
                }
                if self.blocks.is_empty() && reason == FinishReason::Stop {
                    return Err(LlmFailure::new(
                        "EMPTY_RESPONSE",
                        "DeepSeek Messages returned no content",
                    ));
                }
                // Truncated tool JSON is kept here and pruned by the assembler.
                if reason != FinishReason::MaxTokens {
                    for block in self.blocks.values() {
                        if let Content::ToolCall { arguments, .. } = &block.content {
                            match serde_json::from_str::<Value>(arguments) {
                                Err(_) => return Err(malformed("tool input is invalid JSON")),
                                Ok(Value::Object(_)) => {}
                                Ok(_) => return Err(not_object()),
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
                let replay: Vec<ReplayBlock> = self
                    .order
                    .iter()
                    .map(|w| self.blocks[w].replay.clone())
                    .collect();
                out.push(StreamChunk::Usage(usage));
                out.push(StreamChunk::Finish {
                    reason,
                    replay_state: Some(replay_state(&self.model, &replay)),
                });
                self.done = true;
            }
        }
        Ok(out)
    }

    /// The response body ended; a stream without `message_stop` failed.
    pub fn finish(&self) -> Result<(), LlmFailure> {
        if self.done {
            Ok(())
        } else {
            Err(LlmFailure::new(
                "STREAM_CLOSED",
                "DeepSeek Messages stream ended before message_stop",
            ))
        }
    }
}

fn index_of(event: &Map<String, Value>) -> Result<u64, LlmFailure> {
    // Safe non-negative integers only (JS Number.isSafeInteger).
    match event.get("index") {
        Some(Value::Number(n)) => match (n.as_u64(), n.as_f64()) {
            (Some(i), _) if i < (1 << 53) => Ok(i),
            (None, Some(f)) if f >= 0.0 && f.fract() == 0.0 && f <= ((1u64 << 53) - 1) as f64 => {
                Ok(f as u64)
            }
            _ => Err(malformed("invalid block index")),
        },
        _ => Err(malformed("invalid block index")),
    }
}

fn start_block(event: &Map<String, Value>, index: usize) -> Result<Block, LlmFailure> {
    let native = object(event.get("content_block"))?;
    let (content, replay) = match native.get("type").and_then(Value::as_str) {
        Some("text") => (
            Content::Text(string(native.get("text"))?),
            ReplayBlock {
                kind: "text",
                signature: None,
            },
        ),
        Some("thinking") => {
            let text = string(native.get("thinking"))?;
            let signature = match native.get("signature") {
                None => None,
                Some(value) => Some(string(Some(value))?),
            };
            (
                Content::Reasoning(text),
                ReplayBlock {
                    kind: "reasoning",
                    signature,
                },
            )
        }
        Some("tool_use") => {
            let id = string(native.get("id"))?;
            let name = string(native.get("name"))?;
            let input = object(native.get("input"))?;
            if id.is_empty() || name.is_empty() {
                return Err(malformed("empty tool identity"));
            }
            let arguments = crate::json::to_string(&Value::Object(input.clone()));
            (
                Content::ToolCall {
                    id,
                    name,
                    arguments,
                },
                ReplayBlock {
                    kind: "tool-call",
                    signature: None,
                },
            )
        }
        other => {
            let shown = match other {
                Some(kind) => kind.to_string(),
                None => native
                    .get("type")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "undefined".into()),
            };
            return Err(LlmFailure::new(
                "UNSUPPORTED_CONTENT",
                format!("DeepSeek Messages does not support response block {shown}"),
            ));
        }
    };
    Ok(Block {
        index,
        content,
        replay,
        closed: false,
        json: String::new(),
    })
}

fn delta_chunk(block: &mut Block, raw: Option<&Value>) -> Result<Option<StreamChunk>, LlmFailure> {
    let delta = object(raw)?;
    let kind = delta.get("type").and_then(Value::as_str).unwrap_or("");
    match (kind, &mut block.content) {
        ("text_delta", Content::Text(text)) => {
            let piece = string(delta.get("text"))?;
            text.push_str(&piece);
            Ok(Some(StreamChunk::TextDelta {
                index: block.index,
                text: piece,
            }))
        }
        ("thinking_delta", Content::Reasoning(text)) => {
            let piece = string(delta.get("thinking"))?;
            text.push_str(&piece);
            Ok(Some(StreamChunk::ReasoningDelta {
                index: block.index,
                text: piece,
            }))
        }
        ("signature_delta", Content::Reasoning(_)) => {
            let piece = string(delta.get("signature"))?;
            block
                .replay
                .signature
                .get_or_insert_with(String::new)
                .push_str(&piece);
            Ok(None)
        }
        ("input_json_delta", Content::ToolCall { id, .. }) => {
            let piece = string(delta.get("partial_json"))?;
            block.json.push_str(&piece);
            Ok(Some(StreamChunk::ToolCallDelta {
                index: block.index,
                id: id.clone(),
                name: None,
                arguments_delta: piece,
            }))
        }
        (_, content) => {
            let shown = delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("undefined");
            let target = match content {
                Content::Text(_) => "text",
                Content::Reasoning(_) => "reasoning",
                Content::ToolCall { .. } => "tool-call",
            };
            Err(malformed(format!("unsupported delta {shown} for {target}")))
        }
    }
}

fn stop_reason(raw: &Value) -> Result<FinishReason, LlmFailure> {
    match raw.as_str() {
        Some("end_turn" | "stop_sequence") => Ok(FinishReason::Stop),
        Some("tool_use") => Ok(FinishReason::ToolCalls),
        Some("max_tokens") => Ok(FinishReason::MaxTokens),
        _ => {
            let shown = raw
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| raw.to_string());
            Err(malformed(format!("unsupported stop reason {shown}")))
        }
    }
}

fn update_usage(usage: &mut TokenUsage, raw: Option<&Value>) -> Result<(), LlmFailure> {
    let fields = object(raw)?;
    for wire in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let Some(value) = fields.get(wire) else {
            continue;
        };
        let count = value.as_u64().or_else(|| {
            value
                .as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0)
                .map(|f| f as u64)
        });
        let Some(count) = count.filter(|v| *v < (1 << 53)) else {
            return Err(malformed(format!("invalid {wire}")));
        };
        match wire {
            "input_tokens" => usage.input_tokens = count,
            "output_tokens" => usage.output_tokens = count,
            "cache_read_input_tokens" => usage.cache_read_tokens = Some(count),
            _ => usage.cache_write_tokens = Some(count),
        }
    }
    Ok(())
}
