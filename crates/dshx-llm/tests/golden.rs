//! Golden tests: for every request the official dsh 0.2.0-rc.2 sent in the
//! captured sessions, rebuilding it from the session log must give the very
//! same body bytes (minus dsh's telemetry fields `dsh_plugin_packages` and
//! `dsh_session_log`, which dsh-codex never sends).
mod common;

use common::*;
use dshx_core::{LlmClient, StreamChunk};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, json};
use dshx_testkit::{MockDeepSeek, Reply};
use futures::StreamExt;
use serde_json::Value;

fn client(config: DeepSeekConfig) -> DeepSeekClient {
    fixture_client(config)
}

async fn check_session(name: &str) {
    let (steps, title) = recorded_requests(name);
    let (bodies, title_body) = captured_bodies(name);
    assert_eq!(steps.len(), bodies.len(), "{name}: agent requests");
    let client = client(DeepSeekConfig::default());
    for (index, (step, expected)) in steps.iter().zip(&bodies).enumerate() {
        let wire = client
            .build_request(&step.request)
            .await
            .unwrap_or_else(|e| panic!("{name} #{index}: {e:?}"));
        assert!(!wire.tool_changes);
        assert_same_bytes(
            &json::to_vec(&wire.body),
            expected,
            &format!("{name} step {}", index + 1),
        );
    }
    let wire = client.build_request(&title.unwrap().request).await.unwrap();
    assert_same_bytes(
        &json::to_vec(&wire.body),
        &title_body,
        &format!("{name} title"),
    );
}

#[tokio::test]
async fn thinking_session_matches_dsh_bytes() {
    // Signed thinking replayed across three steps of one turn.
    check_session("thinking").await;
}

#[tokio::test]
async fn image_session_matches_dsh_bytes() {
    // read_image result: handle text with the attachment path + inline base64.
    check_session("image").await;
}

#[tokio::test]
async fn model_switch_session_matches_dsh_bytes() {
    // flash → v4-pro (effort max, prompt replaced, signatures dropped) →
    // flash (effort low, in-history system update, own signatures kept).
    check_session("switch").await;
}

#[tokio::test]
async fn orchestrator_captures_match_structurally() {
    // docs/rust/upstream/wire-0.2.0-rc.2: no session log was kept, so rebuild
    // the history from the wire messages and compare the full body.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/rust/upstream/wire-0.2.0-rc.2");
    let client = client(DeepSeekConfig::default());
    for n in 1..=4 {
        let capture: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{n:03}.json"))).unwrap())
                .unwrap();
        let mut expected = capture["body"].clone();
        let object = expected.as_object_mut().unwrap();
        object.remove("dsh_plugin_packages");
        object.remove("dsh_session_log");
        let request = request_from_wire(&expected);
        let wire = client.build_request(&request).await.unwrap();
        assert_eq!(wire.body, expected, "capture {n:03}");
        assert_eq!(capture["path"], "/anthropic/v1/messages");
    }
}

/// Canonical history that serializes to `body` (wire → canonical).
fn request_from_wire(body: &Value) -> dshx_core::GenerateRequest {
    use dshx_core::{ContentBlock, Message};
    let mut messages = vec![Message::system(body["system"].as_str().unwrap())];
    for wire in body["messages"].as_array().unwrap() {
        let blocks = wire["content"].as_array().unwrap();
        match wire["role"].as_str().unwrap() {
            "assistant" => {
                let content = blocks
                    .iter()
                    .map(|b| match b["type"].as_str().unwrap() {
                        "text" => ContentBlock::text(b["text"].as_str().unwrap()),
                        "tool_use" => ContentBlock::ToolCall {
                            id: b["id"].as_str().unwrap().into(),
                            name: b["name"].as_str().unwrap().into(),
                            arguments: b["input"].to_string(),
                        },
                        other => panic!("{other}"),
                    })
                    .collect();
                messages.push(Message {
                    content,
                    source: dshx_core::MessageSource::Model {
                        provider: "deepseek-official".into(),
                        model: "deepseek-flash".into(),
                        replay_state: None,
                    },
                    role: dshx_core::Role::Assistant,
                    ..Message::user_text("")
                });
            }
            _ => {
                for block in blocks {
                    match block["type"].as_str().unwrap() {
                        "text" => {
                            messages.push(Message::user_text(block["text"].as_str().unwrap()))
                        }
                        "tool_result" => messages.push(Message::tool_result(
                            block["tool_use_id"].as_str().unwrap(),
                            vec![ContentBlock::text(
                                block["content"][0]["text"].as_str().unwrap(),
                            )],
                            block["is_error"].as_bool().unwrap(),
                        )),
                        other => panic!("{other}"),
                    }
                }
            }
        }
    }
    let tools = body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| dshx_core::ToolSchema {
                    name: t["name"].as_str().unwrap().into(),
                    description: t["description"].as_str().unwrap().into(),
                    parameters: t["input_schema"].clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    dshx_core::GenerateRequest {
        model: body["model"].as_str().unwrap().into(),
        reasoning_effort: Some(
            body["output_config"]["effort"]
                .as_str()
                .unwrap_or("off")
                .into(),
        ),
        messages,
        tools,
        max_tokens: body["max_tokens"].as_u64().map(|v| v as u32),
        temperature: None,
        session_id: None,
    }
}

#[tokio::test]
async fn sent_bytes_and_headers_match_dsh_over_http() {
    let (steps, _) = recorded_requests("thinking");
    let (bodies, _) = captured_bodies("thinking");
    let mock = MockDeepSeek::start().await;
    mock.push(Reply::text("ok"));
    let client = client(DeepSeekConfig {
        base_url: mock.base_url(),
        ..Default::default()
    });
    let chunks: Vec<StreamChunk> = client
        .stream(steps[0].request.clone(), Default::default())
        .await
        .collect()
        .await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: dshx_core::FinishReason::Stop,
            ..
        })
    ));
    let request = &mock.requests()[0];
    assert_same_bytes(&request.raw_body, &bodies[0], "HTTP body");

    let captured: Value =
        serde_json::from_slice(&read_fixture("thinking", "001.headers.json")).unwrap();
    assert_eq!(request.path, captured["path"]);
    for name in [
        "content-type",
        "accept",
        "anthropic-version",
        "x-deepseek-harness-session-id",
    ] {
        assert_eq!(
            request.header(name),
            captured["headers"][name].as_str(),
            "{name}"
        );
    }
    assert_eq!(request.header("x-api-key"), Some("sk-test"));
    assert_eq!(
        request.header("user-agent"),
        Some(concat!("dsh-codex/", env!("CARGO_PKG_VERSION")))
    );
    // Nothing dsh-codex must not send.
    assert_eq!(request.header("x-deepseek-harness-user-id"), None);
    assert_eq!(request.header("anthropic-beta"), None);
    assert!(
        request.body.get("dsh_session_log").is_none()
            && request.body.get("dsh_plugin_packages").is_none()
    );
}
