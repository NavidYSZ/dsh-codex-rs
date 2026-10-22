//! Behaviour of the agent loop on a scripted model: event sequences, tool
//! scheduling, cancellation, retries, timeouts and argument errors.
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use dshx_agent::testing::{
    Block, FakeTool, Response, chunks, failure, offload_failure, text, tool_calls,
};
use dshx_agent::{AgentStatus, CancelCause, TurnEndReason};
use dshx_core::{
    ContentBlock, FinishReason, ImageAttachmentRef, StreamChunk, ToolOutput, ToolResultPart,
};
use serde_json::{Value, json};

const M: &str = "deepseek-flash";

#[tokio::test]
async fn two_step_turn_logs_dsh_event_sequence() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![
            tool_calls(M, &[("call_1", "bash", "{\"command\": \"date\"}")]),
            tool_calls(M, &[("call_2", "bash", "{\"command\": \"hostname\"}")]),
            text(M, "FERTIG"),
        ],
        vec![echo_tool("bash", false, "out\n", events)],
        |_, _| {},
    );
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("Run two commands please")])
        .await
        .unwrap();
    assert_eq!(outcome.turn, Some(1));
    assert_eq!(outcome.reason, TurnEndReason::Completed);
    let title = wait_title(&h.log).await;
    assert_eq!(title.data["title"], "Testtitel");
    assert_eq!(
        kinds(&h.log),
        [
            "permission/preset",
            "sandbox/mode",
            "approval/policy",
            "agent/inbox/spliced",
            "turn/start",
            "agent/inbox/spliced",
            "step/start",
            "system/message",
            "user/message",
            "user/message",
            "request/header",
            "request/context",
            "session/title",
            "session/title-llm-request",
            "assistant/message",
            "tool/call",
            "tool/result",
            "step/end",
            "step/start",
            "assistant/message",
            "tool/call",
            "tool/result",
            "step/end",
            "step/start",
            "assistant/message",
            "step/end",
            "turn/end",
        ]
    );
    validate(&h.log);
    // The third request sees the whole history; its messages equal dsh's derive.
    let all = events_of(&h);
    let last_assistant = all
        .iter()
        .rposition(|e| e.kind == "assistant/message")
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].messages,
        dshx_session::derive_messages(&all[..last_assistant])
    );
    assert_eq!(requests[2].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(requests[2].max_tokens, Some(256_000));
    assert_eq!(h.agent.status(), AgentStatus::Idle);
    let statuses = h.observer.statuses.lock().unwrap().clone();
    assert_eq!(statuses, vec![AgentStatus::Running, AgentStatus::Idle]);
}

fn events_of(h: &Harness) -> Vec<dshx_core::SessionEvent> {
    use dshx_core::SessionLog;
    h.log.events()
}

fn timed(tag: &str, ms: u64) -> String {
    json!({"tag": tag, "ms": ms}).to_string()
}

#[tokio::test]
async fn parallel_pool_then_exclusive_barrier_and_model_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (a, b, c, d, e) = (
        timed("a", 60),
        timed("b", 10),
        timed("c", 30),
        timed("d", 5),
        timed("e", 5),
    );
    let h = harness(
        vec![
            tool_calls(
                M,
                &[
                    ("ca", "probe", &a),
                    ("cb", "probe", &b),
                    ("cc", "probe", &c),
                    ("cd", "mutate", &d),
                    ("ce", "probe", &e),
                ],
            ),
            text(M, "done"),
        ],
        vec![
            timed_tool("probe", true, events.clone()),
            timed_tool("mutate", false, events.clone()),
        ],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let order = events.lock().unwrap().clone();
    assert_eq!(&order[..3], ["start:a", "start:b", "start:c"], "{order:?}");
    assert_eq!(&order[3..6], ["end:b", "end:c", "end:a"], "{order:?}");
    assert_eq!(
        &order[6..],
        ["start:d", "end:d", "start:e", "end:e"],
        "{order:?}"
    );
    let results: Vec<String> = of_kind(&h.log, "tool/result")
        .iter()
        .map(text_of_result)
        .collect();
    assert_eq!(results, ["done a", "done b", "done c", "done d", "done e"]);
    // Every result cites its own tool/call.
    let all = events_of(&h);
    for r in all.iter().filter(|e| e.kind == "tool/result") {
        let call = &all[r.extra["sourceEventSeqs"][0].as_u64().unwrap() as usize];
        assert_eq!(call.kind, "tool/call");
        assert_eq!(call.data["callId"], r.data["message"]["toolCallId"]);
    }
    validate(&h.log);
}

#[tokio::test]
async fn parallel_pool_is_bounded() {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (l, p) = (live.clone(), peak.clone());
    let tool = FakeTool::new(
        "probe",
        json!({"type": "object", "properties": {"tag": {"type": "string"}}}),
        true,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(move |_, _| {
            let (l, p) = (l.clone(), p.clone());
            Box::pin(async move {
                let now = l.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                l.fetch_sub(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ok"))
            })
        }),
    );
    let calls: Vec<(String, String)> = (0..12)
        .map(|i| (format!("c{i}"), json!({"tag": format!("t{i}")}).to_string()))
        .collect();
    let refs: Vec<(&str, &str, &str)> = calls
        .iter()
        .map(|(id, a)| (id.as_str(), "probe", a.as_str()))
        .collect();
    let h = harness(
        vec![tool_calls(M, &refs), text(M, "ok")],
        vec![tool],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 10);
    assert_eq!(of_kind(&h.log, "tool/result").len(), 12);
    validate(&h.log);
}

#[tokio::test]
async fn unknown_tool_invalid_json_and_schema_violations() {
    let todo = FakeTool::new(
        "todo_write",
        json!({"type": "object", "properties": {"todos": {"type": "array", "items": {"type": "object", "additionalProperties": false,
            "properties": {"content": {"type": "string"}, "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}},
            "required": ["content", "status"]}}}, "required": ["todos"]}),
        false,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(|args: Value, _| {
            Box::pin(async move {
                let n = args["todos"].as_array().map(Vec::len).unwrap_or(0);
                Ok(ToolOutput {
                    value: Some(json!({"todos": args["todos"]})),
                    ..ToolOutput::text(format!("Updated todo list: {n} items."))
                })
            })
        }),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![
            tool_calls(
                M,
                &[
                    ("c1", "nope", "{}"),
                    ("c2", "probe", "{\"tag\": "),
                    (
                        "c3",
                        "todo_write",
                        "{\"todos\": [{\"content\": \"Do it\", \"status\": \"in_progress\", \"activeForm\": \"x\"}]}",
                    ),
                    (
                        "c4",
                        "todo_write",
                        "{\"todos\": [{\"content\": \"Do it\", \"status\": \"pending\"}]}",
                    ),
                ],
            ),
            text(M, "ok"),
        ],
        vec![todo, timed_tool("probe", true, events)],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let results = of_kind(&h.log, "tool/result");
    assert_eq!(text_of_result(&results[0]), "Error: unknown tool \"nope\"");
    assert_eq!(
        results[0].data["error"],
        json!({"name": "ToolNotFoundError", "code": "UNKNOWN_TOOL"})
    );
    assert_eq!(
        text_of_result(&results[1]),
        "Error: invalid arguments: \"arguments\" must be an object"
    );
    assert_eq!(
        results[1].data["error"],
        json!({"name": "ToolArgsError", "code": "INVALID_ARGS"})
    );
    assert_eq!(
        text_of_result(&results[2]),
        "Error: invalid arguments: \"todos[0].activeForm\" is not a declared property (additionalProperties: false)"
    );
    assert_eq!(results[3].data["message"]["isError"], false);
    let todo = of_kind(&h.log, "todo/write");
    assert_eq!(todo.len(), 1);
    assert_eq!(
        todo[0].data,
        json!({"todos": [{"content": "Do it", "status": "pending"}]})
    );
    assert!(todo[0].seq < results[3].seq);
    validate(&h.log);
}

#[tokio::test]
async fn tool_timeout_and_image_results() {
    let mut slow = FakeTool::new(
        "slow",
        json!({"type": "object"}),
        false,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(|_, ctx| {
            Box::pin(async move {
                ctx.cancel.cancelled().await;
                Ok(ToolOutput::text("stopped"))
            })
        }),
    );
    Arc::get_mut(&mut slow).unwrap().timeout = Some(Duration::from_millis(30));
    let image = ImageAttachmentRef {
        attachment_id: "att-1".into(),
        media_type: "image/png".into(),
        bytes: 10,
        width: 4,
        height: 3,
        name: None,
        original_dimensions: None,
    };
    let img = image.clone();
    let shot = FakeTool::new(
        "shot",
        json!({"type": "object"}),
        false,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(move |_, _| {
            let img = img.clone();
            Box::pin(async move {
                Ok(ToolOutput {
                    content: vec![
                        ToolResultPart::Text {
                            text: "screen".into(),
                        },
                        ToolResultPart::Image { attachment: img },
                    ],
                    meta: Some(json!({"tool": "shot"})),
                    ..Default::default()
                })
            })
        }),
    );
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "slow", "{}"), ("c2", "shot", "{}")]),
            text(M, "ok"),
        ],
        vec![slow, shot],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let results = of_kind(&h.log, "tool/result");
    assert_eq!(
        text_of_result(&results[0]),
        "Error: tool call timed out after 30ms"
    );
    assert_eq!(
        results[0].data["error"],
        json!({"name": "ToolTimeoutError", "code": "TOOL_TIMEOUT"})
    );
    assert_eq!(
        results[1].data["message"]["content"][1],
        json!({"type": "image", "attachment": image})
    );
    assert_eq!(results[1].data["meta"], json!({"tool": "shot"}));
    validate(&h.log);
}

#[tokio::test]
async fn cancel_mid_stream_keeps_visible_prefix() {
    let h = harness(
        vec![
            Response::Hang(vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "reasoning",
                },
                StreamChunk::ReasoningDelta {
                    index: 0,
                    text: "thinking".into(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Reasoning {
                        text: "thinking".into(),
                    },
                },
                StreamChunk::BlockStart {
                    index: 1,
                    block_type: "text",
                },
                StreamChunk::TextDelta {
                    index: 1,
                    text: "partial".into(),
                },
                StreamChunk::BlockStart {
                    index: 2,
                    block_type: "tool-call",
                },
                StreamChunk::ToolCallDelta {
                    index: 2,
                    id: "c1".into(),
                    name: Some("bash".into()),
                    arguments_delta: "{\"co".into(),
                },
            ]),
            text(M, "after"),
        ],
        vec![],
        |_, _| {},
    );
    let agent = h.agent.clone();
    let run = tokio::spawn(async move { agent.run_turn(vec![ContentBlock::text("go")]).await });
    while h.observer.deltas.lock().unwrap().len() < 2 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    h.agent.cancel().await;
    let outcome = run.await.unwrap().unwrap();
    assert_eq!(
        outcome.reason,
        TurnEndReason::Aborted {
            reason: CancelCause::User
        }
    );
    let msg = &of_kind(&h.log, "assistant/message")[0];
    assert_eq!(msg.data["interrupted"], true);
    assert_eq!(
        msg.data["message"]["content"],
        json!([{"type": "reasoning", "text": "thinking"}, {"type": "text", "text": "partial"}])
    );
    assert!(of_kind(&h.log, "tool/call").is_empty());
    let end = of_kind(&h.log, "turn/end");
    assert_eq!(
        end[0].data["reason"],
        json!({"kind": "aborted", "reason": {"kind": "user"}})
    );
    // The next turn sees what the user saw.
    h.agent
        .run_turn(vec![ContentBlock::text("continue")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.plain_text() == "partial")
    );
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn cancel_before_visible_content_records_attempt() {
    let h = harness(
        vec![Response::Hang(vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "tool-call",
            },
            StreamChunk::ToolCallDelta {
                index: 0,
                id: "c1".into(),
                name: Some("read".into()),
                arguments_delta: "{\"pa".into(),
            },
        ])],
        vec![],
        |_, _| {},
    );
    let agent = h.agent.clone();
    let run = tokio::spawn(async move { agent.run_turn(vec![ContentBlock::text("go")]).await });
    while h.llm.request_count() == 0 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    h.agent.cancel().await;
    run.await.unwrap().unwrap();
    assert!(of_kind(&h.log, "assistant/message").is_empty());
    let attempt = &of_kind(&h.log, "assistant/attempt")[0];
    assert_eq!(attempt.data["stream"][1]["type"], "tool-call-chunks");
    validate(&h.log);
}

#[tokio::test]
async fn cancel_during_tools_settles_started_and_skips_the_rest() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (a, b, c) = (timed("a", 10_000), timed("b", 10_000), timed("c", 1));
    let h = harness(
        vec![
            tool_calls(
                M,
                &[
                    ("ca", "probe", &a),
                    ("cb", "probe", &b),
                    ("cc", "mutate", &c),
                ],
            ),
            text(M, "next"),
        ],
        vec![
            timed_tool("probe", true, events.clone()),
            timed_tool("mutate", false, events.clone()),
        ],
        |_, _| {},
    );
    let agent = h.agent.clone();
    let run = tokio::spawn(async move { agent.run_turn(vec![ContentBlock::text("go")]).await });
    while events.lock().unwrap().len() < 2 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    h.agent.cancel().await;
    let outcome = run.await.unwrap().unwrap();
    assert_eq!(
        outcome.reason,
        TurnEndReason::Aborted {
            reason: CancelCause::User
        }
    );
    let results = of_kind(&h.log, "tool/result");
    assert_eq!(results.len(), 3);
    assert_eq!(text_of_result(&results[0]), "Error: tool call aborted");
    assert_eq!(
        results[0].data["error"],
        json!({"name": "AbortError", "code": "ABORTED"})
    );
    assert_eq!(
        text_of_result(&results[2]),
        "Error: tool call aborted before dispatch"
    );
    assert_eq!(
        results[2].data["error"],
        json!({"name": "AbortError", "code": "ABORTED_BEFORE_DISPATCH"})
    );
    assert_eq!(of_kind(&h.log, "tool/call").len(), 3);
    assert!(!events.lock().unwrap().contains(&"start:c".to_owned()));
    validate(&h.log);
    // Replay stays paired: the next request carries all three results.
    h.agent
        .run_turn(vec![ContentBlock::text("again")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|m| m.role == dshx_core::Role::Tool)
            .count(),
        3
    );
}

#[tokio::test]
async fn cancel_drops_queued_prompts() {
    let h = harness(
        vec![Response::Hang(vec![]), text(M, "x")],
        vec![],
        |_, _| {},
    );
    let a1 = h.agent.clone();
    let first = tokio::spawn(async move { a1.run_turn(vec![ContentBlock::text("first")]).await });
    while h.llm.request_count() == 0 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let a2 = h.agent.clone();
    let second = tokio::spawn(async move { a2.run_turn(vec![ContentBlock::text("second")]).await });
    while of_kind(&h.log, "agent/inbox/spliced").len() < 3 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    h.agent.cancel().await;
    assert_eq!(first.await.unwrap().unwrap().turn, Some(1));
    let queued = second.await.unwrap().unwrap();
    assert_eq!(queued.turn, None);
    let splices = of_kind(&h.log, "agent/inbox/spliced");
    assert_eq!(
        splices[3].data,
        json!({"target": "next-turn", "start": 0, "removedCount": 1, "inserted": [], "outcome": "canceled"})
    );
    assert_eq!(of_kind(&h.log, "turn/start").len(), 1);
    validate(&h.log);
}

#[tokio::test(start_paused = true)]
async fn retries_follow_policy_and_log_attempts() {
    let h = harness(
        vec![
            failure("SERVER", "boom"),
            failure("RATE_LIMIT", "slow down"),
            text(M, "ok"),
        ],
        vec![],
        |c, _| {
            c.retry.random = Arc::new(|| 0.5);
        },
    );
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    assert_eq!(outcome.reason, TurnEndReason::Completed);
    let k: Vec<String> = kinds(&h.log).into_iter().skip(13).collect();
    assert_eq!(
        k,
        [
            "session/title-llm-request",
            "assistant/attempt",
            "llm/retry",
            "llm/retry-started",
            "assistant/attempt",
            "llm/retry",
            "llm/retry-started",
            "assistant/message",
            "step/end",
            "turn/end",
        ]
    );
    let retries = of_kind(&h.log, "llm/retry");
    assert_eq!(retries[0].data["delayMs"], 500);
    assert_eq!(retries[1].data["delayMs"], 1000);
    assert_eq!(retries[1].data["retry"], 2);
    assert_eq!(retries[0].data["retryId"], retries[1].data["retryId"]);
    assert_eq!(
        retries[0].data["policyKey"],
        "[\"normal\",5,[\"EMPTY_RESPONSE\",\"RATE_LIMIT\",\"SERVER\",\"TIMEOUT\",\"TRANSPORT\"],500,10000,0.1]"
    );
    assert_eq!(
        retries[1].data["failure"],
        json!({"message": "slow down", "code": "RATE_LIMIT"})
    );
    // Retries re-send the same history without re-admitting messages.
    assert_eq!(of_kind(&h.log, "user/message").len(), 2);
    assert_eq!(of_kind(&h.log, "request/header").len(), 1);
    validate(&h.log);
}

#[tokio::test(start_paused = true)]
async fn exhausted_and_non_retryable_failures_end_the_turn() {
    let script: Vec<Response> = (0..6)
        .map(|_| failure("SERVER", "down"))
        .chain([failure("AUTH", "bad key")])
        .collect();
    let h = harness(script, vec![], |_, _| {});
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    assert!(matches!(&outcome.reason, TurnEndReason::Error { error } if error.code == "SERVER"));
    assert_eq!(of_kind(&h.log, "llm/retry").len(), 5);
    assert!(matches!(h.agent.status(), AgentStatus::Error { .. }));
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("again")])
        .await
        .unwrap();
    assert!(matches!(&outcome.reason, TurnEndReason::Error { error } if error.code == "AUTH"));
    assert_eq!(of_kind(&h.log, "llm/retry").len(), 5);
    let end = of_kind(&h.log, "turn/end");
    assert_eq!(
        end[1].data["reason"],
        json!({"kind": "error", "error": {"message": "bad key", "code": "AUTH"}})
    );
    validate(&h.log);
}

#[tokio::test(start_paused = true)]
async fn cancel_during_retry_backoff() {
    let h = harness(vec![failure("SERVER", "boom")], vec![], |_, _| {});
    let agent = h.agent.clone();
    let run = tokio::spawn(async move { agent.run_turn(vec![ContentBlock::text("go")]).await });
    while of_kind(&h.log, "llm/retry").is_empty() {
        tokio::task::yield_now().await;
    }
    h.agent.cancel().await;
    let outcome = run.await.unwrap().unwrap();
    assert_eq!(
        outcome.reason,
        TurnEndReason::Aborted {
            reason: CancelCause::User
        }
    );
    assert!(of_kind(&h.log, "llm/retry-started").is_empty());
    validate(&h.log);
}

#[tokio::test]
async fn max_tokens_drops_tool_calls_and_ends_the_turn() {
    let h = harness(
        vec![Response::Chunks(chunks(
            M,
            &[
                Block::Text("cut".into()),
                Block::Tool("c1".into(), "bash".into(), "{\"co".into()),
            ],
            FinishReason::MaxTokens,
        ))],
        vec![],
        |_, _| {},
    );
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    assert_eq!(outcome.reason, TurnEndReason::MaxTokens);
    let msg = &of_kind(&h.log, "assistant/message")[0];
    assert_eq!(
        msg.data["message"]["content"],
        json!([{"type": "text", "text": "cut"}])
    );
    assert_eq!(
        msg.data["message"]["source"]["replayState"]["blocks"],
        json!([{"type": "text"}])
    );
    assert!(of_kind(&h.log, "tool/call").is_empty());
    validate(&h.log);
}

#[tokio::test]
async fn repeated_identical_calls_get_reminders() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let call = "{\"command\": \"ls\"}";
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "bash", call)]),
            tool_calls(M, &[("c2", "bash", call)]),
            tool_calls(M, &[("c3", "bash", call)]),
            text(M, "ok"),
        ],
        vec![echo_tool("bash", false, "x", events)],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let users = of_kind(&h.log, "user/message");
    let reminder = users
        .iter()
        .find(|u| u.data["source"]["kind"] == "repeat-tool-reminder")
        .expect("reminder");
    assert_eq!(reminder.data["source"]["summary"], "bash × 3");
    assert!(
        reminder.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("You are repeating the exact same tool call")
    );
    let splices = of_kind(&h.log, "agent/inbox/spliced");
    assert_eq!(splices.len(), 4);
    assert_eq!(splices[2].data["target"], "next-step");
    assert_eq!(splices[3].data["removedCount"], 1);
    validate(&h.log);
}

#[tokio::test]
async fn steer_reaches_the_next_step() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "probe", &timed("a", 80))]),
            text(M, "ok"),
        ],
        vec![timed_tool("probe", true, events.clone())],
        |_, _| {},
    );
    let agent = h.agent.clone();
    let run = tokio::spawn(async move { agent.run_turn(vec![ContentBlock::text("go")]).await });
    while events.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        h.agent
            .steer(vec![ContentBlock::text("also check b")])
            .await
            .unwrap()
            .is_none()
    );
    run.await.unwrap().unwrap();
    let requests = h.llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].messages.last().unwrap().plain_text(),
        "also check b"
    );
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn image_offload_retries_without_the_retry_budget() {
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "shot", "{}")]),
            tool_calls(M, &[("c2", "shot", "{}")]),
            offload_failure(1),
            text(M, "ok"),
        ],
        vec![image_tool("shot", image_ref("att-1"))],
        |_, _| {},
    );
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    assert_eq!(outcome.reason, TurnEndReason::Completed);
    let results = of_kind(&h.log, "tool/result");
    let offload = of_kind(&h.log, "image/offload");
    assert_eq!(
        offload.iter().map(|e| &e.data).collect::<Vec<_>>(),
        [&json!({"targets": [{"seq": results[0].seq, "imageIndexes": [0]}]})]
    );
    assert!(of_kind(&h.log, "llm/retry").is_empty());
    // The retry opens a new request series and sends the oldest image as text.
    let series = of_kind(&h.log, "request/header");
    assert!(
        series
            .iter()
            .any(|e| e.seq > offload[0].seq && e.data["reason"] == "series")
    );
    {
        let requests = h.llm.requests.lock().unwrap();
        assert_eq!(request_images(&requests[2]), [false, false]);
        assert_eq!(request_images(&requests[3]), [true, false]);
    }
    validate(&h.log);

    // A resumed agent keeps the image offloaded.
    let resumed = with_log(
        dshx_agent::testing::ScriptedLlm::new(vec![text(M, "again")]),
        vec![],
        h.dir.clone(),
        h.dir.path().join("work"),
        h.log.clone(),
        |_, _| {},
    );
    resumed
        .agent
        .run_turn(vec![ContentBlock::text("more")])
        .await
        .unwrap();
    let requests = resumed.llm.requests.lock().unwrap();
    assert_eq!(request_images(&requests[0]), [true, false]);
}

#[tokio::test]
async fn image_input_and_observer_deltas() {
    let h = harness(
        vec![Response::Chunks(chunks(
            M,
            &[
                Block::Reasoning("hmm".into(), "s".into()),
                Block::Text("hi".into()),
            ],
            FinishReason::Stop,
        ))],
        vec![],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("look")])
        .await
        .unwrap();
    let deltas = h.observer.deltas.lock().unwrap().clone();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].text, "hmm");
    assert_eq!(deltas[1].index, 1);
    let msg = &of_kind(&h.log, "assistant/message")[0];
    assert_eq!(
        msg.data["message"]["source"]["replayState"]["blocks"][0]["signature"],
        "s"
    );
}
