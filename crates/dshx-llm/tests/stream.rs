//! SSE translation (ported from dsh `stream.spec.ts`) and the HTTP client
//! against `MockDeepSeek`: errors, disconnects, timeouts, cancellation.
use std::sync::Arc;
use std::time::{Duration, Instant};

use dshx_core::{
    ContentBlock, FinishReason, GenerateRequest, LlmClient, LlmFailure, Message, StreamChunk,
    TokenUsage,
};
use dshx_llm::translate::Translator;
use dshx_llm::{BlockAssembler, DeepSeekClient, DeepSeekConfig, SharedApiKey, StaticApiKey};
use dshx_testkit::{MockDeepSeek, Reply, sse_text};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "deepseek-flash";

fn start() -> Value {
    json!({"type": "message_start", "message": {"id": "msg_1", "model": MODEL, "usage": {"input_tokens": 12, "output_tokens": 1}}})
}
fn text_start() -> Value {
    json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
}
fn text_delta(text: &str) -> Value {
    json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}})
}
fn stop(index: u64) -> Value {
    json!({"type": "content_block_stop", "index": index})
}
fn end(reason: &str) -> Vec<Value> {
    vec![
        json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ]
}
fn text_events() -> Vec<Value> {
    let mut events = vec![start(), text_start(), text_delta("Hello 世界"), stop(0)];
    events.extend(end("end_turn"));
    events
}

/// Runs events through the translator; `Err` carries the chunks before the failure.
fn translate(events: &[Value]) -> Result<Vec<StreamChunk>, (Vec<StreamChunk>, LlmFailure)> {
    let mut translator = Translator::new(MODEL);
    let mut out = vec![];
    for event in events {
        let map: Map<String, Value> = event.as_object().cloned().unwrap();
        match translator.push(&map) {
            Ok(chunks) => out.extend(chunks),
            Err(failure) => return Err((out, failure)),
        }
        if translator.is_done() {
            return Ok(out);
        }
    }
    match translator.finish() {
        Ok(()) => Ok(out),
        Err(failure) => Err((out, failure)),
    }
}

fn code(events: &[Value]) -> String {
    translate(events)
        .map(|_| "ok".to_string())
        .unwrap_or_else(|(_, f)| f.code)
}

fn assemble(chunks: &[StreamChunk]) -> BlockAssembler {
    let mut assembler = BlockAssembler::new();
    chunks.iter().for_each(|c| assembler.push(c));
    assembler
}

#[test]
fn streams_text_already_present_in_the_starting_block() {
    let mut events = vec![
        start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "initial"}}),
        stop(0),
    ];
    events.extend(end("end_turn"));
    let chunks = translate(&events).unwrap();
    assert!(chunks.contains(&StreamChunk::TextDelta {
        index: 0,
        text: "initial".into()
    }));
}

#[test]
fn first_seen_blocks_raw_tool_json_and_disjoint_usage() {
    let events = vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 12, "output_tokens": 1, "cache_read_input_tokens": 30, "cache_creation_input_tokens": 7}}}),
        json!({"type": "content_block_start", "index": 4, "content_block": {"type": "thinking", "thinking": "first", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 4, "delta": {"type": "thinking_delta", "thinking": " thought"}}),
        json!({"type": "content_block_delta", "index": 4, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_delta", "index": 4, "delta": {"type": "signature_delta", "signature": "nature"}}),
        stop(4),
        json!({"type": "content_block_start", "index": 9, "content_block": {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 9, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
        json!({"type": "content_block_delta", "index": 9, "delta": {"type": "input_json_delta", "partial_json": " \"a\"}"}}),
        stop(9),
        json!({"type": "message_delta", "delta": {}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ];
    let chunks = translate(&events).unwrap();
    let starts: Vec<usize> = chunks
        .iter()
        .filter_map(|c| {
            if let StreamChunk::BlockStart { index, .. } = c {
                Some(*index)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(starts, vec![0, 1]);
    let usage = TokenUsage {
        input_tokens: 12,
        output_tokens: 5,
        total_tokens: Some(54),
        cache_read_tokens: Some(30),
        cache_write_tokens: Some(7),
        reasoning_tokens: None,
    };
    assert_eq!(chunks[chunks.len() - 2], StreamChunk::Usage(usage));
    let message = assemble(&chunks).message("deepseek-official", MODEL);
    assert_eq!(
        message.content,
        vec![
            ContentBlock::Reasoning {
                text: "first thought".into()
            },
            ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: "{\"path\": \"a\"}".into()
            },
        ]
    );
    let StreamChunk::Finish {
        reason,
        replay_state,
    } = chunks.last().unwrap()
    else {
        panic!()
    };
    assert_eq!(reason, &FinishReason::ToolCalls);
    assert_eq!(
        replay_state.as_ref().unwrap(),
        &json!({"response": {"kind": "deepseek-messages", "version": 1, "model": MODEL}, "blocks": [{"type": "reasoning", "signature": "signature"}, {"type": "tool-call"}]})
    );
}

#[test]
fn maps_stop_reasons_and_ignores_unknown_envelope_events() {
    for (reason, expected) in [
        ("end_turn", FinishReason::Stop),
        ("stop_sequence", FinishReason::Stop),
        ("max_tokens", FinishReason::MaxTokens),
    ] {
        let mut events = vec![
            start(),
            json!({"type": "future_event"}),
            json!({"type": "ping"}),
            text_start(),
            text_delta("x"),
            stop(0),
        ];
        events.extend(end(reason));
        let chunks = translate(&events).unwrap();
        assert!(
            matches!(chunks.last(), Some(StreamChunk::Finish { reason: r, .. }) if *r == expected)
        );
    }
}

#[test]
fn preserves_initial_tool_input_without_json_deltas() {
    for input in [json!({}), json!({"key": "initial", "n": 1.0})] {
        let mut events = vec![
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "read", "input": input}}),
            stop(0),
        ];
        events.extend(end("tool_use"));
        let message = assemble(&translate(&events).unwrap()).message("deepseek-official", MODEL);
        let ContentBlock::ToolCall { arguments, .. } = &message.content[0] else {
            panic!()
        };
        // JSON.stringify: 1.0 is written as 1.
        assert_eq!(
            arguments,
            if input.as_object().unwrap().is_empty() {
                "{}"
            } else {
                r#"{"key":"initial","n":1}"#
            }
        );
    }
}

#[test]
fn keeps_signature_only_thinking_and_prunes_truncated_tools_with_replay() {
    let mut events = vec![
        start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "opaque"}}),
        stop(0),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "a", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{"}}),
        stop(1),
    ];
    events.extend(end("max_tokens"));
    let assembler = assemble(&translate(&events).unwrap());
    let message = assembler.message("deepseek-official", MODEL);
    assert_eq!(
        message.content,
        vec![ContentBlock::Reasoning {
            text: String::new()
        }]
    );
    let dshx_core::MessageSource::Model { replay_state, .. } = &message.source else {
        panic!()
    };
    assert_eq!(
        replay_state.as_ref().unwrap()["blocks"],
        json!([{"type": "reasoning", "signature": "opaque"}])
    );
}

#[test]
fn rejects_malformed_event_ordering_or_fields() {
    let t1 = text_start();
    let cases: Vec<Vec<Value>> = vec![
        vec![start(), start()],
        vec![t1.clone()],
        vec![
            start(),
            json!({"type": "content_block_start", "index": -1, "content_block": {"type": "text", "text": ""}}),
        ],
        vec![start(), t1.clone(), t1.clone()],
        vec![start(), stop(0)],
        vec![start(), t1.clone(), stop(0), stop(0)],
        vec![
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": 2}}),
        ],
        vec![
            start(),
            t1.clone(),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "bad"}}),
        ],
        vec![
            start(),
            json!({"type": "message_delta", "delta": {"stop_reason": "mystery"}}),
        ],
        vec![start(), json!({"type": "message_stop"})],
        [vec![start(), t1.clone()], end("end_turn")].concat(),
        vec![
            start(),
            json!({"type": "message_delta", "delta": {}, "usage": {"input_tokens": -1}}),
        ],
        vec![
            start(),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            t1.clone(),
        ],
        vec![
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "", "name": "read", "input": {}}}),
        ],
        vec![
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "x", "name": "read", "input": []}}),
        ],
    ];
    for (n, events) in cases.iter().enumerate() {
        assert_eq!(code(events), "MALFORMED_RESPONSE", "case {n}");
    }
}

#[test]
fn refuses_non_object_tool_json_unsupported_blocks_empty_responses_and_eof() {
    for partial in ["{", "[]"] {
        let mut events = vec![
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "read", "input": {}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": partial}}),
            stop(0),
        ];
        events.extend(end("tool_use"));
        assert_eq!(code(&events), "MALFORMED_RESPONSE", "{partial}");
    }
    let redacted = vec![
        start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "redacted_thinking", "data": "x"}}),
    ];
    assert_eq!(code(&redacted), "UNSUPPORTED_CONTENT");
    assert_eq!(
        code(&[vec![start()], end("end_turn")].concat()),
        "EMPTY_RESPONSE"
    );
    let events = text_events();
    assert_eq!(code(&events[..events.len() - 1]), "STREAM_CLOSED");
}

// ---- over HTTP ----

fn request(text: &str) -> GenerateRequest {
    GenerateRequest {
        model: MODEL.into(),
        reasoning_effort: None,
        messages: vec![Message::system("Be terse."), Message::user_text(text)],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        session_id: Some("session-test".into()),
    }
}

fn client_for(mock: &MockDeepSeek) -> DeepSeekClient {
    client_with(mock, DeepSeekConfig::default())
}

fn client_with(mock: &MockDeepSeek, config: DeepSeekConfig) -> DeepSeekClient {
    DeepSeekClient::builder(DeepSeekConfig {
        base_url: mock.base_url(),
        ..config
    })
    .api_key(Arc::new(StaticApiKey("sk-test".into())))
    .build()
    .unwrap()
}

async fn run(client: &DeepSeekClient, request: GenerateRequest) -> Vec<StreamChunk> {
    client
        .stream(request, CancellationToken::new())
        .await
        .collect()
        .await
}

fn failure(chunks: &[StreamChunk]) -> &LlmFailure {
    match chunks.last() {
        Some(StreamChunk::Finish {
            reason: FinishReason::Error { failure } | FinishReason::Aborted { failure },
            ..
        }) => failure,
        other => panic!("expected a failed finish, got {other:?}"),
    }
}

fn finishes(chunks: &[StreamChunk]) -> usize {
    chunks
        .iter()
        .filter(|c| matches!(c, StreamChunk::Finish { .. }))
        .count()
}

#[tokio::test]
async fn split_chunks_over_http_give_the_same_stream() {
    let mock = MockDeepSeek::start().await;
    let reply = Reply::stream()
        .thinking("Ünïcödé thoughts 思考", "sig-1")
        .text_chunks(["Hello ", "世界"])
        .tool_call(
            "toolu_1",
            "bash",
            json!({"command": "ls -la", "description": "List"}),
        )
        .ping()
        .cache(80, 3);
    mock.push(reply.clone());
    mock.push(reply.split_bytes(5));
    mock.push(Reply::RawSse(format!(
        "\u{FEFF}: heartbeat\r\n\r\n{}",
        sse_text(&text_events()).replace('\n', "\r\n")
    )));
    let client = client_for(&mock);
    let whole = run(&client, request("a")).await;
    let split = run(&client, request("b")).await;
    assert_eq!(whole, split);
    assert_eq!(finishes(&whole), 1);
    let assembler = assemble(&whole);
    assert_eq!(assembler.finish(), FinishReason::ToolCalls);
    assert_eq!(assembler.usage().unwrap().cache_read_tokens, Some(80));
    let message = assembler.message("deepseek-official", MODEL);
    assert_eq!(
        message.content[0],
        ContentBlock::Reasoning {
            text: "Ünïcödé thoughts 思考".into()
        }
    );
    assert_eq!(message.plain_text(), "Hello 世界");
    let crlf = run(&client, request("c")).await;
    assert_eq!(
        assemble(&crlf).message("p", MODEL).plain_text(),
        "Hello 世界"
    );
}

#[tokio::test]
async fn http_errors_map_to_dsh_failure_codes() {
    let mock = MockDeepSeek::start().await;
    let cases = vec![
        (Reply::rate_limited(2), "RATE_LIMIT", Some(429)),
        (Reply::server_error(), "SERVER", Some(500)),
        (Reply::overloaded(), "SERVER", Some(529)),
        (
            Reply::context_overflow(),
            "CONTEXT_WINDOW_EXCEEDED",
            Some(400),
        ),
        (
            Reply::error(400, "invalid_request_error", "messages: bad"),
            "INVALID_REQUEST",
            Some(400),
        ),
        (
            Reply::error(401, "authentication_error", "invalid x-api-key"),
            "AUTH",
            Some(401),
        ),
        (
            Reply::error(402, "invalid_request_error", "Insufficient Balance"),
            "QUOTA",
            Some(402),
        ),
        (
            Reply::http(502, "<html>bad gateway</html>"),
            "SERVER",
            Some(502),
        ),
        (Reply::http(404, "nope"), "HTTP_404", Some(404)),
    ];
    let client = client_for(&mock);
    for (reply, expected, status) in cases {
        mock.push(reply);
        let chunks = run(&client, request("x")).await;
        assert_eq!(chunks.len(), 1, "{expected}: only the finish");
        let failure = failure(&chunks);
        assert_eq!(
            (failure.code.as_str(), failure.status),
            (expected, status),
            "{failure:?}"
        );
    }
    mock.push(Reply::rate_limited(2).header("request-id", "req-42"));
    let chunks = run(&client, request("x")).await;
    assert_eq!(failure(&chunks).provider_retry_after_ms, Some(2000));
    assert_eq!(failure(&chunks).request_id.as_deref(), Some("req-42"));
    mock.push(Reply::http(502, "not json"));
    assert_eq!(
        failure(&run(&client, request("x")).await).message,
        "DeepSeek Messages request failed (502)"
    );
}

#[tokio::test]
async fn in_band_error_disconnect_and_early_end() {
    let mock = MockDeepSeek::start().await;
    let mut events = vec![start(), text_start(), text_delta("partial")];
    events.push(
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    );
    mock.push(Reply::RawSse(sse_text(&events)));
    mock.push(
        Reply::stream()
            .text_chunks(["one ", "two ", "three"])
            .disconnect_after(3),
    );
    mock.push(Reply::stream().text_chunks(["one ", "two"]).end_after(3));
    let client = client_for(&mock);

    let chunks = run(&client, request("in-band")).await;
    assert!(chunks.contains(&StreamChunk::TextDelta {
        index: 0,
        text: "partial".into()
    }));
    assert_eq!(failure(&chunks).code, "SERVER");
    assert_eq!(failure(&chunks).message, "Overloaded");

    let chunks = run(&client, request("disconnect")).await;
    assert_eq!(failure(&chunks).code, "TRANSPORT");
    assert_eq!(
        assemble(&chunks).interrupted_blocks(),
        vec![ContentBlock::text("one ")]
    );

    let chunks = run(&client, request("early end")).await;
    assert_eq!(failure(&chunks).code, "STREAM_CLOSED");
    assert_eq!(finishes(&chunks), 1);
}

#[tokio::test]
async fn cancellation_aborts_an_open_stream_promptly() {
    let mock = MockDeepSeek::start().await;
    mock.push(Reply::stream().text_chunks(["a", "b", "c"]).stall_after(3));
    let client = client_for(&mock);
    let cancel = CancellationToken::new();
    let mut stream = client.stream(request("x"), cancel.clone()).await;
    let mut seen = vec![];
    while let Some(chunk) = stream.next().await {
        let is_delta = matches!(chunk, StreamChunk::TextDelta { .. });
        seen.push(chunk);
        if is_delta {
            break;
        }
    }
    let started = Instant::now();
    cancel.cancel();
    let rest: Vec<StreamChunk> = tokio::time::timeout(Duration::from_secs(2), stream.collect())
        .await
        .expect("abort is prompt");
    assert!(started.elapsed() < Duration::from_millis(500));
    let failure = match rest.last() {
        Some(StreamChunk::Finish {
            reason: FinishReason::Aborted { failure },
            replay_state: None,
        }) => failure,
        other => panic!("expected aborted finish, got {other:?}"),
    };
    assert_eq!(
        (failure.code.as_str(), failure.message.as_str()),
        ("ABORTED", "DeepSeek Messages request aborted")
    );
    // The connection is gone; the next request opens a new one and works.
    mock.push(Reply::text("again"));
    let again = run(&client, request("y")).await;
    assert_eq!(assemble(&again).message("p", MODEL).plain_text(), "again");
}

#[tokio::test]
async fn cancelled_before_start_sends_nothing() {
    let mock = MockDeepSeek::start().await;
    let client = client_for(&mock);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let chunks: Vec<_> = client.stream(request("x"), cancel).await.collect().await;
    assert!(matches!(
        chunks.as_slice(),
        [StreamChunk::Finish {
            reason: FinishReason::Aborted { .. },
            ..
        }]
    ));
    assert!(mock.all_requests().is_empty());
}

#[tokio::test]
async fn idle_stream_times_out() {
    let mock = MockDeepSeek::start().await;
    mock.push(Reply::stream().text("slow").stall_after(2));
    mock.push(
        Reply::stream()
            .text_chunks(["a", "b"])
            .delay(Duration::from_millis(60)),
    );
    let config = DeepSeekConfig {
        stream_idle_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let client = client_with(&mock, config);
    let chunks = run(&client, request("x")).await;
    assert_eq!(failure(&chunks).code, "TIMEOUT");
    // Slow but steady streams stay alive: every event resets the deadline.
    let chunks = run(&client, request("y")).await;
    assert_eq!(assemble(&chunks).message("p", MODEL).plain_text(), "ab");
}

#[tokio::test]
async fn credentials_are_resolved_per_request() {
    let mock = MockDeepSeek::start().await;
    let key = SharedApiKey::default();
    let client = DeepSeekClient::builder(DeepSeekConfig {
        base_url: mock.base_url(),
        ..Default::default()
    })
    .api_key(Arc::new(key.clone()))
    .build()
    .unwrap();
    let chunks = run(&client, request("x")).await;
    assert_eq!(failure(&chunks).code, "MISSING_CREDENTIAL");
    assert!(mock.all_requests().is_empty());
    key.set(Some("sk-stored".into()));
    mock.push(Reply::text("hi"));
    run(&client, request("x")).await;
    assert_eq!(mock.requests()[0].header("x-api-key"), Some("sk-stored"));
}

#[tokio::test]
async fn unreachable_endpoint_is_a_transport_failure() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let client = DeepSeekClient::builder(DeepSeekConfig {
        base_url: format!("http://127.0.0.1:{port}/anthropic"),
        ..Default::default()
    })
    .api_key(Arc::new(StaticApiKey("sk".into())))
    .build()
    .unwrap();
    let chunks = run(&client, request("x")).await;
    let failure = failure(&chunks);
    assert_eq!(failure.code, "TRANSPORT");
    assert!(
        failure
            .message
            .starts_with("DeepSeek Messages transport failed")
    );
}

#[tokio::test]
async fn dropping_the_stream_releases_the_request() {
    let mock = MockDeepSeek::start().await;
    mock.push(Reply::stream().text_chunks(["a", "b"]).stall_after(3));
    let client = client_for(&mock);
    let mut stream = client.stream(request("x"), CancellationToken::new()).await;
    while let Some(chunk) = stream.next().await {
        if matches!(chunk, StreamChunk::TextDelta { .. }) {
            break;
        }
    }
    drop(stream);
    mock.push(Reply::text("next"));
    let chunks = tokio::time::timeout(Duration::from_secs(2), run(&client, request("y")))
        .await
        .unwrap();
    assert_eq!(assemble(&chunks).message("p", MODEL).plain_text(), "next");
}
