//! The PTC wire contract and nested tool scheduler, using the real Node worker.
mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use dshx_agent::testing::{
    FakeTool, FixedApprover, MemoryAttachments, Response, ScriptedLlm, text, tool_calls,
};
use dshx_agent::{
    Agent, AgentConfig, AgentDeps, ApprovalPolicy, CancelCause, PreStep, PreStepHook, Preset,
    TurnEndCompaction, TurnEndReason,
};
use dshx_core::{
    ApprovalDecision, ContentBlock, GenerateRequest, LlmClient, Message, ModelInfo, Role,
    SandboxMode, SessionLog, StreamChunk, Tool, ToolError, ToolOutput,
};
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::{Barrier, Notify};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "deepseek-flash";

fn program(code: &str) -> Response {
    let args = json!({"code": code, "description": "Exercise programmatic tool calls"}).to_string();
    tool_calls(MODEL, &[("program", "run_code", &args)])
}

fn ptc(config: &mut AgentConfig, _: &std::path::Path) {
    config.apply_preset(Preset::Ptc);
    // Runtime confinement itself has dedicated tests; these verify the agent contract.
    config.sandbox = SandboxMode::DangerFullAccess;
    config.title.enabled = false;
}

struct GatedSummary {
    base: Arc<ScriptedLlm>,
    started: Notify,
    release: Notify,
    first: AtomicBool,
}

#[async_trait]
impl LlmClient for GatedSummary {
    fn provider(&self) -> &str {
        self.base.provider()
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.base.models()
    }
    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        if request
            .messages
            .last()
            .is_some_and(|m| m.plain_text().contains("compaction engine"))
            && self.first.swap(false, Ordering::SeqCst)
        {
            self.started.notify_one();
            self.release.notified().await;
        }
        self.base.stream(request, cancel).await
    }
}

fn gate_summary(h: &mut Harness) -> Arc<GatedSummary> {
    let gate = Arc::new(GatedSummary {
        base: h.llm.clone(),
        started: Notify::new(),
        release: Notify::new(),
        first: AtomicBool::new(true),
    });
    h.agent = Agent::new(
        h.agent.config(),
        AgentDeps {
            llm: gate.clone(),
            tools: h.agent.tools(),
            log: h.log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::AllowedOnce)),
            observer: Some(h.observer.clone()),
            tool_state: None,
        },
    );
    gate
}

fn final_tool_text(h: &Harness) -> String {
    h.llm
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(Message::plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn model_sees_only_transport_and_direct_native_calls_are_rejected() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![
            tool_calls(MODEL, &[("native", "echo", "{}")]),
            program("return await tools.echo({})"),
            text(MODEL, "done"),
        ],
        vec![echo_tool("echo", true, "echoed", recorded.clone())],
        ptc,
    );
    h.agent
        .run_turn(vec![ContentBlock::text("Exercise PTC")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    for request in requests.iter() {
        assert_eq!(
            request
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["run_code"]
        );
        assert!(
            request
                .messages
                .iter()
                .any(|m| m.plain_text().contains("\"echo\": (args:"))
        );
    }
    drop(requests);
    assert_eq!(
        *recorded.lock().unwrap(),
        ["start:echo", "end:echo"],
        "{}",
        final_tool_text(&h)
    );
    assert_eq!(of_kind(&h.log, "tool/ptc-dispatch").len(), 1);
    assert_eq!(
        of_kind(&h.log, "tool/result")[0].data["error"]["code"],
        "UNKNOWN_TOOL"
    );
    validate(&h.log);
}

struct AddCapability {
    seen: Mutex<Vec<Vec<String>>>,
    tool: Arc<dyn Tool>,
}

#[async_trait]
impl PreStepHook for AddCapability {
    async fn pre_step(&self, step: &PreStep<'_>, _: &mut Vec<Message>) -> anyhow::Result<()> {
        self.seen.lock().unwrap().push(step.tools.to_vec());
        step.update_tools(|tools| tools.register(self.tool.clone()));
        Ok(())
    }
}

#[tokio::test]
async fn hooks_see_capabilities_and_generated_sdk_tracks_dynamic_tools_literally() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut added = Arc::try_unwrap(echo_tool(
        "mcp__test__odd-name",
        true,
        "dynamic",
        events.clone(),
    ))
    .ok()
    .unwrap();
    added.schema.description = "Literal {{not_a_prompt_variable}} */ field".into();
    let h = harness(
        vec![
            program("return await tools['mcp__test__odd-name']({})"),
            text(MODEL, "done"),
        ],
        vec![echo_tool("skill", true, "skill", events)],
        ptc,
    );
    let hook = Arc::new(AddCapability {
        seen: Mutex::new(Vec::new()),
        tool: Arc::new(added),
    });
    h.agent.add_pre_step_hook(hook.clone());
    h.agent
        .run_turn(vec![ContentBlock::text("Use the dynamic tool")])
        .await
        .unwrap();
    assert_eq!(hook.seen.lock().unwrap()[0], ["skill"]);
    let requests = h.llm.requests.lock().unwrap();
    let prompt = requests[0].messages[0].plain_text();
    assert!(prompt.contains("{{not_a_prompt_variable}}"));
    assert!(prompt.contains("\"mcp__test__odd-name\""));
    drop(requests);
    assert!(final_tool_text(&h).contains("dynamic"));
    validate(&h.log);
}

#[tokio::test]
async fn canonical_results_are_curated_and_tool_errors_are_catchable() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let data = FakeTool::new(
        "data",
        json!({"type":"object"}),
        true,
        calls.clone(),
        Arc::new(|_, _| {
            Box::pin(async {
                Ok(ToolOutput {
                    content: vec![dshx_core::ToolResultPart::Text {
                        text: "intermediate-only-result".into(),
                    }],
                    value: Some(json!({"rows":[{"id":7},{"id":9}],"omitted":"large data"})),
                    ..Default::default()
                })
            })
        }),
    );
    let fail = FakeTool::new(
        "fail",
        json!({"type":"object","properties":{"number":{"type":"integer"}},"required":["number"]}),
        false,
        calls.clone(),
        Arc::new(|_, _| Box::pin(async { Err(ToolError::Failed("intentional failure".into())) })),
    );
    let h = harness(
        vec![
            program(
                "const rows = (await tools.data({})).rows; let errors = []; for (const args of [{number:'bad'}, {number:1}]) { try { await tools.fail(args); } catch (e) { errors.push({name:e.name,tool:e.toolName,message:e.message}); } } return {id:rows[0].id,errors};",
            ),
            text(MODEL, "done"),
        ],
        vec![data, fail],
        ptc,
    );
    h.agent
        .run_turn(vec![ContentBlock::text("Filter data and catch errors")])
        .await
        .unwrap();
    let output = final_tool_text(&h);
    assert!(output.contains('7'));
    assert!(output.contains("ToolCallError"));
    assert!(output.contains("invalid arguments"));
    assert!(output.contains("intentional failure"));
    assert!(!output.contains("intermediate-only-result"));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.as_str() == "start:fail")
            .count(),
        1,
        "invalid arguments must not execute"
    );
    assert_eq!(of_kind(&h.log, "tool/ptc-dispatch").len(), 3);
    validate(&h.log);
}

#[tokio::test]
async fn nested_reads_overlap_mutations_are_barriers_and_results_commit_in_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let read = FakeTool::new(
        "read_fake",
        json!({"type":"object","properties":{"tag":{"type":"string"}},"required":["tag"]}),
        true,
        events.clone(),
        Arc::new({
            let active = active.clone();
            let barrier = barrier.clone();
            move |args, _| {
                let active = active.clone();
                let barrier = barrier.clone();
                Box::pin(async move {
                    active.fetch_add(1, Ordering::SeqCst);
                    if args["tag"] != "last" {
                        barrier.wait().await;
                    }
                    if args["tag"] == "slow" {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(ToolOutput {
                        value: Some(args),
                        ..ToolOutput::text("read")
                    })
                })
            }
        }),
    );
    let write = FakeTool::new(
        "mutate",
        json!({"type":"object"}),
        false,
        events.clone(),
        Arc::new(move |_, _| {
            let active = active.clone();
            Box::pin(async move {
                assert_eq!(active.load(Ordering::SeqCst), 0);
                Ok(ToolOutput::text("mutated"))
            })
        }),
    );
    let h = harness(
        vec![
            program(
                "return await Promise.all([tools.read_fake({tag:'slow'}), tools.read_fake({tag:'fast'}), tools.mutate({}), tools.read_fake({tag:'last'})]);",
            ),
            text(MODEL, "done"),
        ],
        vec![read, write],
        |config, path| {
            ptc(config, path);
            config.max_parallel_tool_calls = 2;
        },
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        h.agent
            .run_turn(vec![ContentBlock::text("Run independent reads")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            "start:slow",
            "start:fast",
            "end:fast",
            "end:slow",
            "start:mutate",
            "end:mutate",
            "start:last",
            "end:last"
        ]
    );
    let settled = of_kind(&h.log, "tool/ptc-dispatch");
    assert_eq!(
        settled
            .iter()
            .map(|e| e.data["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["read_fake", "read_fake", "mutate", "read_fake"]
    );
    assert_eq!(settled[0].data["arguments"]["tag"], "slow");
    validate(&h.log);
}

#[tokio::test]
async fn images_deferred_context_and_todo_events_survive_program_curation() {
    let note = FakeTool::new(
        "note",
        json!({"type":"object"}),
        true,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(|_, _| {
            Box::pin(async {
                let mut output = ToolOutput::text("hidden note value");
                output
                    .contexts
                    .push(Message::user_text("deferred instruction"));
                Ok(output)
            })
        }),
    );
    let h = harness(
        vec![
            program(
                "await tools.screen({}); await tools.note({}); await tools.todo_write({todos:[{content:'Verified',status:'completed'}]}); return 'curated';",
            ),
            text(MODEL, "done"),
        ],
        vec![
            image_tool("screen", image_ref("test-image")),
            note,
            Arc::new(dshx_tools::todo::TodoWriteTool),
        ],
        ptc,
    );
    h.agent
        .run_turn(vec![ContentBlock::text("Capture and plan")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    assert!(requests.last().unwrap().messages.iter().flat_map(|m| &m.content).any(|c| matches!(c, ContentBlock::Image { attachment, .. } if attachment.attachment_id == "test-image")));
    assert!(
        requests
            .last()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.plain_text() == "deferred instruction")
    );
    assert_eq!(
        of_kind(&h.log, "todo/write")[0].data["todos"][0]["content"],
        "Verified"
    );
    assert_eq!(of_kind(&h.log, "tool/result").len(), 1);
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn compaction_waits_for_the_complete_ptc_answer() {
    let run = |id: &str, code: &str| {
        tool_calls(
            MODEL,
            &[(
                id,
                "run_code",
                &json!({
                    "code": code, "description": "Read and return selected output",
                })
                .to_string(),
            )],
        )
    };
    let hidden = "nested-audit-only".repeat(600);
    let h = harness(
        vec![
            run("p1", "await tools.read_fake({}); return 'a'.repeat(6000);"),
            run("p2", "return 'b'.repeat(6000);"),
            text(MODEL, "done"),
            text(MODEL, "continued"),
        ],
        vec![echo_tool(
            "read_fake",
            true,
            &hidden,
            Arc::new(Mutex::new(Vec::new())),
        )],
        |config, dir| {
            ptc(config, dir);
            config.compaction.turn_end = TurnEndCompaction::EveryTurn;
        },
    );
    h.agent
        .run_turn(vec![ContentBlock::text("x".repeat(6000))])
        .await
        .unwrap();
    let log = h.agent.events();
    let starts = of_kind(&h.log, "compaction/start");
    assert_eq!(starts.len(), 1, "one summary after the whole answer");
    assert!(starts[0].data["turn"].is_null());
    let end = log.iter().find(|e| e.kind == "turn/end").unwrap();
    assert!(end.seq < starts[0].seq);
    let nested = log.iter().find(|e| e.kind == "tool/ptc-dispatch").unwrap();
    let root = log.iter().find(|e| e.kind == "tool/result").unwrap();
    assert!(nested.seq < root.seq && root.seq < starts[0].seq);
    let requests = h.llm.requests.lock().unwrap().clone();
    for (index, (id, output)) in [("p1", "a".repeat(6000)), ("p2", "b".repeat(6000))]
        .iter()
        .enumerate()
    {
        let request = &requests[index + 1];
        assert_eq!(
            request
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["run_code"]
        );
        assert!(request.messages.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolCall { id: call, .. } if call == id))
        }));
        assert!(
            request
                .messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(id) && m.plain_text().contains(output))
        );
        assert!(
            request
                .messages
                .iter()
                .all(|m| !m.plain_text().contains("<compacted-summary>"))
        );
    }
    assert!(
        requests[2].messages.iter().any(|m| {
            m.tool_call_id.as_deref() == Some("p1") && m.plain_text().contains(&"a".repeat(6000))
        }),
        "earlier tool output stays available throughout the answer"
    );
    drop(requests);
    assert!(
        h.llm
            .side_requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.messages.iter().all(|m| !m.plain_text().contains(&hidden)))
    );
    h.agent
        .run_turn(vec![ContentBlock::text("continue")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap();
    let next_turn = &requests[3];
    assert!(
        next_turn
            .messages
            .iter()
            .any(|m| m.plain_text().contains("<compacted-summary>"))
    );
    assert!(next_turn.messages.iter().any(|m| m.plain_text() == "done"));
    assert_eq!(
        of_kind(&h.log, "compaction/start").len(),
        1,
        "a tiny follow-up needs no new checkpoint"
    );
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn steering_and_fifo_followups_survive_a_running_summary() {
    let mut h = harness(
        vec![
            program("return 'selected';"),
            text(MODEL, "work done"),
            text(MODEL, "Follow-up processed"),
            text(MODEL, "Follow-up processed"),
            text(MODEL, "Follow-up processed"),
        ],
        vec![],
        |c, dir| {
            ptc(c, dir);
            c.compaction.turn_end = TurnEndCompaction::EveryTurn;
        },
    );
    let gate = gate_summary(&mut h);
    let first = {
        let agent = h.agent.clone();
        tokio::spawn(async move {
            agent
                .run_turn(vec![ContentBlock::text("x".repeat(6000))])
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.started.notified())
        .await
        .unwrap();
    let steering = Message::user_text("accepted steering");
    let a = Message::user_text("follow-up A");
    let b = Message::user_text("follow-up B");
    h.agent.followup(a.clone()).await.unwrap();
    h.agent.followup(b.clone()).await.unwrap();
    h.agent.steer_detached(steering.clone()).await.unwrap();
    for message in [&steering, &a, &b] {
        assert!(h.agent.inbox_contains(&message.id));
    }
    assert!(of_kind(&h.log, "compaction/end").is_empty());
    gate.release.notify_one();
    assert_eq!(first.await.unwrap().reason, TurnEndReason::Completed);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if h.agent.is_idle() && h.agent.inbox_is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let users: Vec<_> = h
        .agent
        .events()
        .into_iter()
        .filter(|e| e.kind == "user/message" && e.data["source"]["kind"] == "user")
        .collect();
    assert_eq!(
        users[1..]
            .iter()
            .map(|e| e.data["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [&steering.id, &a.id, &b.id]
    );
    let requests = h.llm.requests.lock().unwrap();
    assert!(requests[2].messages.iter().any(|m| m.id == steering.id));
    let first_request = |id: &str| {
        requests
            .iter()
            .position(|r| r.messages.iter().any(|m| m.id == id))
            .unwrap()
    };
    assert!(first_request(&steering.id) <= first_request(&a.id));
    assert!(
        first_request(&a.id) < first_request(&b.id),
        "queued prompts remain FIFO even when steering shares the first resumed step"
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|m| m.plain_text() == "work done")
    );
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn parent_cancel_interrupts_turn_end_summary_and_clears_followups() {
    let mut h = harness(
        vec![
            program("return 'settled';"),
            text(MODEL, "done"),
            program("throw new Error('must not run');"),
        ],
        vec![],
        |c, dir| {
            ptc(c, dir);
            c.compaction.turn_end = TurnEndCompaction::EveryTurn;
        },
    );
    let gate = gate_summary(&mut h);
    let run = {
        let agent = h.agent.clone();
        tokio::spawn(async move {
            agent
                .run_turn(vec![ContentBlock::text("x".repeat(6000))])
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.started.notified())
        .await
        .unwrap();
    h.agent
        .followup(Message::user_text("must not run"))
        .await
        .unwrap();
    h.agent.cancel_with(CancelCause::Parent, false).await;
    let done = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        done.reason,
        TurnEndReason::Completed,
        "the answer was already committed before its summary"
    );
    assert_eq!(h.llm.request_count(), 2);
    assert!(h.agent.inbox_is_empty());
    assert!(of_kind(&h.log, "compaction/summary").is_empty());
    assert_eq!(of_kind(&h.log, "tool/result").len(), 1);
    validate(&h.log);
}

#[tokio::test]
async fn complete_ptc_answers_honor_threshold_and_off() {
    for (policy, expected) in [
        (TurnEndCompaction::Threshold(1_000), 1),
        (TurnEndCompaction::Threshold(200_000), 0),
        (TurnEndCompaction::Off, 0),
    ] {
        let h = harness(
            vec![
                program("return 'selected '.repeat(700);"),
                text(MODEL, "done"),
            ],
            vec![],
            |c, dir| {
                ptc(c, dir);
                c.compaction.turn_end = policy;
            },
        );
        h.agent
            .run_turn(vec![ContentBlock::text("x".repeat(6000))])
            .await
            .unwrap();
        let starts = of_kind(&h.log, "compaction/start");
        assert_eq!(starts.len(), expected, "{policy:?}");
        if let Some(start) = starts.first() {
            assert!(start.data["turn"].is_null());
            assert!(of_kind(&h.log, "turn/end")[0].seq < start.seq);
        }
        assert!(
            h.llm.requests.lock().unwrap()[1]
                .messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("program"))
        );
        validate(&h.log);
    }
}

#[tokio::test]
async fn one_shot_hosts_skip_proactive_compaction_throughout_the_answer() {
    for preset in [Preset::Ptc, Preset::Standard] {
        let script = if preset == Preset::Ptc {
            vec![
                program("return 'settled '.repeat(750);"),
                text(MODEL, "done"),
            ]
        } else {
            vec![text(MODEL, "done")]
        };
        let h = harness(script, vec![], |c, dir| {
            ptc(c, dir);
            c.apply_preset(preset);
            c.compaction.turn_end = TurnEndCompaction::EveryTurn;
            c.compaction.at_turn_end = false;
        });
        h.agent
            .run_turn(vec![ContentBlock::text("x".repeat(40_000))])
            .await
            .unwrap();
        let starts = of_kind(&h.log, "compaction/start");
        assert!(
            starts.is_empty(),
            "one-shot {preset:?} has no intermediate or final checkpoint"
        );
        let events = h.agent.events();
        let end = events.iter().rposition(|e| e.kind == "turn/end").unwrap();
        assert!(
            !events[end + 1..]
                .iter()
                .any(|e| e.kind.starts_with("compaction/"))
        );
        validate(&h.log);
    }
}

#[tokio::test]
async fn delegated_ptc_work_does_not_summarize_after_each_program() {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let cwd = dir.path().to_path_buf();
    let mut header = dshx_session::SessionStore::new_header(cwd.to_str().unwrap());
    header.delegation_depth = 1;
    let log = Arc::new(dshx_session::MemoryLog::with_events(header, vec![]));
    let llm = ScriptedLlm::new(vec![
        tool_calls(
            MODEL,
            &[(
                "p1",
                "run_code",
                &json!({"code":"return 'a'.repeat(6000);", "description":"First step"}).to_string(),
            )],
        ),
        tool_calls(
            MODEL,
            &[(
                "p2",
                "run_code",
                &json!({"code":"return 'b'.repeat(6000);", "description":"Second step"})
                    .to_string(),
            )],
        ),
        text(MODEL, "delegated work done"),
    ]);
    let h = with_log(llm, vec![], dir, cwd, log, |c, dir| {
        ptc(c, dir);
        c.compaction.turn_end = TurnEndCompaction::EveryTurn;
    });
    h.agent
        .run_turn(vec![ContentBlock::text("x".repeat(6000))])
        .await
        .unwrap();
    assert!(of_kind(&h.log, "compaction/start").is_empty());
    assert_eq!(of_kind(&h.log, "tool/result").len(), 2);
    assert_eq!(h.log.header().delegation_depth, 1);
    assert_eq!(h.llm.request_count(), 3);
    let requests = h.llm.requests.lock().unwrap();
    for id in ["p1", "p2"] {
        assert!(
            requests[2]
                .messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(id))
        );
    }
    drop(requests);
    validate(&h.log);
}

#[tokio::test]
async fn nested_approvals_obey_never_and_are_audited() {
    let guarded = FakeTool::new(
        "guarded",
        json!({"type":"object"}),
        false,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(|_, ctx| {
            Box::pin(async move {
                match ctx
                    .approver
                    .approve(
                        &ctx.call_id,
                        "guarded",
                        "Test approval",
                        Some("Needed for test"),
                    )
                    .await
                {
                    ApprovalDecision::AllowedOnce => Ok(ToolOutput::text("allowed")),
                    _ => Err(ToolError::Failed(
                        "denied by standing approval policy".into(),
                    )),
                }
            })
        }),
    );
    let h = harness(
        vec![
            program("try { return await tools.guarded({}); } catch(e) { return e.message; }"),
            text(MODEL, "done"),
        ],
        vec![guarded],
        |config, path| {
            ptc(config, path);
            config.approval = ApprovalPolicy::Never;
        },
    );
    h.agent
        .run_turn(vec![ContentBlock::text("Exercise approval")])
        .await
        .unwrap();
    assert!(final_tool_text(&h).contains("denied by standing approval policy"));
    assert_eq!(of_kind(&h.log, "approval/asked").len(), 1);
    assert_eq!(
        of_kind(&h.log, "approval/decided")[0].data["outcome"],
        "rejected"
    );
    validate(&h.log);
}

#[tokio::test]
async fn permission_changes_cancel_running_program_and_settle_nested_calls() {
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let wait = FakeTool::new(
        "wait_test",
        json!({"type":"object"}),
        true,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new({
            let started = started.clone();
            let cancelled = cancelled.clone();
            move |_, ctx| {
                let started = started.clone();
                let cancelled = cancelled.clone();
                Box::pin(async move {
                    started.notify_one();
                    ctx.cancel.cancelled().await;
                    cancelled.store(true, Ordering::SeqCst);
                    Err(ToolError::Cancelled)
                })
            }
        }),
    );
    let h = harness(
        vec![
            program("await tools.wait_test({}); return 'late-success';"),
            text(MODEL, "done"),
        ],
        vec![wait],
        ptc,
    );
    let agent = h.agent.clone();
    let run = tokio::spawn(async move {
        agent
            .run_turn(vec![ContentBlock::text("Wait for cancellation")])
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    h.agent
        .set_permissions(SandboxMode::ReadOnly, ApprovalPolicy::Never)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(of_kind(&h.log, "tool/ptc-dispatch-start").len(), 1);
    assert_eq!(of_kind(&h.log, "tool/ptc-dispatch").len(), 1);
    assert_eq!(
        of_kind(&h.log, "tool/ptc-dispatch")[0].data["isError"],
        true
    );
    assert!(!final_tool_text(&h).contains("late-success"));
    validate(&h.log);
}
