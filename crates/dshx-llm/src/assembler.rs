//! Chunk → assistant message assembly (dsh `llm/src/assembler.ts`).
//!
//! The agent feeds every chunk of one stream and builds the assistant
//! message from it. Content and replay metadata derive from one keep/drop
//! decision, so the persisted `replayState` always lines up with the
//! content blocks (dsh drops the envelope otherwise).
use std::collections::HashMap;

use dshx_core::{
    ContentBlock, FinishReason, Message, MessageSource, Role, StreamChunk, TokenUsage,
};
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct Partial {
    block_type: String,
    text: String,
    tool_call_id: Option<String>,
    tool_call_name: Option<String>,
    tool_call_arguments: String,
    /// Set by `BlockEnd`; authoritative and final.
    block: Option<ContentBlock>,
}

/// Incremental assembler for one model stream.
#[derive(Debug, Default)]
pub struct BlockAssembler {
    partials: HashMap<usize, Partial>,
    order: Vec<usize>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay_state: Option<Value>,
}

impl BlockAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                self.ensure(*index, block_type);
            }
            StreamChunk::TextDelta { index, text }
            | StreamChunk::ReasoningDelta { index, text } => {
                let kind = if matches!(chunk, StreamChunk::TextDelta { .. }) {
                    "text"
                } else {
                    "reasoning"
                };
                let partial = self.ensure(*index, kind);
                if partial.block.is_none() {
                    partial.text.push_str(text);
                }
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let partial = self.ensure(*index, "tool-call");
                if partial.block.is_none() {
                    partial.tool_call_id = Some(id.clone());
                    if let Some(name) = name.as_ref().filter(|n| !n.is_empty()) {
                        partial.tool_call_name = Some(name.clone());
                    }
                    partial.tool_call_arguments.push_str(arguments_delta);
                }
            }
            StreamChunk::BlockEnd { index, block } => {
                let kind = crate::replay::block_type(block).to_string();
                let partial = self.ensure(*index, &kind);
                if partial.block.is_none() {
                    partial.block = Some(block.clone());
                }
            }
            StreamChunk::Usage(usage) => self.usage = Some(usage.clone()),
            StreamChunk::Finish {
                reason,
                replay_state,
            } => {
                self.finish = Some(reason.clone());
                self.replay_state = replay_state.clone();
            }
        }
    }

    fn ensure(&mut self, index: usize, block_type: &str) -> &mut Partial {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(
                index,
                Partial {
                    block_type: block_type.to_string(),
                    ..Default::default()
                },
            );
        }
        self.partials.get_mut(&index).expect("partial exists")
    }

    fn assemble(partial: &Partial, index: usize) -> Option<ContentBlock> {
        if let Some(block) = &partial.block {
            return Some(block.clone());
        }
        match partial.block_type.as_str() {
            "text" => Some(ContentBlock::Text {
                text: partial.text.clone(),
            }),
            "reasoning" => Some(ContentBlock::Reasoning {
                text: partial.text.clone(),
            }),
            "tool-call" => Some(ContentBlock::ToolCall {
                id: partial
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| format!("call-{index}")),
                name: partial.tool_call_name.clone().unwrap_or_default(),
                arguments: partial.tool_call_arguments.clone(),
            }),
            _ => None,
        }
    }

    /// Max-token truncation drops tool calls (they cannot run safely); the
    /// replay entries are pruned in step.
    fn assembled(&self) -> (Vec<ContentBlock>, Option<Value>) {
        let all: Vec<ContentBlock> = self
            .order
            .iter()
            .filter_map(|i| Self::assemble(&self.partials[i], *i))
            .collect();
        let truncated = self.finish() == FinishReason::MaxTokens;
        let kept: Vec<bool> = all
            .iter()
            .map(|b| !(truncated && matches!(b, ContentBlock::ToolCall { .. })))
            .collect();
        let blocks: Vec<ContentBlock> = all
            .iter()
            .zip(&kept)
            .filter(|(_, k)| **k)
            .map(|(b, _)| b.clone())
            .collect();
        let Some(envelope) = &self.replay_state else {
            return (blocks, None);
        };
        let Some(entries) = envelope.get("blocks").and_then(Value::as_array) else {
            return (blocks, Some(envelope.clone()));
        };
        if entries.len() != all.len() {
            return (blocks, None);
        }
        if blocks.len() == all.len() {
            return (blocks, Some(envelope.clone()));
        }
        let pruned: Vec<Value> = entries
            .iter()
            .zip(&kept)
            .filter(|(_, k)| **k)
            .map(|(e, _)| e.clone())
            .collect();
        (
            blocks,
            Some(
                json!({"response": envelope.get("response").cloned().unwrap_or(Value::Null), "blocks": pruned}),
            ),
        )
    }

    /// All blocks seen so far, in stream order (see the truncation rule).
    pub fn blocks(&self) -> Vec<ContentBlock> {
        self.assembled().0
    }

    /// Blocks an interrupted stream can keep: text and reasoning with
    /// non-whitespace content. Tool calls are dropped (never dispatched).
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.order
            .iter()
            .filter_map(|i| Self::assemble(&self.partials[i], *i))
            .filter(|b| match b {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                    !text.trim().is_empty()
                }
                _ => false,
            })
            .collect()
    }

    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// The finish reason; `Stop` when the stream ended without one.
    pub fn finish(&self) -> FinishReason {
        self.finish.clone().unwrap_or(FinishReason::Stop)
    }

    /// Replay metadata aligned with [`Self::blocks`], if any.
    pub fn replay_state(&self) -> Option<Value> {
        self.assembled().1
    }

    /// The assembled assistant message with a fresh id.
    pub fn message(&self, provider: &str, model: &str) -> Message {
        let (content, replay_state) = self.assembled();
        Message {
            id: dshx_core::new_id(),
            role: Role::Assistant,
            content,
            source: MessageSource::Model {
                provider: provider.into(),
                model: model.into(),
                replay_state,
            },
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        }
    }
}

/// A chunk in dsh's JSON form (`{"type":"block-start","index":0,"blockType":"text"}`, …),
/// as logged in the `stream` of `assistant/message` events.
pub fn chunk_json(chunk: &StreamChunk) -> Value {
    match chunk {
        StreamChunk::BlockStart { index, block_type } => {
            json!({"type": "block-start", "index": index, "blockType": block_type})
        }
        StreamChunk::TextDelta { index, text } => {
            json!({"type": "text-delta", "index": index, "text": text})
        }
        StreamChunk::ReasoningDelta { index, text } => {
            json!({"type": "reasoning-delta", "index": index, "text": text})
        }
        StreamChunk::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => {
            let mut value = json!({"type": "tool-call-delta", "index": index, "id": id});
            if let Some(name) = name {
                value["name"] = name.clone().into();
            }
            value["argumentsDelta"] = arguments_delta.clone().into();
            value
        }
        StreamChunk::BlockEnd { index, block } => {
            json!({"type": "block-end", "index": index, "block": block})
        }
        StreamChunk::Usage(usage) => json!({"type": "usage", "usage": usage}),
        StreamChunk::Finish {
            reason,
            replay_state,
        } => {
            let mut value = json!({"type": "finish", "reason": reason});
            if let Some(state) = replay_state {
                value["replayState"] = state.clone();
            }
            value
        }
    }
}
