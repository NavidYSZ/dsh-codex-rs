//! Request conversion (ported from dsh `serialize.spec.ts`), route
//! projections, images and the model catalog.
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::MemoryAttachments;
use dshx_core::{
    ContentBlock, GenerateRequest, ImageAttachmentRef, LlmClient, LlmFailure, Message,
    MessageSource, Role, StreamChunk, ToolSchema,
};
use dshx_llm::serialize::{BuildContext, serialize};
use dshx_llm::{
    CatalogModel, DeepSeekClient, DeepSeekConfig, StaticApiKey, SystemPromptUpdate, ToolUpdate,
    json, replay_state,
};
use dshx_testkit::{MockDeepSeek, Reply};
use futures::StreamExt;
use serde_json::{Value, json};

/// Uncatalogued like dsh's test model: no in-history updates, text only.
const MODEL: &str = "deepseek-v4-flash";

fn user(text: &str) -> Message {
    Message::user_text(text)
}

fn assistant(content: Vec<ContentBlock>) -> Message {
    assistant_with(content, None)
}

fn assistant_with(content: Vec<ContentBlock>, replay: Option<Value>) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::Assistant,
        content,
        source: MessageSource::Model {
            provider: "deepseek-official".into(),
            model: MODEL.into(),
            replay_state: replay,
        },
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolCall {
        id: id.into(),
        name: "read".into(),
        arguments: r#"{"path":"a"}"#.into(),
    }
}

fn result(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::tool_result(id, content, false)
}

fn result_text(id: &str) -> Message {
    result(id, vec![ContentBlock::text("result")])
}

fn developer(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::Developer,
        content,
        source: MessageSource::Other(json!({"kind": "tool-registry"})),
        ..user("")
    }
}

fn request(messages: Vec<Message>) -> GenerateRequest {
    GenerateRequest {
        model: MODEL.into(),
        reasoning_effort: None,
        messages,
        tools: vec![],
        max_tokens: None,
        temperature: None,
        session_id: None,
    }
}

fn serialize_with(config: &DeepSeekConfig, request: &GenerateRequest) -> Result<Value, LlmFailure> {
    let images = HashMap::new();
    let ctx = BuildContext {
        config,
        provider: "deepseek-official",
        images: &images,
        image_path: None,
    };
    serialize(request, &request.messages, &ctx).map(|w| w.body)
}

fn body(messages: Vec<Message>) -> Value {
    serialize_with(&DeepSeekConfig::default(), &request(messages)).unwrap()
}

fn body_err(messages: Vec<Message>) -> LlmFailure {
    serialize_with(&DeepSeekConfig::default(), &request(messages)).unwrap_err()
}

fn capable() -> DeepSeekConfig {
    let mut config = DeepSeekConfig::default();
    config.models.push(CatalogModel {
        id: MODEL.into(),
        name: None,
        description: None,
        context_window: None,
        max_tokens: None,
        input_modalities: vec!["text".into()],
        system_prompt_update: Some(SystemPromptUpdate::InHistory),
        tool_update: None,
    });
    config
}

fn native(messages: Vec<Message>) -> Result<Value, LlmFailure> {
    serialize_with(&capable(), &request(messages))
}

#[test]
fn omits_saved_empty_assistant_text_without_changing_tool_ids_or_signatures() {
    let id = "toolu_example|toolu_example";
    let content = vec![
        ContentBlock::text(""),
        ContentBlock::Reasoning {
            text: "reason".into(),
        },
        call(id),
        ContentBlock::text(""),
    ];
    let replay = replay_state(
        MODEL,
        &[
            dshx_llm::replay::ReplayBlock {
                kind: "text",
                signature: None,
            },
            dshx_llm::replay::ReplayBlock {
                kind: "reasoning",
                signature: Some("signed".into()),
            },
            dshx_llm::replay::ReplayBlock {
                kind: "tool-call",
                signature: None,
            },
            dshx_llm::replay::ReplayBlock {
                kind: "text",
                signature: None,
            },
        ],
    );
    let message = assistant_with(content, Some(replay));
    let request = body(vec![
        user("hello"),
        assistant(vec![ContentBlock::text("")]),
        message,
        result_text(id),
    ]);
    assert_eq!(
        request["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "reason", "signature": "signed"},
                {"type": "tool_use", "id": id, "name": "read", "input": {"path": "a"}},
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": [{"type": "text", "text": "result"}], "is_error": false}]},
        ])
    );
    assert_eq!(
        body(vec![assistant(vec![ContentBlock::text(" ")])])["messages"][0]["content"],
        json!([{"type": "text", "text": " "}])
    );
}

#[test]
fn does_not_flush_a_system_update_at_an_omitted_empty_assistant_message() {
    let request = native(vec![
        user("hello"),
        Message::system("updated"),
        assistant(vec![ContentBlock::text("")]),
        user("hello"),
        assistant(vec![ContentBlock::text("answer")]),
    ])
    .unwrap();
    assert_eq!(
        request["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "hello"}, {"type": "text", "text": "hello"}]},
            {"role": "system", "content": [{"type": "text", "text": "updated"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "answer"}]},
        ])
    );
}

#[test]
fn rejects_unknown_content_and_tool_changes_outside_developer_messages() {
    let unknown = Message {
        content: vec![ContentBlock::Unknown(
            json!({"type": "plugin:text", "text": "opaque"}),
        )],
        ..user("")
    };
    assert_eq!(body_err(vec![unknown]).code, "UNSUPPORTED_CONTENT");
    for block in [
        ContentBlock::ToolAddition {
            tool_name: "search".into(),
        },
        ContentBlock::ToolRemoval {
            tool_name: "search".into(),
        },
    ] {
        let messages = [
            Message {
                content: vec![block.clone()],
                ..user("")
            },
            Message {
                content: vec![block.clone()],
                ..Message::system("")
            },
            assistant(vec![block.clone()]),
            result("invalid", vec![block.clone()]),
        ];
        for message in messages {
            let failure = body_err(vec![message]);
            assert_eq!(failure.code, "UNSUPPORTED_CONTENT");
        }
    }
}

#[test]
fn places_developer_tool_changes_as_a_system_update_after_the_preceding_user_turn() {
    let changes = developer(vec![
        ContentBlock::ToolAddition {
            tool_name: "search".into(),
        },
        ContentBlock::ToolRemoval {
            tool_name: "fetch".into(),
        },
        ContentBlock::text(""),
        ContentBlock::text("search replaces fetch"),
    ]);
    assert_eq!(
        body(vec![
            user("hello"),
            changes.clone(),
            assistant(vec![ContentBlock::text("ok")])
        ])["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
            {"role": "system", "content": [
                {"type": "tool_addition", "tool": {"type": "tool_reference", "name": "search"}},
                {"type": "tool_removal", "tool": {"type": "tool_reference", "name": "fetch"}},
                {"type": "text", "text": "search replaces fetch"},
            ]},
            {"role": "assistant", "content": [{"type": "text", "text": "ok"}]},
        ])
    );
    assert_eq!(
        body(vec![user("hello"), developer(vec![ContentBlock::text("")])])["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "hello"}]}])
    );
    let roles: Vec<Value> = body(vec![changes.clone(), user("hello")])["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].clone())
        .collect();
    assert_eq!(roles, vec![json!("user"), json!("system")]);
    assert_eq!(body_err(vec![changes]).code, "UNSUPPORTED_CONTENT");
    let plugin = developer(vec![ContentBlock::Unknown(
        json!({"type": "plugin:note", "text": "x"}),
    )]);
    assert_eq!(
        body_err(vec![user("hello"), plugin]).code,
        "UNSUPPORTED_CONTENT"
    );
}

#[test]
fn keeps_the_original_prompt_and_cached_prefix_while_appending_native_updates() {
    let first = vec![Message::system("original"), user("first")];
    let mut second = first.clone();
    second.extend([
        assistant(vec![ContentBlock::text("one")]),
        Message::system("updated"),
        user("second"),
    ]);
    let before = native(first).unwrap();
    let after = native(second.clone()).unwrap();
    assert_eq!(after["system"], before["system"]);
    let prefix = before["messages"].as_array().unwrap().len();
    assert_eq!(
        after["messages"].as_array().unwrap()[..prefix],
        before["messages"].as_array().unwrap()[..]
    );
    let tail: Vec<Value> = after["messages"].as_array().unwrap()[prefix + 1..].to_vec();
    assert_eq!(
        tail,
        vec![
            json!({"role": "user", "content": [{"type": "text", "text": "second"}]}),
            json!({"role": "system", "content": [{"type": "text", "text": "updated"}]})
        ]
    );
}

#[test]
fn places_system_updates_after_all_parallel_tool_results_and_before_the_next_assistant() {
    let history = vec![
        Message::system("original"),
        user("hello"),
        assistant(vec![call("a"), call("b")]),
        Message::system("first update"),
        result_text("a"),
        Message::system("second update"),
        result_text("b"),
        user("more input"),
        assistant(vec![ContentBlock::text("done")]),
    ];
    let request = native(history).unwrap();
    let roles: Vec<&str> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        ["user", "assistant", "user", "system", "system", "assistant"]
    );
    let kinds: Vec<&str> = request["messages"][2]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["tool_result", "tool_result", "text"]);
}

#[test]
fn accepts_trailing_native_updates_and_rejects_unrepresentable_positions() {
    let update = Message::system("update");
    let request = native(vec![user("hello"), update.clone()]).unwrap();
    assert_eq!(
        request["messages"][1],
        json!({"role": "system", "content": [{"type": "text", "text": "update"}]})
    );
    assert!(request.get("system").is_none());
    let err = native(vec![
        user("hello"),
        assistant(vec![ContentBlock::text("done")]),
        update.clone(),
    ])
    .unwrap_err();
    assert_eq!(
        err.message,
        "DeepSeek Messages cannot represent system update without a preceding user or tool-result turn"
    );
    assert!(
        native(vec![user("hello"), Message::system("")])
            .unwrap_err()
            .message
            .contains("empty in-history")
    );
    let err = native(vec![
        user("hello"),
        assistant(vec![call("a"), call("b")]),
        update,
        result_text("a"),
    ])
    .unwrap_err();
    assert!(err.message.contains("immediate results"), "{err:?}");
}

#[test]
fn groups_parallel_results_before_ordinary_text_and_keeps_failures() {
    let failed = Message::tool_result("b", vec![ContentBlock::text("permission denied")], true);
    let request = body(vec![
        user("hello"),
        assistant(vec![call("a"), call("b")]),
        user("follow-up"),
        result_text("a"),
        failed,
    ]);
    assert_eq!(
        request["messages"][2]["content"],
        json!([
            {"type": "tool_result", "tool_use_id": "a", "content": [{"type": "text", "text": "result"}], "is_error": false},
            {"type": "tool_result", "tool_use_id": "b", "content": [{"type": "text", "text": "permission denied"}], "is_error": true},
            {"type": "text", "text": "follow-up"},
        ])
    );
}

#[test]
fn preserves_empty_results_without_inventing_output() {
    let request = body(vec![
        user("hello"),
        assistant(vec![call("a")]),
        result("a", vec![]),
    ]);
    assert_eq!(request["messages"][2]["content"][0]["content"], json!([]));
    let minimal = Message {
        is_error: None,
        ..result("a", vec![ContentBlock::text("")])
    };
    assert_eq!(
        body(vec![assistant(vec![call("a")]), minimal])["messages"][1]["content"][0],
        json!({"type": "tool_result", "tool_use_id": "a", "content": []})
    );
}

#[test]
fn omits_assistant_only_blocks_from_user_input_and_tool_results() {
    let reasoning = ContentBlock::Reasoning {
        text: "child reasoning".into(),
    };
    let mixed = Message {
        content: vec![
            ContentBlock::text("Background subagent finished.\n"),
            reasoning.clone(),
            call("child-call"),
            ContentBlock::text("  child answer  "),
        ],
        ..user("")
    };
    assert_eq!(
        body(vec![mixed])["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "Background subagent finished.\n"}, {"type": "text", "text": "  child answer  "}]}])
    );
    let history = vec![
        assistant(vec![reasoning.clone(), call("a"), call("b")]),
        Message::tool_result(
            "a",
            vec![
                reasoning.clone(),
                call("nested"),
                ContentBlock::text("  result\n"),
            ],
            true,
        ),
        result("b", vec![reasoning.clone(), call("another")]),
    ];
    assert_eq!(
        body(history)["messages"][1]["content"],
        json!([
            {"type": "tool_result", "tool_use_id": "a", "content": [{"type": "text", "text": "  result\n"}], "is_error": true},
            {"type": "tool_result", "tool_use_id": "b", "content": [], "is_error": false},
        ])
    );
    for content in [vec![], vec![reasoning.clone()], vec![call("child-call")]] {
        let empty = Message {
            content,
            ..user("")
        };
        assert_eq!(
            body(vec![
                empty.clone(),
                user("hello"),
                assistant(vec![ContentBlock::text("answer")]),
                empty
            ])["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "hello"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "answer"}]},
            ])
        );
    }
}

#[test]
fn uses_the_latest_complete_system_snapshot_on_routes_without_updates() {
    assert_eq!(
        body(vec![user("hello"), Message::system("instructions")])["system"],
        "instructions"
    );
    let history = vec![
        Message::system("obsolete"),
        user("hello"),
        assistant(vec![call("a")]),
        Message::system("intermediate"),
        result_text("a"),
        Message::system("current"),
        user("continue"),
    ];
    assert_eq!(body(history)["system"], "current");
    let joined = Message {
        content: vec![
            ContentBlock::text("part one"),
            ContentBlock::text(" and part two"),
        ],
        ..Message::system("")
    };
    assert_eq!(
        body(vec![Message::system("old"), joined, user("hello")])["system"],
        "part one and part two"
    );
    for cleared in [vec![], vec![ContentBlock::text("")]] {
        let cleared = Message {
            content: cleared,
            ..Message::system("")
        };
        assert!(
            body(vec![Message::system("old"), user("hello"), cleared])
                .get("system")
                .is_none()
        );
    }
    let invalid = Message {
        content: vec![ContentBlock::Reasoning { text: "bad".into() }],
        ..Message::system("")
    };
    assert!(
        body_err(vec![invalid, user("hello"), Message::system("current")])
            .message
            .contains("non-text system")
    );
}

#[test]
fn maps_reasoning_effort_output_cap_and_temperature() {
    let config = DeepSeekConfig::default();
    for effort in ["off", "low", "high", "max"] {
        let mut req = request(vec![user("hello")]);
        req.reasoning_effort = Some(effort.into());
        let body = serialize_with(&config, &req).unwrap();
        assert_eq!(
            body["thinking"]["type"],
            if effort == "off" {
                "disabled"
            } else {
                "enabled"
            }
        );
        assert_eq!(
            body.get("output_config").cloned(),
            (effort != "off").then(|| json!({"effort": effort}))
        );
    }
    let mut req = request(vec![user("hello")]);
    assert_eq!(
        serialize_with(&config, &req).unwrap()["output_config"],
        json!({"effort": "high"})
    );
    req.temperature = Some(0.7);
    req.max_tokens = Some(123);
    let body = serialize_with(&config, &req).unwrap();
    assert_eq!(json::to_string(&body["temperature"]), "0.7");
    assert_eq!(body["max_tokens"], 123);
    req.reasoning_effort = Some("medium".into());
    assert_eq!(
        serialize_with(&config, &req).unwrap_err().code,
        "UNSUPPORTED_REASONING_EFFORT"
    );
    let disabled = DeepSeekConfig {
        thinking_disabled: true,
        ..Default::default()
    };
    req.reasoning_effort = None;
    assert_eq!(
        serialize_with(&disabled, &req).unwrap()["thinking"],
        json!({"type": "disabled"})
    );
    req.reasoning_effort = Some("high".into());
    assert_eq!(
        serialize_with(&disabled, &req).unwrap_err().code,
        "UNSUPPORTED_REASONING_EFFORT"
    );
    let mut capped = capable();
    capped.models.last_mut().unwrap().max_tokens = Some(321);
    assert_eq!(
        serialize_with(&capped, &request(vec![user("hello")])).unwrap()["max_tokens"],
        321
    );
    let defaulted = DeepSeekConfig {
        reasoning_effort: Some("low".into()),
        ..Default::default()
    };
    assert_eq!(
        serialize_with(&defaulted, &request(vec![user("hello")])).unwrap()["output_config"],
        json!({"effort": "low"})
    );
}

#[test]
fn rejects_unmatched_or_duplicated_tool_history() {
    let cases = vec![
        vec![result_text("a")],
        vec![assistant(vec![call("a")])],
        vec![assistant(vec![call("a")]), user("hello")],
        vec![assistant(vec![call("a"), call("a")]), result_text("a")],
        vec![
            assistant(vec![call("a")]),
            result_text("a"),
            result_text("a"),
        ],
    ];
    for messages in cases {
        let failure = body_err(messages);
        assert_eq!(failure.code, "INVALID_REQUEST");
        assert!(failure.message.contains("tool"), "{failure:?}");
    }
}

#[test]
fn sends_empty_input_for_malformed_historical_tool_arguments() {
    for raw in [
        "{",
        "",
        "[]",
        "null",
        "42",
        "true",
        "\"text\"",
        "{\"description\":\"最快，但\"某个说法\"没有证据。\"}",
    ] {
        let message = assistant(vec![ContentBlock::ToolCall {
            id: "a".into(),
            name: "read".into(),
            arguments: raw.into(),
        }]);
        let history = vec![
            user("hello"),
            message,
            Message::tool_result("a", vec![ContentBlock::text("Invalid arguments")], true),
            user("Continue"),
        ];
        assert_eq!(
            body(history)["messages"][1]["content"][0]["input"],
            json!({}),
            "{raw}"
        );
    }
}

#[test]
fn tool_input_is_written_like_json_stringify() {
    let message = assistant(vec![ContentBlock::ToolCall {
        id: "a".into(),
        name: "x".into(),
        arguments: r#"{"timeout": 1.0, "2": "b", "big": 1e21, "1": 0.5}"#.into(),
    }]);
    let wire = body(vec![user("hello"), message, result_text("a")]);
    assert_eq!(
        json::to_string(&wire["messages"][1]["content"][0]["input"]),
        r#"{"1":0.5,"2":"b","timeout":1,"big":1e+21}"#
    );
}

#[test]
fn replays_own_signatures_and_degrades_foreign_or_damaged_metadata() {
    let replay = replay_state(
        MODEL,
        &[
            dshx_llm::replay::ReplayBlock {
                kind: "reasoning",
                signature: Some("signed".into()),
            },
            dshx_llm::replay::ReplayBlock {
                kind: "text",
                signature: None,
            },
        ],
    );
    let content = vec![
        ContentBlock::Reasoning {
            text: String::new(),
        },
        ContentBlock::text("answer"),
    ];
    let message = assistant_with(content.clone(), Some(replay.clone()));
    assert_eq!(
        body(vec![user("hello"), message.clone(), user("again")])["messages"][1]["content"],
        json!([{"type": "thinking", "thinking": "", "signature": "signed"}, {"type": "text", "text": "answer"}])
    );
    // Cross-model: signatures are not portable.
    let mut other = request(vec![user("hello"), message, user("again")]);
    other.model = "deepseek-v4-pro".into();
    assert_eq!(
        serialize_with(&DeepSeekConfig::default(), &other).unwrap()["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": ""})
    );
    // Damaged metadata degrades to unsigned thinking but keeps the content.
    let damaged = assistant_with(
        content,
        Some(json!({"response": {"kind": "deepseek-messages", "version": 2}, "blocks": []})),
    );
    assert_eq!(
        body(vec![user("hello"), damaged, user("again")])["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": ""})
    );
}

// ---- projections and images through the client ----

fn image_ref(id_char: char, name: Option<&str>) -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: format!("sha256:{}", id_char.to_string().repeat(64)),
        media_type: "image/png".into(),
        bytes: 3,
        width: 1,
        height: 1,
        name: name.map(Into::into),
        original_dimensions: None,
    }
}

fn image(r: &ImageAttachmentRef, offloaded: bool) -> ContentBlock {
    ContentBlock::Image {
        attachment: r.clone(),
        offloaded,
    }
}

fn vision_client(config: DeepSeekConfig) -> (DeepSeekClient, MemoryAttachments) {
    let store = MemoryAttachments::default();
    let client = DeepSeekClient::builder(config)
        .api_key(Arc::new(StaticApiKey("sk".into())))
        .attachments(Arc::new(store.clone()))
        .image_path(Arc::new(|_: &ImageAttachmentRef| {
            Some("/workspace/image.png".to_string())
        }))
        .build()
        .unwrap();
    (client, store)
}

fn flash(messages: Vec<Message>) -> GenerateRequest {
    GenerateRequest {
        model: "deepseek-flash".into(),
        ..request(messages)
    }
}

#[tokio::test]
async fn keeps_image_bytes_inside_tool_results() {
    let (client, store) = vision_client(DeepSeekConfig::default());
    let r = image_ref('a', None);
    store.insert(&r.attachment_id, vec![1, 2, 3], "image/png");
    let history = vec![
        assistant(vec![call("a")]),
        result("a", vec![image(&r, false), image(&r, false)]),
    ];
    let wire = client.build_request(&flash(history)).await.unwrap();
    let content = &wire.body["messages"][1]["content"][0]["content"];
    assert!(
        content[0]["text"]
            .as_str()
            .unwrap()
            .contains("\"/workspace/image.png\"")
    );
    assert_eq!(
        content[1],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AQID"}})
    );
    assert_eq!(content.as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn projects_images_for_text_models_and_offloaded_images() {
    let (client, store) = vision_client(DeepSeekConfig::default());
    let r = image_ref('a', Some("shot.png"));
    store.insert(&r.attachment_id, vec![1, 2, 3], "image/png");
    let history = vec![Message {
        content: vec![ContentBlock::text("see"), image(&r, false)],
        ..user("")
    }];
    let mut pro = flash(history.clone());
    pro.model = "deepseek-v4-pro".into();
    let wire = client.build_request(&pro).await.unwrap();
    assert_eq!(
        wire.body["messages"][0]["content"][1],
        json!({"type": "text", "text": "[image omitted because this model accepts text only; attachment sha256:aaaaaaaa]"})
    );
    let offloaded = vec![Message {
        content: vec![image(&r, true)],
        ..user("")
    }];
    let wire = client.build_request(&flash(offloaded)).await.unwrap();
    let text = wire.body["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.starts_with("[image omitted to fit request image limits; \"shot.png\" (sha256:aaaa"),
        "{text}"
    );
    assert!(text.contains("\"/workspace/image.png\""));
}

#[tokio::test]
async fn rejects_unrepresentable_images_and_requires_offload_over_budget() {
    let (client, store) = vision_client(DeepSeekConfig::default());
    let r = image_ref('b', None);
    store.insert(&r.attachment_id, vec![0; 30], "image/png");
    let in_assistant = vec![user("hi"), assistant(vec![image(&r, false)])];
    assert_eq!(
        client
            .build_request(&flash(in_assistant))
            .await
            .unwrap_err()
            .code,
        "UNSUPPORTED_CONTENT"
    );
    let no_store = DeepSeekClient::builder(DeepSeekConfig::default())
        .build()
        .unwrap();
    let in_result = vec![
        assistant(vec![call("a")]),
        result("a", vec![image(&r, false)]),
    ];
    assert_eq!(
        no_store
            .build_request(&flash(in_result.clone()))
            .await
            .unwrap_err()
            .code,
        "UNSUPPORTED_CONTENT"
    );
    let missing = image_ref('c', None);
    let unknown = vec![
        assistant(vec![call("a")]),
        result("a", vec![image(&missing, false)]),
    ];
    assert_eq!(
        client
            .build_request(&flash(unknown))
            .await
            .unwrap_err()
            .code,
        "INVALID_REQUEST"
    );

    let tight = DeepSeekConfig {
        max_inline_request_image_bytes: 4,
        inline_image_offload_byte_quantum: 1,
        max_images_per_request: 2,
        image_offload_count_quantum: 1,
        ..Default::default()
    };
    let (client, store) = vision_client(tight);
    let small = image_ref('d', None);
    store.insert(&small.attachment_id, vec![1, 2, 3], "image/png");
    let two = vec![
        assistant(vec![call("a")]),
        result("a", vec![image(&small, false), image(&small, false)]),
    ];
    let failure = client.build_request(&flash(two)).await.unwrap_err();
    assert_eq!(failure.code, "IMAGE_OFFLOAD_REQUIRED");
    assert_eq!(failure.offload_images, Some(1));
    assert!(
        failure.message.contains("1 more oldest occurrence"),
        "{failure:?}"
    );
    let retry = vec![
        assistant(vec![call("a")]),
        result("a", vec![image(&small, true), image(&small, false)]),
    ];
    assert!(client.build_request(&flash(retry)).await.is_ok());
}

#[tokio::test]
async fn tool_updates_follow_the_route_mode() {
    let client = DeepSeekClient::builder(DeepSeekConfig::default())
        .build()
        .unwrap();
    let tools = vec![
        ToolSchema {
            name: "search".into(),
            description: "Search".into(),
            parameters: json!({"type": "object"}),
        },
        ToolSchema {
            name: "read".into(),
            description: "Read".into(),
            parameters: json!({"type": "object"}),
        },
    ];
    let changes = developer(vec![
        ContentBlock::ToolAddition {
            tool_name: "search".into(),
        },
        ContentBlock::ToolRemoval {
            tool_name: "fetch".into(),
        },
    ]);
    let history = vec![
        user("hello"),
        changes,
        assistant(vec![ContentBlock::text("ok")]),
        user("go"),
    ];
    let mut req = flash(history);
    req.tools = tools;
    // deepseek-flash: addition-only — removals dropped, added tool deferred, beta header needed.
    let wire = client.build_request(&req).await.unwrap();
    assert!(wire.tool_changes);
    assert_eq!(
        wire.body["messages"][1],
        json!({"role": "system", "content": [{"type": "tool_addition", "tool": {"type": "tool_reference", "name": "search"}}]})
    );
    assert_eq!(
        wire.body["tools"][0],
        json!({"name": "search", "description": "Search", "input_schema": {"type": "object"}, "defer_loading": true})
    );
    assert_eq!(
        wire.body["tools"][1],
        json!({"name": "read", "description": "Read", "input_schema": {"type": "object"}})
    );
    // deepseek-v4-pro: no tool updates — developer messages vanish, nothing deferred.
    req.model = "deepseek-v4-pro".into();
    let wire = client.build_request(&req).await.unwrap();
    assert!(!wire.tool_changes);
    assert_eq!(wire.body["messages"].as_array().unwrap().len(), 3);
    assert!(wire.body["tools"][0].get("defer_loading").is_none());
}

#[tokio::test]
async fn sends_the_tool_changes_beta_only_with_tool_changes() {
    let mock = MockDeepSeek::with_script([Reply::text("a"), Reply::text("b")]).await;
    let client = DeepSeekClient::builder(DeepSeekConfig {
        base_url: mock.base_url(),
        ..Default::default()
    })
    .api_key(Arc::new(StaticApiKey("sk".into())))
    .build()
    .unwrap();
    let mut req = flash(vec![
        user("hello"),
        developer(vec![ContentBlock::ToolAddition {
            tool_name: "search".into(),
        }]),
    ]);
    req.tools = vec![ToolSchema {
        name: "search".into(),
        description: "S".into(),
        parameters: json!({}),
    }];
    let _: Vec<StreamChunk> = client.stream(req, Default::default()).await.collect().await;
    let _: Vec<StreamChunk> = client
        .stream(flash(vec![user("hello")]), Default::default())
        .await
        .collect()
        .await;
    let requests = mock.requests();
    assert_eq!(
        requests[0].header("anthropic-beta"),
        Some(dshx_llm::TOOL_CHANGES_BETA)
    );
    assert_eq!(requests[1].header("anthropic-beta"), None);
    assert_eq!(requests[1].header("x-deepseek-harness-session-id"), None);
}

#[tokio::test]
async fn ignores_replay_metadata_of_other_providers() {
    let client = DeepSeekClient::builder(DeepSeekConfig::default())
        .build()
        .unwrap();
    let replay = replay_state(
        "deepseek-flash",
        &[dshx_llm::replay::ReplayBlock {
            kind: "reasoning",
            signature: Some("s".into()),
        }],
    );
    let mut message = assistant_with(
        vec![ContentBlock::Reasoning { text: "t".into() }],
        Some(replay),
    );
    message.source = MessageSource::Model {
        provider: "other-route".into(),
        model: "deepseek-flash".into(),
        replay_state: match message.source {
            MessageSource::Model { replay_state, .. } => replay_state,
            _ => None,
        },
    };
    let wire = client
        .build_request(&flash(vec![user("hi"), message.clone(), user("again")]))
        .await
        .unwrap();
    assert_eq!(
        wire.body["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "t"})
    );
    if let MessageSource::Model { provider, .. } = &mut message.source {
        *provider = "deepseek-official".into();
    }
    let wire = client
        .build_request(&flash(vec![user("hi"), message, user("again")]))
        .await
        .unwrap();
    assert_eq!(wire.body["messages"][1]["content"][0]["signature"], "s");
}

#[test]
fn advertises_the_dsh_model_catalog() {
    let client = DeepSeekClient::builder(DeepSeekConfig::default())
        .build()
        .unwrap();
    assert_eq!(client.provider(), "deepseek-official");
    let models = client.models();
    assert_eq!(models.len(), 2);
    assert_eq!(
        (models[0].id.as_str(), models[0].name.as_str()),
        ("deepseek-flash", "DeepSeek-V4.1-Flash")
    );
    assert!(models[0].accepts_images());
    assert_eq!(models[0].context_window, 1_000_000);
    assert_eq!(models[0].reasoning_efforts, ["off", "low", "high", "max"]);
    assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        (models[1].id.as_str(), models[1].accepts_images()),
        ("deepseek-v4-pro", false)
    );
    let flash = client.resolve_model("deepseek-flash");
    assert_eq!(
        (
            flash.system_prompt_update,
            flash.tool_update,
            flash.default_max_tokens
        ),
        (
            Some(SystemPromptUpdate::InHistory),
            Some(ToolUpdate::AdditionOnly),
            256_000
        )
    );
    let pro = client.resolve_model("deepseek-v4-pro");
    assert_eq!((pro.system_prompt_update, pro.tool_update), (None, None));
    let custom = client.resolve_model("custom");
    assert_eq!(
        (
            custom.info.input_modalities.clone(),
            custom.info.context_window
        ),
        (vec!["text".to_string()], 1_000_000)
    );
    let off_only = DeepSeekClient::builder(DeepSeekConfig {
        thinking_disabled: true,
        ..Default::default()
    })
    .build()
    .unwrap();
    assert_eq!(
        off_only
            .resolve_model("deepseek-flash")
            .info
            .reasoning_efforts,
        ["off"]
    );
    assert_eq!(
        serde_json::to_value(&flash).unwrap()["systemPromptUpdate"],
        "in-history",
        "request/context uses dsh's spelling"
    );
}
