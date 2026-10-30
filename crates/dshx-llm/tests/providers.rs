//! Provider routes besides DeepSeek: request goldens for Anthropic Messages
//! and OpenAI Chat Completions, stream translation against the mock, error
//! and usage mapping, the model list, and the router keeping the DeepSeek
//! wire byte for byte.
//!
//! `UPDATE_GOLDENS=1 cargo test -p dshx-llm --test providers` rewrites the
//! goldens in `tests/fixtures/providers/`.
mod common;

use std::sync::Arc;

use common::MemoryAttachments;
use dshx_core::config::{ProviderCompat, ProviderConfig, ProviderModelConfig};
use dshx_core::{
    ContentBlock, FinishReason, GenerateRequest, ImageAttachmentRef, LlmClient, Message,
    MessageSource, Role, StreamChunk, ToolSchema,
};
use dshx_llm::discover::{Probe, fetch_models};
use dshx_llm::{
    Api, ApiKeySource, BlockAssembler, DeepSeekClient, DeepSeekConfig, ProviderClient,
    ProviderRouter, ProviderSpec, StaticApiKey,
};
use dshx_testkit::{MockDeepSeek, Reply};
use futures::StreamExt;
use serde_json::{Value, json};

const PIXEL_ID: &str = "sha256:21f3b2a077613d783e48f3ed20841820cd61ea1031afe87bbb22bcdf115d08d3";

fn golden(name: &str, actual: &Value) {
    let path = common::fixture_dir("providers").join(name);
    let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden {name}: {e} (run with UPDATE_GOLDENS=1)"));
    assert_eq!(text, expected, "golden {name} differs");
}

fn pixel() -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: PIXEL_ID.into(),
        media_type: "image/png".into(),
        bytes: 70,
        width: 1,
        height: 1,
        name: Some("pixel.png".into()),
        original_dimensions: None,
    }
}

fn attachments() -> MemoryAttachments {
    let store = MemoryAttachments::default();
    store.insert(
        PIXEL_ID,
        common::read_fixture("image", "pixel.png"),
        "image/png",
    );
    store
}

fn user(content: Vec<ContentBlock>) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content,
        source: MessageSource::User,
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

fn assistant(
    provider: &str,
    model: &str,
    content: Vec<ContentBlock>,
    state: Option<Value>,
) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::Assistant,
        content,
        source: MessageSource::Model {
            provider: provider.into(),
            model: model.into(),
            replay_state: state,
        },
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

fn tool_result(call_id: &str, content: Vec<ContentBlock>) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::Tool,
        content,
        source: MessageSource::Tool {
            call_id: call_id.into(),
        },
        tool_call_id: Some(call_id.into()),
        is_error: None,
        extra: Default::default(),
    }
}

fn tools() -> Vec<ToolSchema> {
    vec![
        ToolSchema {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: json!({"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]}),
        },
        ToolSchema {
            name: "bash".into(),
            description: "Run a command.".into(),
            parameters: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
    ]
}

/// A chat that started on `provider/model`, went through one tool call
/// with an image result, and had one DeepSeek turn in between.
fn history(provider: &str, model: &str) -> Vec<Message> {
    let own_state = dshx_llm::anthropic::replay::replay_state(
        provider,
        model,
        &[
            dshx_llm::anthropic::replay::Replay::Signed("sig-plan".into()),
            dshx_llm::anthropic::replay::Replay::Plain("text"),
            dshx_llm::anthropic::replay::Replay::Plain("tool-call"),
        ],
    );
    let deepseek_state = dshx_llm::replay_state(
        "deepseek-flash",
        &[
            dshx_llm::replay::ReplayBlock {
                kind: "reasoning",
                signature: Some("ds-sig".into()),
            },
            dshx_llm::replay::ReplayBlock {
                kind: "text",
                signature: None,
            },
        ],
    );
    vec![
        Message::system("You are terse."),
        user(vec![
            ContentBlock::text("Look at this"),
            ContentBlock::Image {
                attachment: pixel(),
                offloaded: false,
            },
        ]),
        assistant(
            provider,
            model,
            vec![
                ContentBlock::Reasoning {
                    text: "plan".into(),
                },
                ContentBlock::text("Reading it."),
                ContentBlock::ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: r#"{"file_path":"a.rs"}"#.into(),
                },
            ],
            Some(own_state),
        ),
        tool_result(
            "call_1",
            vec![
                ContentBlock::text("fn main() {}"),
                ContentBlock::Image {
                    attachment: pixel(),
                    offloaded: false,
                },
            ],
        ),
        user(vec![ContentBlock::text("Also check the tests.")]),
        assistant(
            "deepseek-official",
            "deepseek-flash",
            vec![
                ContentBlock::Reasoning {
                    text: "foreign thoughts".into(),
                },
                ContentBlock::text("Done."),
            ],
            Some(deepseek_state),
        ),
        user(vec![ContentBlock::text("Thanks. Summarize.")]),
    ]
}

fn request(model: &str, effort: &str, messages: Vec<Message>) -> GenerateRequest {
    GenerateRequest {
        model: model.into(),
        reasoning_effort: Some(effort.into()),
        messages,
        tools: tools(),
        max_tokens: Some(256_000),
        temperature: None,
        session_id: Some("session-1".into()),
    }
}

fn anthropic_spec(base_url: &str) -> ProviderSpec {
    ProviderSpec::resolve(
        "anthropic",
        &ProviderConfig {
            base_url: Some(base_url.into()),
            headers: [("anthropic-beta".to_string(), "test-beta".to_string())].into(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn relay_spec(api: &str, base_url: &str) -> ProviderSpec {
    ProviderSpec::resolve(
        "relay",
        &ProviderConfig {
            api: Some(api.into()),
            base_url: Some(base_url.into()),
            compat: ProviderCompat {
                reasoning_content: Some(true),
                ..Default::default()
            },
            models: vec![ProviderModelConfig {
                id: "glm-5.3".into(),
                input: vec!["text".into(), "image".into()],
                reasoning_efforts: vec!["low".into(), "high".into()],
                max_tokens: Some(131_072),
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .unwrap()
}

fn client(spec: ProviderSpec, key: Option<&str>) -> ProviderClient {
    let key: Arc<dyn ApiKeySource> = match key {
        Some(k) => Arc::new(StaticApiKey(k.into())),
        None => Arc::new(dshx_llm::SharedApiKey::default()),
    };
    ProviderClient::new(
        spec,
        key,
        Some(Arc::new(attachments())),
        Some(Arc::new(common::dsh_attachment_path)),
    )
    .unwrap()
}

async fn collect(
    llm: &dyn LlmClient,
    request: GenerateRequest,
) -> (Vec<StreamChunk>, BlockAssembler) {
    let mut stream = llm.stream(request, Default::default()).await;
    let mut chunks = vec![];
    let mut asm = BlockAssembler::new();
    while let Some(chunk) = stream.next().await {
        asm.push(&chunk);
        chunks.push(chunk);
    }
    (chunks, asm)
}

fn failure(asm: &BlockAssembler) -> dshx_core::LlmFailure {
    match asm.finish() {
        FinishReason::Error { failure } => failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}

// ---- request goldens ----------------------------------------------------------

#[tokio::test]
async fn anthropic_request_golden() {
    let spec = anthropic_spec("https://api.anthropic.com");
    let c = client(spec, Some("k"));
    let body = c
        .build_request(&request(
            "claude-sonnet-5-5",
            "max",
            history("anthropic", "claude-sonnet-5-5"),
        ))
        .await
        .unwrap();
    golden("anthropic-request.json", &body);
    // Own thinking goes back signed, foreign reasoning not at all.
    let text = body.to_string();
    assert!(
        text.contains("sig-plan") && !text.contains("foreign thoughts") && !text.contains("ds-sig")
    );
    // Breakpoints: last tool, system, newest turn.
    assert_eq!(text.matches("\"cache_control\"").count(), 3);
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"], json!({"effort": "max"}));
    // Capped at the model's output limit.
    assert_eq!(body["max_tokens"], 128_000);
}

#[tokio::test]
async fn original_dsh_providers_dispatch_thinking_and_replay_tool_reasoning() {
    for (provider, model, effort, thinking, wire_effort) in [
        (
            "opencode-go",
            "deepseek-v4.1-flash",
            "max",
            json!({"type": "enabled"}),
            Some("max"),
        ),
        (
            "opencode-go",
            "glm-5.3-flash",
            "low",
            Value::Null,
            Some("low"),
        ),
        ("opencode-go", "kimi-k3", "high", Value::Null, Some("max")),
        (
            "xiaomi-token-plan-ams",
            "mimo-v2.6-pro",
            "off",
            json!({"type": "disabled"}),
            None,
        ),
        (
            "xiaomi-token-plan-ams",
            "mimo-v2.6-pro",
            "max",
            json!({"type": "enabled"}),
            Some("high"),
        ),
        (
            "zai",
            "glm-5.3",
            "off",
            json!({"type": "enabled", "clear_thinking": false}),
            Some("low"),
        ),
        (
            "command-code",
            "deepseek/deepseek-v4.1-flash",
            "off",
            Value::Null,
            Some("off"),
        ),
    ] {
        let c = client(
            ProviderSpec::resolve(provider, &ProviderConfig::default()).unwrap(),
            Some("test-key"),
        );
        let r = request(model, effort, history(provider, model));
        let body = c.build_request(&r).await.unwrap();
        assert_eq!(body["thinking"], thinking, "{provider}/{model}");
        assert_eq!(
            body["reasoning_effort"].as_str(),
            wire_effort,
            "{provider}/{model}"
        );
        assert_eq!(
            body["tool_stream"].as_bool(),
            (provider == "zai").then_some(true)
        );
        let assistants: Vec<_> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "assistant")
            .collect();
        assert_eq!(assistants[0]["reasoning_content"], "plan");
        assert_eq!(assistants[1]["reasoning_content"], "");
        assert!(!body.to_string().contains("foreign thoughts"));
    }
}

#[tokio::test]
async fn openai_request_golden() {
    let c = client(
        relay_spec("openai-completions", "https://relay.example/v1"),
        None,
    );
    let body = c
        .build_request(&request("glm-5.3", "max", history("relay", "glm-5.3")))
        .await
        .unwrap();
    golden("openai-request.json", &body);
    let messages = body["messages"].as_array().unwrap();
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "system",
            "user",
            "assistant",
            "tool",
            "user",
            "user",
            "assistant",
            "user"
        ]
    );
    // Same-model reasoning replays as reasoning_content, DeepSeek's does not.
    assert_eq!(messages[2]["reasoning_content"], "plan");
    assert_eq!(messages[6]["reasoning_content"], "");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["max_tokens"], 131_072);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert!(
        messages[1]["content"][2]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    // Tool results are text; their images follow in a user message.
    assert_eq!(messages[4]["content"][1]["type"], "image_url");
}

#[tokio::test]
async fn anthropic_signatures_stay_with_their_model_on_gateways() {
    // A gateway (not api.anthropic.com) gets signatures of the same model only.
    let spec = anthropic_spec("https://gw.example");
    let c = client(spec, Some("k"));
    let body = c
        .build_request(&request(
            "claude-opus-5-5",
            "high",
            history("anthropic", "claude-sonnet-5-5"),
        ))
        .await
        .unwrap();
    assert!(!body.to_string().contains("sig-plan"));
    // The official API takes them across models (it drops what it cannot read).
    let c = client(anthropic_spec("https://api.anthropic.com"), Some("k"));
    let body = c
        .build_request(&request(
            "claude-opus-5-5",
            "high",
            history("anthropic", "claude-sonnet-5-5"),
        ))
        .await
        .unwrap();
    assert!(body.to_string().contains("sig-plan"));
}

#[tokio::test]
async fn efforts_map_to_each_models_thinking() {
    let c = client(anthropic_spec("https://api.anthropic.com"), Some("k"));
    let messages = vec![Message::system("s"), user(vec![ContentBlock::text("hi")])];
    let body = |model: &str, effort: &str| {
        let c = c.clone();
        let r = request(model, effort, messages.clone());
        async move { c.build_request(&r).await.unwrap() }
    };
    // Opus 5.5 cannot turn thinking off: off becomes its lowest level.
    let opus = body("claude-opus-5-5", "off").await;
    assert_eq!(opus["thinking"]["type"], "adaptive");
    assert_eq!(opus["output_config"]["effort"], "low");
    // Haiku 5.5 can.
    let haiku = body("claude-haiku-5-5", "off").await;
    assert_eq!(haiku["thinking"], json!({"type": "disabled"}));
    assert!(haiku.get("output_config").is_none());
    // A model this route does not list asks for nothing.
    let unknown = body("claude-unknown", "high").await;
    assert!(unknown.get("thinking").is_none() && unknown.get("output_config").is_none());
    assert_eq!(unknown["max_tokens"], 64_000);
    // Budget thinking stays below max_tokens.
    let spec = ProviderSpec::resolve(
        "anthropic",
        &ProviderConfig {
            models: vec![ProviderModelConfig {
                id: "claude-haiku-4-5".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let c = client(spec, Some("k"));
    let mut r = request("claude-haiku-4-5", "high", messages.clone());
    r.max_tokens = Some(8000);
    let budget = c.build_request(&r).await.unwrap();
    assert_eq!(
        budget["thinking"],
        json!({"type": "enabled", "budget_tokens": 6976})
    );
    r.reasoning_effort = Some("off".into());
    assert!(c.build_request(&r).await.unwrap().get("thinking").is_none());
}

#[tokio::test]
async fn text_only_models_get_image_placeholders_and_dangling_tools_fail() {
    let c = client(
        relay_spec("anthropic-messages", "https://gw.example"),
        Some("k"),
    );
    let mut spec_messages = history("relay", "glm-5.3");
    let body = c
        .build_request(&request("other-model", "high", spec_messages.clone()))
        .await
        .unwrap();
    let text = body.to_string();
    assert!(
        !text.contains("\"image\"")
            && text.contains("image omitted because this model accepts text only")
    );
    spec_messages.truncate(3);
    let err = c
        .build_request(&request("glm-5.3", "high", spec_messages))
        .await
        .unwrap_err();
    assert_eq!(err.code, "INVALID_REQUEST");
}

// ---- streams --------------------------------------------------------------------

#[tokio::test]
async fn anthropic_stream_headers_usage_and_replay() {
    let mock = MockDeepSeek::with_script([Reply::stream()
        .thinking("thinking about it", "sig-1")
        .text("Hello")
        .tool_call("toolu_1", "bash", json!({"command": "ls"}))
        .usage(120, 30)
        .cache(800, 64)
        .into()])
    .await;
    let c = client(anthropic_spec(&mock.base_url()), Some("sk-ant-test"));
    let messages = vec![Message::system("s"), user(vec![ContentBlock::text("hi")])];
    let (chunks, asm) = collect(&c, request("claude-sonnet-5-5", "high", messages)).await;
    let r = &mock.requests()[0];
    assert_eq!(r.path, "/anthropic/v1/messages");
    assert_eq!(r.header("x-api-key"), Some("sk-ant-test"));
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(r.header("anthropic-beta"), Some("test-beta"));
    assert_eq!(r.header("authorization"), None);
    assert_eq!(r.header("x-deepseek-harness-session-id"), None);
    assert_eq!(asm.finish(), FinishReason::ToolCalls);
    let usage = asm.usage().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.output_tokens
        ),
        (120, Some(800), Some(64), 30)
    );
    let message = asm.message("anthropic", "claude-sonnet-5-5");
    assert_eq!(message.content.len(), 3);
    let MessageSource::Model { replay_state, .. } = &message.source else {
        panic!()
    };
    assert_eq!(
        replay_state.as_ref().unwrap()["blocks"][0],
        json!({"type": "reasoning", "signature": "sig-1"})
    );
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, StreamChunk::ReasoningDelta { .. }))
    );
    // The replayed history sends the signature back.
    let next = request(
        "claude-sonnet-5-5",
        "high",
        vec![
            Message::system("s"),
            user(vec![ContentBlock::text("hi")]),
            message,
            tool_result("toolu_1", vec![ContentBlock::text("ok")]),
        ],
    );
    let body = c.build_request(&next).await.unwrap();
    assert_eq!(body["messages"][1]["content"][0]["signature"], "sig-1");
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

fn start(input: u64) -> Value {
    json!({"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "content": [],
        "usage": {"input_tokens": input, "output_tokens": 1, "cache_read_input_tokens": null}}})
}

#[tokio::test]
async fn anthropic_redacted_thinking_refusal_and_overflow() {
    let redacted = sse(&[
        start(10),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "redacted_thinking", "data": "enc"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "server_tool_use", "id": "s", "name": "web_search", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "ok"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 4}}),
        json!({"type": "message_stop"}),
    ]);
    let refusal = sse(&[
        start(10),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "Sure, "}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}}}),
        json!({"type": "message_stop"}),
    ]);
    let overflow = sse(&[
        start(10),
        json!({"type": "message_delta", "delta": {"stop_reason": "model_context_window_exceeded"}}),
        json!({"type": "message_stop"}),
    ]);
    let mock = MockDeepSeek::with_script([
        Reply::RawSse(redacted),
        Reply::RawSse(refusal),
        Reply::RawSse(overflow),
        Reply::error(401, "authentication_error", "invalid x-api-key"),
        Reply::rate_limited(3),
        Reply::error(529, "overloaded_error", "Overloaded"),
    ])
    .await;
    let c = client(anthropic_spec(&mock.base_url()), Some("k"));
    let r = || {
        request(
            "claude-opus-5-5",
            "high",
            vec![user(vec![ContentBlock::text("hi")])],
        )
    };
    let (_, asm) = collect(&c, r()).await;
    assert_eq!(asm.finish(), FinishReason::Stop);
    let message = asm.message("anthropic", "claude-opus-5-5");
    assert_eq!(
        message.content,
        vec![
            ContentBlock::Reasoning {
                text: String::new()
            },
            ContentBlock::text("ok")
        ]
    );
    let MessageSource::Model { replay_state, .. } = &message.source else {
        panic!()
    };
    assert_eq!(
        replay_state.as_ref().unwrap()["blocks"][0]["redacted"],
        "enc"
    );
    let refused = failure(&collect(&c, r()).await.1);
    assert_eq!(refused.code, "REFUSAL");
    assert!(refused.message.contains("cyber"));
    assert_eq!(
        failure(&collect(&c, r()).await.1).code,
        "CONTEXT_WINDOW_EXCEEDED"
    );
    assert_eq!(failure(&collect(&c, r()).await.1).code, "AUTH");
    let limited = failure(&collect(&c, r()).await.1);
    assert_eq!(
        (limited.code.as_str(), limited.provider_retry_after_ms),
        ("RATE_LIMIT", Some(3000))
    );
    assert_eq!(failure(&collect(&c, r()).await.1).code, "SERVER");
}

fn chat_chunk(delta: Value, finish: Option<&str>) -> String {
    let chunk = json!({"id": "c", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    format!("data: {chunk}\n\n")
}

#[tokio::test]
async fn opencode_go_routes_conversations_and_auxiliary_requests() {
    let reply =
        || Reply::RawSse(chat_chunk(json!({"content": "ok"}), Some("stop")) + "data: [DONE]\n\n");
    let mock = MockDeepSeek::with_script([reply(), reply(), reply(), reply()]).await;
    let spec = ProviderSpec::resolve(
        "opencode-go",
        &ProviderConfig {
            base_url: Some(format!("http://{}/v1", mock.addr())),
            ..Default::default()
        },
    )
    .unwrap();
    let c = client(spec, Some("sk-go-test"));
    for session in [Some("session-one"), Some("session-two"), None, None] {
        let mut r = request("deepseek-v4.1-flash", "low", vec![Message::user_text("hi")]);
        r.session_id = session.map(str::to_owned);
        assert_eq!(collect(&c, r).await.1.finish(), FinishReason::Stop);
    }
    let requests = mock.requests();
    assert_eq!(
        requests[0].header("x-opencode-session"),
        Some("session-one")
    );
    assert_eq!(
        requests[1].header("x-opencode-session"),
        Some("session-two")
    );
    let fallback = requests[2].header("x-opencode-session").unwrap();
    assert!(!fallback.is_empty() && fallback != "session-one");
    assert_eq!(requests[3].header("x-opencode-session"), Some(fallback));
    assert!(
        requests
            .iter()
            .all(|r| r.header("user-agent").unwrap().starts_with("dsh-codex/"))
    );
}

#[tokio::test]
async fn openai_stream_reasoning_tools_and_cached_usage() {
    let body = [
        chat_chunk(json!({"role": "assistant", "reasoning_content": "Let me "}), None),
        chat_chunk(json!({"reasoning_content": "look."}), None),
        chat_chunk(json!({"content": "Running ls."}), None),
        chat_chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "bash", "arguments": "{\"comm"}}]}), None),
        chat_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "and\":\"ls\"}"}}]}), None),
        chat_chunk(json!({}), Some("tool_calls")),
        format!("data: {}\n\n", json!({"choices": [], "usage": {"prompt_tokens": 1000, "completion_tokens": 40,
            "prompt_tokens_details": {"cached_tokens": 900}, "completion_tokens_details": {"reasoning_tokens": 12}}})),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    let error = json!({"error": {"message": "This model's maximum context length is 8192 tokens", "type": "invalid_request_error", "code": "context_length_exceeded"}});
    let mock = MockDeepSeek::with_script([
        Reply::RawSse(body),
        Reply::http(400, error.to_string()),
        Reply::RawSse(chat_chunk(json!({"content": "cut"}), None)),
    ])
    .await;
    let base = format!("http://{}/v1", mock.addr());
    let c = client(relay_spec("openai-completions", &base), Some("sk-relay"));
    let r = || {
        request(
            "glm-5.3",
            "low",
            vec![Message::system("s"), user(vec![ContentBlock::text("hi")])],
        )
    };
    let (_, asm) = collect(&c, r()).await;
    let recorded = &mock.requests()[0];
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(recorded.header("authorization"), Some("Bearer sk-relay"));
    assert_eq!(recorded.header("x-api-key"), None);
    assert_eq!(recorded.body["reasoning_effort"], "low");
    assert_eq!(asm.finish(), FinishReason::ToolCalls);
    assert_eq!(
        asm.blocks(),
        vec![
            ContentBlock::Reasoning {
                text: "Let me look.".into()
            },
            ContentBlock::text("Running ls."),
            ContentBlock::ToolCall {
                id: "call_a".into(),
                name: "bash".into(),
                arguments: r#"{"command":"ls"}"#.into()
            },
        ]
    );
    let usage = asm.usage().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens,
            usage.reasoning_tokens
        ),
        (100, Some(900), 40, Some(12))
    );
    assert_eq!(
        failure(&collect(&c, r()).await.1).code,
        "CONTEXT_WINDOW_EXCEEDED"
    );
    assert_eq!(failure(&collect(&c, r()).await.1).code, "STREAM_CLOSED");
}

#[tokio::test]
async fn keys_are_required_only_where_the_catalog_says() {
    let mock = MockDeepSeek::with_script([Reply::text("ok")]).await;
    // A catalog provider without a key fails before sending anything.
    let c = client(anthropic_spec(&mock.base_url()), None);
    let r = request(
        "claude-opus-5-5",
        "high",
        vec![user(vec![ContentBlock::text("hi")])],
    );
    let missing = failure(&collect(&c, r.clone()).await.1);
    assert_eq!(missing.code, "MISSING_CREDENTIAL");
    assert!(missing.message.contains("ANTHROPIC_API_KEY"));
    // A custom endpoint may need none: no auth header at all.
    let c = client(relay_spec("anthropic-messages", &mock.base_url()), None);
    let (_, asm) = collect(
        &c,
        request(
            "glm-5.3",
            "high",
            vec![user(vec![ContentBlock::text("hi")])],
        ),
    )
    .await;
    assert_eq!(asm.finish(), FinishReason::Stop);
    let recorded = &mock.requests()[0];
    assert_eq!(
        (
            recorded.header("x-api-key"),
            recorded.header("authorization")
        ),
        (None, None)
    );
}

// ---- router ---------------------------------------------------------------------

#[tokio::test]
async fn router_keeps_the_deepseek_wire_byte_for_byte() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|_| Reply::text("ok"));
    let config = DeepSeekConfig {
        base_url: mock.base_url(),
        ..Default::default()
    };
    let deepseek = Arc::new(common::fixture_client(config));
    let router = ProviderRouter::new(deepseek.clone());
    router.set_routes(vec![client(anthropic_spec(&mock.base_url()), Some("k"))]);
    let messages = vec![
        Message::system("You are terse."),
        user(vec![
            ContentBlock::text("Look"),
            ContentBlock::Image {
                attachment: pixel(),
                offloaded: false,
            },
        ]),
    ];
    let r = request("deepseek-flash", "max", messages);
    collect(deepseek.as_ref(), r.clone()).await;
    collect(&router, r.clone()).await;
    let routed = router.for_provider("deepseek-official").unwrap();
    collect(routed.as_ref(), r.clone()).await;
    let sent = mock.requests();
    assert_eq!(sent.len(), 3);
    for other in &sent[1..] {
        assert_eq!(other.raw_body, sent[0].raw_body);
        assert_eq!(other.headers, sent[0].headers);
    }
    let wire = deepseek.build_request(&r).await.unwrap();
    assert_eq!(sent[0].raw_body, dshx_llm::json::to_vec(&wire.body));
    // Another route answers for its own provider, an unknown one fails clearly.
    assert_eq!(
        router.for_provider("anthropic").unwrap().provider(),
        "anthropic"
    );
    let gone = router.for_provider("gone").unwrap();
    let (_, asm) = collect(gone.as_ref(), r).await;
    assert_eq!(failure(&asm).code, "PROVIDER_UNAVAILABLE");
    assert_eq!(mock.requests().len(), 3);
}

#[tokio::test]
async fn router_resolves_references_with_prices() {
    let router = ProviderRouter::new(Arc::new(DeepSeekClient::from_env().unwrap()));
    router.set_routes(vec![client(
        anthropic_spec("https://api.anthropic.com"),
        Some("k"),
    )]);
    assert_eq!(
        router.split("deepseek-flash"),
        ("deepseek-official".into(), "deepseek-flash".into())
    );
    assert_eq!(
        router.split("anthropic/claude-opus-5-5"),
        ("anthropic".into(), "claude-opus-5-5".into())
    );
    // A bare id that only one other route knows goes there.
    assert_eq!(
        router.split("claude-haiku-5-5"),
        ("anthropic".into(), "claude-haiku-5-5".into())
    );
    let resolved = router.resolve("anthropic/claude-sonnet-5-5");
    assert_eq!(resolved.model.info.context_window, 1_000_000);
    assert_eq!(resolved.model.default_max_tokens, 64_000);
    assert_eq!(resolved.model.cost.unwrap().output, 10.0);
    assert_eq!(router.resolve("deepseek-flash").model.cost, None);
}

// ---- model lists ------------------------------------------------------------------

#[tokio::test]
async fn model_lists_of_both_protocols() {
    let mock = MockDeepSeek::start().await;
    mock.set_models(json!({"data": [
        {"type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5", "max_input_tokens": 1000000, "max_tokens": 128000},
        {"type": "model", "id": "claude-next", "display_name": "Claude Next"}
    ], "has_more": false, "last_id": "claude-next"}));
    let models = fetch_models(&Probe {
        api: Api::AnthropicMessages,
        base_url: &mock.base_url(),
        key: Some("sk-ant"),
        bearer: false,
        headers: &[],
    })
    .await
    .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].cost.unwrap().input, 2.0);
    assert_eq!(models[1].name.as_deref(), Some("Claude Next"));
    let r = mock
        .all_requests()
        .into_iter()
        .find(|r| r.method == "GET")
        .unwrap();
    assert_eq!(r.path, "/anthropic/v1/models?limit=1000");
    assert_eq!(r.header("x-api-key"), Some("sk-ant"));

    let openai = fetch_models(&Probe {
        api: Api::OpenAiCompletions,
        base_url: &format!("http://{}/v1", mock.addr()),
        key: Some("sk-relay"),
        bearer: true,
        headers: &[],
    })
    .await
    .unwrap();
    assert_eq!(openai.len(), 2);
    let r = mock
        .all_requests()
        .into_iter()
        .rfind(|r| r.method == "GET")
        .unwrap();
    assert_eq!(
        (r.path.as_str(), r.header("authorization")),
        ("/v1/models", Some("Bearer sk-relay"))
    );
}
