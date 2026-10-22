//! Thinking replay metadata of the Anthropic Messages route.
//!
//! The assistant message keeps all text; `source.replayState` adds what the
//! API needs back unchanged, per block:
//! `{"response":{"kind":"anthropic-messages","version":1,"provider":P,"model":M},"blocks":[{"type":"reasoning","signature":"…"},{"type":"reasoning","redacted":"…"},{"type":"text"},…]}`.
//! Signatures go back only to the route that produced them.
use dshx_core::{ContentBlock, Message, MessageSource};
use serde_json::{Map, Value, json};

use crate::replay::block_type;

pub const REPLAY_KIND: &str = "anthropic-messages";
pub const REPLAY_VERSION: u64 = 1;

/// Index-aligned metadata of one emitted block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay {
    /// Text and tool calls carry nothing.
    Plain(&'static str),
    /// A `thinking` block and its signature.
    Signed(String),
    /// A `redacted_thinking` block's encrypted `data`.
    Redacted(String),
    /// Thinking that came without a signature; never sent back.
    Unsigned,
}

impl Replay {
    fn to_json(&self) -> Value {
        match self {
            Replay::Plain(kind) => json!({"type": kind}),
            Replay::Signed(signature) => json!({"type": "reasoning", "signature": signature}),
            Replay::Redacted(data) => json!({"type": "reasoning", "redacted": data}),
            Replay::Unsigned => json!({"type": "reasoning"}),
        }
    }
}

/// The envelope persisted in `source.replayState`.
pub fn replay_state(provider: &str, model: &str, blocks: &[Replay]) -> Value {
    json!({
        "response": {"kind": REPLAY_KIND, "version": REPLAY_VERSION, "provider": provider, "model": model},
        "blocks": blocks.iter().map(Replay::to_json).collect::<Vec<_>>(),
    })
}

/// Replay metadata of `message` for a request of `provider`/`model`, index
/// aligned with its content; `None` for foreign, cross-model (unless
/// `across_models`) or unusable metadata.
pub fn read_replay(
    message: &Message,
    provider: &str,
    model: &str,
    across_models: bool,
) -> Option<Vec<Replay>> {
    let MessageSource::Model {
        provider: source_provider,
        model: source_model,
        replay_state: Some(state),
    } = &message.source
    else {
        return None;
    };
    if source_provider != provider || (source_model != model && !across_models) {
        return None;
    }
    let envelope = state.as_object()?;
    let response = envelope.get("response").and_then(Value::as_object)?;
    let field = |name: &str| response.get(name).and_then(Value::as_str);
    if field("kind") != Some(REPLAY_KIND)
        || response.get("version").and_then(Value::as_u64) != Some(REPLAY_VERSION)
        || field("provider") != Some(source_provider.as_str())
        || field("model") != Some(source_model.as_str())
    {
        tracing::warn!(
            provider,
            model,
            "discarding unusable Anthropic replay metadata"
        );
        return None;
    }
    let blocks = envelope.get("blocks").and_then(Value::as_array)?;
    if blocks.len() != message.content.len() {
        tracing::warn!(
            provider,
            model,
            "Anthropic replay metadata does not match the blocks"
        );
        return None;
    }
    let mut out = Vec::with_capacity(blocks.len());
    for (block, content) in blocks.iter().zip(&message.content) {
        let entry: &Map<String, Value> = block.as_object()?;
        let kind = entry.get("type").and_then(Value::as_str)?;
        if kind != block_type(content) {
            return None;
        }
        out.push(
            match (content, entry.get("signature"), entry.get("redacted")) {
                (ContentBlock::Reasoning { .. }, Some(Value::String(s)), None) => {
                    Replay::Signed(s.clone())
                }
                (ContentBlock::Reasoning { .. }, None, Some(Value::String(d))) => {
                    Replay::Redacted(d.clone())
                }
                (ContentBlock::Reasoning { .. }, None, None) => Replay::Unsigned,
                (ContentBlock::Text { .. }, None, None) => Replay::Plain("text"),
                (ContentBlock::ToolCall { .. }, None, None) => Replay::Plain("tool-call"),
                _ => return None,
            },
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_core::Role;

    fn assistant(provider: &str, model: &str, state: Value) -> Message {
        Message {
            id: "m".into(),
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning { text: "a".into() },
                ContentBlock::Reasoning {
                    text: String::new(),
                },
                ContentBlock::Text { text: "hi".into() },
            ],
            source: MessageSource::Model {
                provider: provider.into(),
                model: model.into(),
                replay_state: Some(state),
            },
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn signatures_stay_on_their_route() {
        let blocks = [
            Replay::Signed("sig".into()),
            Replay::Redacted("enc".into()),
            Replay::Plain("text"),
        ];
        let state = replay_state("anthropic", "claude-opus-5-5", &blocks);
        assert_eq!(
            crate::json::to_string(&state),
            r#"{"response":{"kind":"anthropic-messages","version":1,"provider":"anthropic","model":"claude-opus-5-5"},"blocks":[{"type":"reasoning","signature":"sig"},{"type":"reasoning","redacted":"enc"},{"type":"text"}]}"#
        );
        let m = assistant("anthropic", "claude-opus-5-5", state.clone());
        assert_eq!(
            read_replay(&m, "anthropic", "claude-opus-5-5", false).unwrap(),
            blocks
        );
        assert_eq!(
            read_replay(&m, "anthropic", "claude-sonnet-5-5", false),
            None
        );
        assert!(read_replay(&m, "anthropic", "claude-sonnet-5-5", true).is_some());
        assert_eq!(read_replay(&m, "relay", "claude-opus-5-5", true), None);
        // A forged envelope of another route is ignored.
        let other = assistant(
            "anthropic",
            "claude-opus-5-5",
            replay_state("relay", "claude-opus-5-5", &blocks),
        );
        assert_eq!(
            read_replay(&other, "anthropic", "claude-opus-5-5", false),
            None
        );
        let short = assistant(
            "anthropic",
            "claude-opus-5-5",
            replay_state("anthropic", "claude-opus-5-5", &blocks[..1]),
        );
        assert_eq!(
            read_replay(&short, "anthropic", "claude-opus-5-5", false),
            None
        );
    }
}
