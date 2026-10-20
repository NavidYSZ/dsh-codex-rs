//! Replay round trip against the official dsh: the same SSE that dsh
//! turned into its persisted `assistant/message` must give us the identical
//! message (content, `source.replayState`, usage), and a history made of
//! *our* messages must serialize to the exact bytes dsh sent next.
mod common;

use common::*;
use dshx_core::{LlmClient, Message, Role, StreamChunk};
use dshx_llm::{BlockAssembler, DeepSeekClient, DeepSeekConfig, json};
use dshx_testkit::{MockDeepSeek, scenarios};
use futures::StreamExt;
use serde_json::Value;

/// Streams every recorded step of `name` through our client against the
/// mock replaying the capture script; returns our assistant messages.
async fn our_messages(name: &str, scenario: &str) -> Vec<(Message, Value)> {
    let (steps, _) = recorded_requests(name);
    let mock = MockDeepSeek::start().await;
    let client = fixture_client(DeepSeekConfig {
        base_url: mock.base_url(),
        ..Default::default()
    });
    let mut out = vec![];
    for (index, step) in steps.iter().enumerate() {
        let dsh = step.assistant.as_ref().unwrap();
        let id = dsh["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == "tool-call")
            .map(|b| b["id"].as_str().unwrap().to_string())
            .unwrap_or_default();
        let tool_steps = step
            .request
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count();
        mock.push(scenarios::step_reply(scenario, tool_steps, &id));
        let chunks: Vec<StreamChunk> = client
            .stream(step.request.clone(), Default::default())
            .await
            .collect()
            .await;
        let mut assembler = BlockAssembler::new();
        chunks.iter().for_each(|c| assembler.push(c));
        let usage = serde_json::to_value(
            assembler
                .usage()
                .unwrap_or_else(|| panic!("{name} #{index}: {chunks:?}")),
        )
        .unwrap();
        out.push((
            assembler.message(client.provider(), &step.request.model),
            usage,
        ));
    }
    out
}

fn without_id(message: &Value) -> Value {
    let mut message = message.clone();
    message.as_object_mut().unwrap().remove("id");
    message
}

async fn round_trip(name: &str, scenario: &str) {
    let (steps, _) = recorded_requests(name);
    let ours = our_messages(name, scenario).await;
    for (step, (message, usage)) in steps.iter().zip(&ours) {
        let dsh = step.assistant.as_ref().unwrap();
        assert_eq!(
            without_id(&serde_json::to_value(message).unwrap()),
            without_id(dsh),
            "{name} seq {:?}",
            step.assistant_seq
        );
        assert_eq!(
            usage,
            step.usage.as_ref().unwrap(),
            "{name} usage seq {:?}",
            step.assistant_seq
        );
        // The persisted replay JSON is byte-identical to dsh's too.
        let dshx_core::MessageSource::Model {
            replay_state: Some(state),
            ..
        } = &message.source
        else {
            panic!()
        };
        assert_eq!(
            json::to_string(state),
            json::to_string(&dsh["source"]["replayState"])
        );
    }

    // Swap dsh's assistant messages for ours: the next requests must not change by a byte.
    let (bodies, _) = captured_bodies(name);
    let client = DeepSeekClient::builder(DeepSeekConfig::default())
        .build()
        .unwrap();
    for (index, step) in steps.iter().enumerate() {
        let mut request = step.request.clone();
        let mut assistant = 0;
        for message in request
            .messages
            .iter_mut()
            .filter(|m| m.role == Role::Assistant)
        {
            *message = ours[assistant].0.clone();
            assistant += 1;
        }
        assert_eq!(assistant, index, "{name}: history of step {index}");
        let wire = client.build_request(&request).await.unwrap();
        assert_same_bytes(
            &json::to_vec(&wire.body),
            &bodies[index],
            &format!("{name} step {} with our history", index + 1),
        );
    }
}

#[tokio::test]
async fn thinking_session_round_trips_with_dsh() {
    round_trip("thinking", "thinking").await;
}

#[tokio::test]
async fn model_switch_session_round_trips_with_dsh() {
    round_trip("switch", "text").await;
}

#[tokio::test]
async fn image_session_round_trips_with_dsh() {
    // Text-only replies without thinking: replay blocks carry no signatures.
    let (steps, _) = recorded_requests("image");
    let ours = our_messages("image", "image").await;
    for (step, (message, _)) in steps.iter().zip(&ours) {
        assert_eq!(
            without_id(&serde_json::to_value(message).unwrap()),
            without_id(step.assistant.as_ref().unwrap())
        );
    }
}
