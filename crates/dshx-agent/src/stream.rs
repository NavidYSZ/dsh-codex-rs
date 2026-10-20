//! Assistant stream bookkeeping: the compact durable stream records embedded
//! in `assistant/message` / `assistant/attempt` (dsh `AssistantStreamAccumulator`)
//! and the canonical chunk-to-block assembly (dsh `BlockAssembler`).
use std::collections::HashMap;

use dshx_core::{ContentBlock, FinishReason, StreamChunk, TokenUsage};
use serde_json::{Map, Value, json};

/// dsh JSON form of one stream chunk.
pub(crate) fn chunk_json(chunk: &StreamChunk) -> Value {
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
            let mut m = Map::new();
            m.insert("type".into(), "tool-call-delta".into());
            m.insert("index".into(), json!(index));
            m.insert("id".into(), json!(id));
            if let Some(n) = name {
                m.insert("name".into(), json!(n));
            }
            m.insert("argumentsDelta".into(), json!(arguments_delta));
            Value::Object(m)
        }
        StreamChunk::BlockEnd { index, block } => {
            json!({"type": "block-end", "index": index, "block": block})
        }
        StreamChunk::Usage(usage) => json!({"type": "usage", "usage": usage}),
        StreamChunk::Finish {
            reason,
            replay_state,
        } => {
            let mut m = Map::new();
            m.insert("type".into(), "finish".into());
            m.insert(
                "reason".into(),
                serde_json::to_value(reason).unwrap_or(Value::Null),
            );
            if let Some(r) = replay_state {
                m.insert("replayState".into(), r.clone());
            }
            Value::Object(m)
        }
    }
}

enum Record {
    Text {
        reasoning: bool,
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        texts: Vec<String>,
        last: u64,
    },
    Tool {
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        id: String,
        name: Option<String>,
        args: Vec<String>,
        last: u64,
    },
    Chunk {
        time: u64,
        chunk: Value,
    },
}

/// Packs consecutive deltas of one block into runs without losing boundaries.
#[derive(Default)]
pub(crate) struct StreamAccumulator {
    records: Vec<Record>,
}

impl StreamAccumulator {
    pub fn push(&mut self, time: u64, chunk: &StreamChunk) {
        let gap = |last: u64| time as i64 - last as i64;
        match chunk {
            StreamChunk::TextDelta { index, text }
            | StreamChunk::ReasoningDelta { index, text } => {
                let reasoning = matches!(chunk, StreamChunk::ReasoningDelta { .. });
                if let Some(Record::Text {
                    reasoning: r,
                    index: i,
                    dt,
                    texts,
                    last,
                    ..
                }) = self.records.last_mut()
                    && *r == reasoning
                    && *i == *index
                {
                    dt.push(gap(*last));
                    texts.push(text.clone());
                    *last = time;
                    return;
                }
                self.records.push(Record::Text {
                    reasoning,
                    time0: time,
                    index: *index,
                    dt: Vec::new(),
                    texts: vec![text.clone()],
                    last: time,
                });
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                if id.is_empty() || name.as_deref() == Some("") {
                    self.records.push(Record::Chunk {
                        time,
                        chunk: chunk_json(chunk),
                    });
                    return;
                }
                if let Some(Record::Tool {
                    index: i,
                    id: pid,
                    name: pname,
                    dt,
                    args,
                    last,
                    ..
                }) = self.records.last_mut()
                    && *i == *index
                    && pid == id
                    && pname == name
                {
                    dt.push(gap(*last));
                    args.push(arguments_delta.clone());
                    *last = time;
                    return;
                }
                self.records.push(Record::Tool {
                    time0: time,
                    index: *index,
                    dt: Vec::new(),
                    id: id.clone(),
                    name: name.clone(),
                    args: vec![arguments_delta.clone()],
                    last: time,
                });
            }
            _ => self.records.push(Record::Chunk {
                time,
                chunk: chunk_json(chunk),
            }),
        }
    }

    /// The compact records as stored in the durable event.
    pub fn snapshot(&self) -> Value {
        Value::Array(
            self.records
                .iter()
                .map(|r| match r {
                    Record::Text {
                        reasoning,
                        time0,
                        index,
                        dt,
                        texts,
                        ..
                    } => json!({
                        "type": if *reasoning { "reasoning-chunks" } else { "text-chunks" },
                        "time0": time0, "index": index, "dt": dt, "texts": texts,
                    }),
                    Record::Tool {
                        time0,
                        index,
                        dt,
                        id,
                        name,
                        args,
                        ..
                    } => {
                        let mut m = Map::new();
                        m.insert("type".into(), "tool-call-chunks".into());
                        m.insert("time0".into(), json!(time0));
                        m.insert("index".into(), json!(index));
                        m.insert("dt".into(), json!(dt));
                        m.insert("id".into(), json!(id));
                        if let Some(n) = name {
                            m.insert("name".into(), json!(n));
                        }
                        m.insert("args".into(), json!(args));
                        Value::Object(m)
                    }
                    Record::Chunk { time, chunk } => {
                        json!({"type": "chunk", "time": time, "chunk": chunk})
                    }
                })
                .collect(),
        )
    }
}

#[derive(Default)]
struct Partial {
    block_type: String,
    text: String,
    tool_id: Option<String>,
    tool_name: Option<String>,
    tool_args: String,
    block: Option<ContentBlock>,
}

/// Incremental chunk-to-block assembly.
#[derive(Default)]
pub(crate) struct BlockAssembler {
    partials: HashMap<usize, Partial>,
    order: Vec<usize>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay: Option<Value>,
}

impl BlockAssembler {
    fn ensure(&mut self, index: usize, block_type: &str) -> &mut Partial {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(
                index,
                Partial {
                    block_type: block_type.to_owned(),
                    ..Default::default()
                },
            );
        }
        self.partials.get_mut(&index).unwrap()
    }

    pub fn push(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                self.ensure(*index, block_type);
            }
            StreamChunk::TextDelta { index, text }
            | StreamChunk::ReasoningDelta { index, text } => {
                let ty = if matches!(chunk, StreamChunk::TextDelta { .. }) {
                    "text"
                } else {
                    "reasoning"
                };
                let p = self.ensure(*index, ty);
                if p.block.is_none() {
                    p.text.push_str(text);
                }
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let p = self.ensure(*index, "tool-call");
                if p.block.is_none() {
                    p.tool_id = Some(id.clone());
                    if let Some(n) = name.as_ref().filter(|n| !n.is_empty()) {
                        p.tool_name = Some(n.clone());
                    }
                    p.tool_args.push_str(arguments_delta);
                }
            }
            StreamChunk::BlockEnd { index, block } => {
                let ty = block_type_of(block);
                let p = self.ensure(*index, &ty);
                if p.block.is_none() {
                    p.block = Some(block.clone());
                }
            }
            StreamChunk::Usage(u) => self.usage = Some(u.clone()),
            StreamChunk::Finish {
                reason,
                replay_state,
            } => {
                self.finish = Some(reason.clone());
                self.replay = replay_state.clone();
            }
        }
    }

    fn assemble(p: &Partial, index: usize) -> Option<ContentBlock> {
        if let Some(b) = &p.block {
            return Some(b.clone());
        }
        match p.block_type.as_str() {
            "text" => Some(ContentBlock::Text {
                text: p.text.clone(),
            }),
            "reasoning" => Some(ContentBlock::Reasoning {
                text: p.text.clone(),
            }),
            "tool-call" => Some(ContentBlock::ToolCall {
                id: p.tool_id.clone().unwrap_or_else(|| format!("call-{index}")),
                name: p.tool_name.clone().unwrap_or_default(),
                arguments: p.tool_args.clone(),
            }),
            _ => None,
        }
    }

    fn assembled(&self) -> (Vec<ContentBlock>, Option<Value>) {
        let all: Vec<ContentBlock> = self
            .order
            .iter()
            .filter_map(|i| Self::assemble(&self.partials[i], *i))
            .collect();
        let max_tokens = matches!(self.finish, Some(FinishReason::MaxTokens));
        let kept: Vec<bool> = all
            .iter()
            .map(|b| !(max_tokens && matches!(b, ContentBlock::ToolCall { .. })))
            .collect();
        let blocks: Vec<ContentBlock> = all
            .iter()
            .zip(&kept)
            .filter(|(_, k)| **k)
            .map(|(b, _)| b.clone())
            .collect();
        let replay = match &self.replay {
            None => None,
            Some(env) => match env.get("blocks").and_then(Value::as_array) {
                None => Some(env.clone()),
                Some(meta) if meta.len() != all.len() => None,
                Some(meta) => {
                    if blocks.len() == all.len() {
                        Some(env.clone())
                    } else {
                        let filtered: Vec<Value> = meta
                            .iter()
                            .zip(&kept)
                            .filter(|(_, k)| **k)
                            .map(|(m, _)| m.clone())
                            .collect();
                        let mut out = Map::new();
                        out.insert(
                            "response".into(),
                            env.get("response").cloned().unwrap_or(Value::Null),
                        );
                        out.insert("blocks".into(), Value::Array(filtered));
                        Some(Value::Object(out))
                    }
                }
            },
        };
        (blocks, replay)
    }

    pub fn blocks(&self) -> Vec<ContentBlock> {
        self.assembled().0
    }

    pub fn replay_state(&self) -> Option<Value> {
        self.assembled().1
    }

    /// Text and reasoning with visible content, the prefix an interrupted stream keeps.
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.order
            .iter()
            .filter_map(|i| {
                let p = &self.partials[i];
                let ty = p
                    .block
                    .as_ref()
                    .map(block_type_of)
                    .unwrap_or_else(|| p.block_type.clone());
                if ty != "text" && ty != "reasoning" {
                    return None;
                }
                Self::assemble(p, *i)
            })
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

    /// `stop` when the stream ended without a finish chunk.
    pub fn finish(&self) -> FinishReason {
        self.finish.clone().unwrap_or(FinishReason::Stop)
    }
}

fn block_type_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { .. } => "text".into(),
        ContentBlock::Reasoning { .. } => "reasoning".into(),
        ContentBlock::Image { .. } => "image".into(),
        ContentBlock::ToolCall { .. } => "tool-call".into(),
        ContentBlock::ToolAddition { .. } => "tool-addition".into(),
        ContentBlock::ToolRemoval { .. } => "tool-removal".into(),
        ContentBlock::Unknown(v) => v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_runs_like_dsh() {
        let mut acc = StreamAccumulator::default();
        acc.push(
            10,
            &StreamChunk::BlockStart {
                index: 0,
                block_type: "text",
            },
        );
        acc.push(
            10,
            &StreamChunk::TextDelta {
                index: 0,
                text: "Hel".into(),
            },
        );
        acc.push(
            11,
            &StreamChunk::TextDelta {
                index: 0,
                text: "lo ".into(),
            },
        );
        acc.push(
            11,
            &StreamChunk::TextDelta {
                index: 0,
                text: "world".into(),
            },
        );
        acc.push(
            12,
            &StreamChunk::ToolCallDelta {
                index: 1,
                id: "c1".into(),
                name: Some("bash".into()),
                arguments_delta: String::new(),
            },
        );
        acc.push(
            12,
            &StreamChunk::ToolCallDelta {
                index: 1,
                id: "c1".into(),
                name: None,
                arguments_delta: "{}".into(),
            },
        );
        let s = acc.snapshot();
        assert_eq!(
            s[1],
            json!({"type":"text-chunks","time0":10,"index":0,"dt":[1,0],"texts":["Hel","lo ","world"]})
        );
        assert_eq!(s[2]["name"], "bash");
        assert!(s[3].get("name").is_none());
        assert_eq!(s.as_array().unwrap().len(), 4);
    }

    #[test]
    fn max_tokens_drops_tool_calls_and_prunes_replay() {
        let mut a = BlockAssembler::default();
        a.push(&StreamChunk::TextDelta {
            index: 0,
            text: "hi".into(),
        });
        a.push(&StreamChunk::ToolCallDelta {
            index: 1,
            id: "c".into(),
            name: Some("x".into()),
            arguments_delta: "{".into(),
        });
        a.push(&StreamChunk::Finish {
            reason: FinishReason::MaxTokens,
            replay_state: Some(
                json!({"response": {"k": 1}, "blocks": [{"type": "text"}, {"type": "tool-call"}]}),
            ),
        });
        assert_eq!(a.blocks(), vec![ContentBlock::text("hi")]);
        assert_eq!(
            a.replay_state().unwrap()["blocks"],
            json!([{"type": "text"}])
        );
    }

    #[test]
    fn interrupted_keeps_visible_text_only() {
        let mut a = BlockAssembler::default();
        a.push(&StreamChunk::ReasoningDelta {
            index: 0,
            text: "  ".into(),
        });
        a.push(&StreamChunk::TextDelta {
            index: 1,
            text: "partial".into(),
        });
        a.push(&StreamChunk::ToolCallDelta {
            index: 2,
            id: "c".into(),
            name: Some("x".into()),
            arguments_delta: "{".into(),
        });
        assert_eq!(a.interrupted_blocks(), vec![ContentBlock::text("partial")]);
    }
}
