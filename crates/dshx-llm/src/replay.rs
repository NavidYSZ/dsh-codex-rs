//! Native thinking replay metadata (dsh `llm-deepseek/src/replay.ts`).
//!
//! The assistant message keeps all text; `source.replayState` only adds the
//! per-block thinking signatures:
//! `{"response":{"kind":"deepseek-messages","version":1,"model":M},"blocks":[{"type":"reasoning","signature":"…"},{"type":"text"},…]}`.
//! The JSON is byte-identical to dsh's, so either harness can continue the other's sessions.
use dshx_core::{ContentBlock, LlmFailure, Message, MessageSource};
use serde_json::{Map, Value, json};

pub const REPLAY_KIND: &str = "deepseek-messages";
pub const REPLAY_VERSION: u64 = 1;

/// Index-aligned metadata of one emitted block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBlock {
    /// `text`, `reasoning` or `tool-call`.
    pub kind: &'static str,
    pub signature: Option<String>,
}

impl ReplayBlock {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("type".into(), Value::String(self.kind.into()));
        if let Some(signature) = &self.signature {
            map.insert("signature".into(), Value::String(signature.clone()));
        }
        Value::Object(map)
    }
}

/// The envelope persisted in `source.replayState`.
pub fn replay_state(model: &str, blocks: &[ReplayBlock]) -> Value {
    json!({
        "response": {"kind": REPLAY_KIND, "version": REPLAY_VERSION, "model": model},
        "blocks": blocks.iter().map(ReplayBlock::to_json).collect::<Vec<_>>(),
    })
}

/// dsh `ContentBlock.type` of a canonical block.
pub fn block_type(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text { .. } => "text",
        ContentBlock::Reasoning { .. } => "reasoning",
        ContentBlock::Image { .. } => "image",
        ContentBlock::ToolCall { .. } => "tool-call",
        ContentBlock::ToolAddition { .. } => "tool-addition",
        ContentBlock::ToolRemoval { .. } => "tool-removal",
        ContentBlock::Unknown(value) => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    }
}

/// Validated replay metadata for `message` when sent to `model`.
///
/// `None` for foreign or cross-model history (signatures are not portable)
/// and for unusable metadata; the latter calls `on_degrade` with a reason
/// that never contains message content or signatures.
pub fn read_replay(
    message: &Message,
    model: &str,
    on_degrade: &mut dyn FnMut(&str),
) -> Option<Vec<Option<String>>> {
    match validate(message, model) {
        Ok(blocks) => blocks,
        Err(failure) => {
            on_degrade(&failure.message);
            None
        }
    }
}

fn validate(message: &Message, model: &str) -> Result<Option<Vec<Option<String>>>, LlmFailure> {
    let MessageSource::Model {
        model: source_model,
        replay_state: Some(state),
        ..
    } = &message.source
    else {
        return Ok(None);
    };
    let fail = |detail: &str| {
        LlmFailure::new(
            "INVALID_REPLAY_STATE",
            format!("DeepSeek Messages replay: {detail}"),
        )
    };
    let not_object = || {
        LlmFailure::new(
            "INVALID_REPLAY_STATE",
            "DeepSeek Messages expected a JSON object",
        )
    };
    let envelope = state.as_object().ok_or_else(not_object)?;
    let response = envelope
        .get("response")
        .and_then(Value::as_object)
        .ok_or_else(not_object)?;
    let version_ok = response.get("version").and_then(Value::as_f64) == Some(REPLAY_VERSION as f64);
    if response.get("kind").and_then(Value::as_str) != Some(REPLAY_KIND) || !version_ok {
        return Err(fail("unsupported kind or version"));
    }
    if response.get("model").and_then(Value::as_str) != Some(source_model.as_str()) {
        return Err(fail("model does not match assistant source model"));
    }
    let blocks = match envelope.get("blocks").and_then(Value::as_array) {
        Some(blocks) if blocks.len() == message.content.len() => blocks,
        _ => return Err(fail("block count mismatch")),
    };
    let mut out = Vec::with_capacity(blocks.len());
    for (index, value) in blocks.iter().enumerate() {
        let block = value.as_object().ok_or_else(not_object)?;
        let kind = block.get("type").and_then(Value::as_str);
        if kind != Some(block_type(&message.content[index]))
            || !matches!(kind, Some("text" | "reasoning" | "tool-call"))
        {
            return Err(fail("block type mismatch"));
        }
        let signature = match block.get("signature") {
            None => None,
            Some(Value::String(signature)) if kind == Some("reasoning") => Some(signature.clone()),
            Some(_) => return Err(fail("invalid signature")),
        };
        out.push(signature);
    }
    Ok((source_model == model).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_core::Role;

    fn assistant(content: Vec<ContentBlock>, state: Option<Value>) -> Message {
        Message {
            id: "m".into(),
            role: Role::Assistant,
            content,
            source: MessageSource::Model {
                provider: "deepseek-official".into(),
                model: "deepseek-flash".into(),
                replay_state: state,
            },
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn envelope_json_matches_dsh() {
        let state = replay_state(
            "deepseek-flash",
            &[
                ReplayBlock {
                    kind: "reasoning",
                    signature: Some("sig0".into()),
                },
                ReplayBlock {
                    kind: "text",
                    signature: None,
                },
            ],
        );
        assert_eq!(
            crate::json::to_string(&state),
            r#"{"response":{"kind":"deepseek-messages","version":1,"model":"deepseek-flash"},"blocks":[{"type":"reasoning","signature":"sig0"},{"type":"text"}]}"#
        );
    }

    #[test]
    fn degrades_unusable_state_with_one_diagnostic() {
        let content = vec![ContentBlock::Reasoning {
            text: "think".into(),
        }];
        let cases = [
            json!(null),
            json!([]),
            json!({"response": null, "blocks": []}),
            json!({"response": {"kind": "other", "version": 1}, "blocks": []}),
            json!({"response": {"kind": "deepseek-messages", "version": 2}, "blocks": []}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "wrong"}, "blocks": []}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"}, "blocks": []}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"}, "blocks": null}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"}, "blocks": [null]}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"}, "blocks": [{"type": "tool-call"}]}),
            json!({"response": {"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"}, "blocks": [{"type": "reasoning", "signature": 3}]}),
        ];
        for state in cases {
            let mut reasons = vec![];
            let message = assistant(content.clone(), Some(state.clone()));
            assert_eq!(
                read_replay(&message, "deepseek-flash", &mut |r| reasons
                    .push(r.to_string())),
                None,
                "{state}"
            );
            assert_eq!(reasons.len(), 1, "{state}");
        }
    }

    #[test]
    fn keeps_own_signatures_and_drops_cross_model_quietly() {
        let state = replay_state(
            "deepseek-flash",
            &[ReplayBlock {
                kind: "reasoning",
                signature: Some(String::new()),
            }],
        );
        let message = assistant(
            vec![ContentBlock::Reasoning {
                text: "think".into(),
            }],
            Some(state),
        );
        let mut calls = 0;
        assert_eq!(
            read_replay(&message, "deepseek-flash", &mut |_| calls += 1),
            Some(vec![Some(String::new())])
        );
        assert_eq!(
            read_replay(&message, "deepseek-v4-pro", &mut |_| calls += 1),
            None
        );
        assert_eq!(calls, 0);
    }
}
