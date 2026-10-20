//! Prompt assembly, workspace instructions, compaction, titles, resume and
//! permission/approval facts.
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use dshx_agent::testing::{FakeTool, Response, failure, text, tool_calls};
use dshx_agent::{
    ApprovalPolicy, PromptConfig, RouteCapabilities, TurnEndReason, render_system_prompt,
};
use dshx_core::{
    ApprovalDecision, ContentBlock, SandboxMode, SessionEvent, SessionLog, TokenUsage, Tool,
    ToolContext, ToolError, ToolOutput, ToolSchema,
};
use dshx_session::MemoryLog;
use serde_json::{Value, json};

const M: &str = "deepseek-flash";
const UPSTREAM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/rust/upstream");

#[test]
fn system_prompt_matches_dsh_byte_for_byte() {
    let header: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{UPSTREAM}/request-header-0.2.0-rc.2.json")).unwrap(),
    )
    .unwrap();
    let names: Vec<String> = header["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 24);
    let expected =
        std::fs::read_to_string(format!("{UPSTREAM}/system-prompt-0.2.0-rc.2.txt")).unwrap();
    let rendered = render_system_prompt(
        &PromptConfig::default(),
        &names,
        "deepseek-official",
        M,
        "/tmp/dsh-smoke",
    )
    .unwrap();
    assert_eq!(rendered, expected);
}

#[test]
fn system_prompt_sections_follow_the_tool_set() {
    let names: Vec<String> = ["bash", "read", "write", "grep", "web_search"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let p = render_system_prompt(
        &PromptConfig::default(),
        &names,
        "deepseek-official",
        M,
        "/w",
    )
    .unwrap();
    assert!(p.contains("Read an existing file before overwriting it with write (the default fs-observation-policy requires it).\n"));
    assert!(p.contains("Use the grep tool — not shell grep or rg — to search file contents. Use read on a matched file"));
    assert!(p.contains("Use the returned source snippets when available"));
    assert!(!p.contains("job_output"));
    assert!(p.ends_with("Your working directory is /w."));
}

fn read_tool() -> Arc<FakeTool> {
    FakeTool::new(
        "read",
        json!({"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]}),
        true,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(|args: Value, _| {
            Box::pin(async move {
                let text = std::fs::read_to_string(args["file_path"].as_str().unwrap())
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
                Ok(ToolOutput::text(text))
            })
        }),
    )
}

#[tokio::test]
async fn instructions_baseline_dedup_and_nested_refresh() {
    let h = harness(vec![], vec![read_tool()], |_, _| {});
    let cwd = h.agent.config().cwd;
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    std::fs::create_dir_all(cwd.join("sub")).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "# Project rules\nUse tabs.\n").unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "# Project rules\nUse tabs.\n").unwrap();
    std::fs::write(cwd.join("AGENTS.local.md"), "local overlay\n").unwrap();
    std::fs::write(cwd.join("sub/AGENTS.md"), "Sub rules here.\n").unwrap();
    std::fs::write(cwd.join("sub/x.txt"), "x\n").unwrap();
    let home = h.dir.path().join("home/.agents");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("AGENTS.md"), "global rule\n").unwrap();
    let x = cwd.join("sub/x.txt");
    h.llm.push(tool_calls(
        M,
        &[("c1", "read", &json!({"file_path": x}).to_string())],
    ));
    h.llm.push(text(M, "done"));
    h.agent
        .run_turn(vec![ContentBlock::text("Inspect")])
        .await
        .unwrap();

    let users = of_kind(&h.log, "user/message");
    assert_eq!(users[0].data["source"]["kind"], "user");
    let baseline = &users[1].data;
    assert_eq!(
        baseline["content"][0]["text"],
        "<system-reminder>\nThe following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.\n\nInstructions from: ~/.agents/AGENTS.md\n\nglobal rule\n\n\nInstructions from: AGENTS.md\n\n# Project rules\nUse tabs.\n\n\nInstructions from: AGENTS.local.md\n\nlocal overlay\n\n</system-reminder>"
    );
    assert_eq!(baseline["source"]["baseline"], true);
    assert_eq!(
        baseline["source"]["baselineIdentity"],
        "{\"projectRoot\":\"\",\"projectRootMarkers\":[\".git\"],\"maxBytes\":65536,\"maxSourceBytes\":1048576,\"instructionFileCandidates\":[\"AGENTS.md\",\"CLAUDE.md\"],\"localInstructionFileCandidates\":[\"AGENTS.local.md\",\"CLAUDE.local.md\"]}"
    );
    assert_eq!(baseline["source"]["changes"][1]["scope"], ".\u{0}AGENTS.md");
    assert_eq!(
        baseline["source"]["changes"][0]["scope"],
        "agents-global\u{0}AGENTS.md"
    );
    assert_eq!(
        baseline["source"]["changes"][1]["digest"],
        "83c903fdce7e7429c1fbda35497f47b462cc4a8d"
    );
    assert_eq!(users[2].data["source"]["kind"], "runtime-context");

    let nested = &users[3].data;
    assert_eq!(
        nested["content"][0]["text"],
        "<system-reminder>\nAdditional instructions from: sub/AGENTS.md\n\nThese instructions apply to work under `sub`. Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.\n\nSub rules here.\n\n</system-reminder>"
    );
    assert_eq!(
        nested["source"],
        json!({"kind": "agent-instructions", "form": "instructions", "changes": [{"action": "set", "scope": "sub\u{0}AGENTS.md", "path": "sub/AGENTS.md", "digest": "f76dfbdc61a964fecf1442c3c183574851c4d54a"}]})
    );
    let k = kinds(&h.log);
    let i = k.iter().rposition(|x| x == "step/end").unwrap();
    assert_eq!(
        &k[i - 6..i - 1],
        [
            "step/end",
            "agent/inbox/spliced",
            "agent/inbox/spliced",
            "step/start",
            "user/message"
        ]
    );
    let splices = of_kind(&h.log, "agent/inbox/spliced");
    assert_eq!(splices[3].data["outcome"], "canceled");

    // An edited AGENTS.md is replaced at the next request.
    std::fs::write(cwd.join("AGENTS.md"), "# Project rules\nUse spaces.\n").unwrap();
    h.llm.push(text(M, "ok"));
    h.agent
        .run_turn(vec![ContentBlock::text("again")])
        .await
        .unwrap();
    let users = of_kind(&h.log, "user/message");
    let update = users.last().unwrap();
    assert!(
        update.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Updated instructions from: AGENTS.md")
    );
    assert_eq!(update.data["source"]["changes"][0]["action"], "replace");
    validate(&h.log);
}

/// A response whose usage claims `input` prompt tokens.
fn text_with_usage(t: &str, input: u64) -> Response {
    let mut c = dshx_agent::testing::chunks(
        M,
        &[dshx_agent::testing::Block::Text(t.into())],
        dshx_core::FinishReason::Stop,
    );
    for chunk in &mut c {
        if let dshx_core::StreamChunk::Usage(u) = chunk {
            *u = TokenUsage {
                input_tokens: input,
                output_tokens: 5,
                total_tokens: Some(input + 5),
                ..Default::default()
            };
        }
    }
    Response::Chunks(c)
}

fn small_window(c: &mut dshx_agent::AgentConfig) {
    c.route = Some(RouteCapabilities {
        context_window: Some(100_000),
        ..RouteCapabilities::for_model(M)
    });
    c.max_tokens = Some(20_000);
    c.compaction.headroom_tokens = 10_000;
    c.compaction.retain_tokens = Some(10);
}

#[tokio::test]
async fn pressure_compaction_replaces_the_oldest_span() {
    let h = harness(
        vec![text_with_usage("first answer", 70_100), text(M, "second")],
        vec![],
        |c, _| small_window(c),
    );
    let long = "please consider this long request ".repeat(120);
    h.agent
        .run_turn(vec![ContentBlock::text(long)])
        .await
        .unwrap();
    h.agent
        .run_turn(vec![ContentBlock::text("next")])
        .await
        .unwrap();
    let k = kinds(&h.log);
    let start = k
        .iter()
        .position(|x| x == "compaction/start")
        .expect("compaction ran");
    assert_eq!(
        &k[start - 2..start + 10],
        [
            "turn/start",
            "agent/inbox/spliced",
            "compaction/start",
            "compaction/summary",
            "user/message",
            "compaction/end",
            "step/start",
            "user/message",
            "request/header",
            "assistant/message",
            "step/end",
            "turn/end"
        ]
    );
    let all = h.log.events();
    let summary = &of_kind(&h.log, "compaction/summary")[0];
    assert_eq!(summary.data["shadowedRange"], json!({"start": 8, "end": 9}));
    assert_eq!(summary.data["llmStreamCall"], true);
    assert_eq!(summary.data["maxTokens"], 10_000);
    let checkpoint = &all[summary.seq as usize + 1];
    assert_eq!(
        checkpoint.extra["surfaceOp"],
        json!({"op": "replace", "startSeq": 8, "endSeq": 9})
    );
    assert_eq!(checkpoint.extra["sourceEventSeqs"][1], summary.seq);
    assert!(
        checkpoint.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("\n\n<compacted-summary>")
    );
    let header = of_kind(&h.log, "request/header");
    assert_eq!(header[1].data["reason"], "series");
    // The summarizer replayed the system prompt and the shadowed span, then the instruction.
    {
        let side = h.llm.side_requests.lock().unwrap();
        let summarize = side
            .iter()
            .find(|r| {
                r.messages
                    .last()
                    .unwrap()
                    .plain_text()
                    .contains("compaction engine")
            })
            .unwrap();
        assert_eq!(summarize.messages[0].role, dshx_core::Role::System);
        assert_eq!(summarize.messages.len(), 4);
    }
    // The second request no longer contains the shadowed prompt.
    {
        let requests = h.llm.requests.lock().unwrap();
        assert!(
            !requests[1]
                .messages
                .iter()
                .any(|m| m.plain_text().contains("long request"))
        );
        assert!(
            requests[1].messages[1]
                .plain_text()
                .starts_with("This is an automatically generated checkpoint")
        );
    }
    validate(&h.log);
}

#[tokio::test]
async fn context_overflow_compacts_once_and_retries() {
    let h = harness(
        vec![
            text(M, "first"),
            failure("CONTEXT_WINDOW_EXCEEDED", "too long"),
            text(M, "fits now"),
        ],
        vec![],
        |c, _| small_window(c),
    );
    let long = "please consider this long request ".repeat(120);
    h.agent
        .run_turn(vec![ContentBlock::text(long)])
        .await
        .unwrap();
    let outcome = h
        .agent
        .run_turn(vec![ContentBlock::text("next")])
        .await
        .unwrap();
    assert_eq!(outcome.reason, TurnEndReason::Completed);
    let k = kinds(&h.log);
    let attempt = k.iter().position(|x| x == "assistant/attempt").unwrap();
    assert_eq!(
        &k[attempt..attempt + 7],
        [
            "assistant/attempt",
            "compaction/start",
            "compaction/summary",
            "user/message",
            "compaction/end",
            "request/header",
            "assistant/message"
        ]
    );
    assert!(of_kind(&h.log, "llm/retry").is_empty());
    validate(&h.log);
}

#[tokio::test]
async fn oversized_tool_results_are_pruned_under_pressure() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let big = "y".repeat(20_000);
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "bash", "{}")]),
            text_with_usage("done", 70_100),
            text(M, "ok"),
        ],
        vec![echo_tool("bash", false, &big, events)],
        |c, _| {
            small_window(c);
            c.compaction.retain_tokens = Some(60_000);
            c.route = Some(RouteCapabilities {
                context_window: Some(2_000_000),
                ..RouteCapabilities::for_model(M)
            });
            c.compaction.threshold_ratio = 0.035;
        },
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    h.agent
        .run_turn(vec![ContentBlock::text("again")])
        .await
        .unwrap();
    let prune = of_kind(&h.log, "compaction/prune");
    assert_eq!(prune.len(), 1);
    let all = h.log.events();
    let replacement = &all[prune[0].seq as usize + 1];
    assert_eq!(replacement.kind, "tool/result");
    let text = replacement.data["message"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        text.chars().count(),
        4096 + 1024 + "\n\n[... tool result middle pruned ...]\n\n".len()
    );
    validate(&h.log);
}

#[tokio::test]
async fn title_request_runs_beside_the_first_step() {
    let h = harness(vec![text(M, "hi")], vec![], |_, _| {});
    h.agent
        .run_turn(vec![ContentBlock::text(
            "Bitte \u{1b}[31mrefactor\u{1b}[0m the parser module now please",
        )])
        .await
        .unwrap();
    let title = wait_title(&h.log).await;
    let fallback = &of_kind(&h.log, "session/title")[0];
    assert_eq!(
        fallback.data,
        json!({"title": "Bitte refactor the parser module", "messageSeqs": [8], "source": {"kind": "fallback"}})
    );
    let req = &of_kind(&h.log, "session/title-llm-request")[0];
    assert_eq!(req.data["maxTokens"], 64);
    assert_eq!(
        req.data["route"],
        json!({"provider": "deepseek-official", "model": M})
    );
    assert!(req.data["messages"][0]["content"][0]["text"].as_str().unwrap().starts_with("Generate the session title from this JSON array of human messages:\n[{\"seq\":8,\"text\":"));
    assert_eq!(
        title.data["source"]["model"],
        json!({"provider": "deepseek-official", "model": M})
    );
    {
        let side = h.llm.side_requests.lock().unwrap();
        assert_eq!(side[0].reasoning_effort.as_deref(), Some("off"));
        assert!(side[0].tools.is_empty());
    }
    // No second title for later prompts.
    h.llm.push(text(M, "again"));
    h.agent
        .run_turn(vec![ContentBlock::text("more")])
        .await
        .unwrap();
    assert_eq!(of_kind(&h.log, "session/title-llm-request").len(), 1);
    h.agent.rename("Mein Titel").await.unwrap();
    assert_eq!(
        of_kind(&h.log, "session/title").last().unwrap().data["source"]["kind"],
        "user"
    );
    validate(&h.log);
}

#[tokio::test]
async fn resume_closes_a_crashed_turn_and_logs_a_resume_header() {
    // A first agent runs one tool step, then the "process dies" mid-dispatch.
    let events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![tool_calls(M, &[("c1", "bash", "{}")]), text(M, "x")],
        vec![echo_tool("bash", false, "ok", events.clone())],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("first")])
        .await
        .unwrap();
    let mut stored = h.log.events();
    let assistant = stored
        .iter()
        .position(|e| e.kind == "assistant/message")
        .unwrap();
    stored.truncate(assistant + 2); // assistant/message + tool/call, no result
    let log = Arc::new(MemoryLog::with_events(h.log.header(), stored));
    let cwd = h.agent.config().cwd;
    let llm = dshx_agent::testing::ScriptedLlm::new(vec![text(M, "resumed")]);
    let h2 = with_log(
        llm,
        vec![echo_tool("bash", false, "ok", events)],
        h.dir.clone(),
        cwd,
        log,
        |_, _| {},
    );
    h2.agent
        .run_turn(vec![ContentBlock::text("continue")])
        .await
        .unwrap();
    let all = h2.log.events();
    let closer = &all[assistant + 2];
    assert_eq!(closer.kind, "tool/result");
    assert_eq!(closer.data["error"]["code"], "TOOL_OUTCOME_UNKNOWN");
    assert_eq!(
        closer.data["message"]["id"],
        format!("interrupted-tool-result-c1-{}", assistant + 2)
    );
    assert_eq!(all[assistant + 3].kind, "step/end");
    assert_eq!(
        all[assistant + 4].data["reason"],
        json!({"kind": "interrupted"})
    );
    let headers = of_kind(&h2.log, "request/header");
    assert_eq!(headers.last().unwrap().data["reason"], "resume");
    assert_eq!(
        of_kind(&h2.log, "turn/start").last().unwrap().data["turn"],
        2
    );
    validate(&h2.log);
}

#[tokio::test]
async fn tool_set_and_model_changes_update_the_header() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![text(M, "a"), text(M, "b"), text("deepseek-v4-pro", "c")],
        vec![echo_tool("alpha", false, "x", events.clone())],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("one")])
        .await
        .unwrap();
    let mut registry = dshx_core::ToolRegistry::new();
    registry.register(echo_tool("alpha", false, "x", events.clone()));
    registry.register(echo_tool("beta", false, "x", events));
    h.agent.set_tools(registry);
    h.agent
        .run_turn(vec![ContentBlock::text("two")])
        .await
        .unwrap();
    let headers = of_kind(&h.log, "request/header");
    assert_eq!(headers[1].data["reason"], "change");
    let dev = &of_kind(&h.log, "developer/message")[0];
    assert_eq!(
        dev.data["message"]["content"],
        json!([{"type": "tool-addition", "toolName": "beta"}])
    );
    assert_eq!(dev.data["headerSeq"], headers[1].seq);
    // A model without in-history prompt updates consolidates the prompt at the head.
    h.agent.update_config(|c| {
        c.model = "deepseek-v4-pro".into();
        c.effort = Some("max".into());
    });
    h.agent
        .run_turn(vec![ContentBlock::text("three")])
        .await
        .unwrap();
    let headers = of_kind(&h.log, "request/header");
    let last = &headers.last().unwrap().data["header"];
    assert_eq!(
        last["config"],
        json!({"provider": "deepseek-official", "model": "deepseek-v4-pro", "maxTokens": 256000, "reasoningEffort": "max"})
    );
    assert_eq!(last["adapterDefaults"], json!({"maxTokens": true}));
    let systems = of_kind(&h.log, "system/message");
    assert_eq!(systems.len(), 2);
    assert_eq!(systems[1].extra["surfaceOp"]["op"], "replace");
    let contexts = of_kind(&h.log, "request/context");
    assert_eq!(
        contexts.last().unwrap().data,
        json!({"provider": "deepseek-official", "model": "deepseek-v4-pro", "contextWindow": 1000000})
    );
    validate(&h.log);
}

struct AskingTool;

#[async_trait]
impl Tool for AskingTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "escalate".into(),
            description: "asks".into(),
            parameters: json!({"type": "object"}),
        }
    }
    async fn execute(&self, _: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let d = ctx
            .approver
            .approve(
                &ctx.call_id,
                "escalate",
                "rm -rf build",
                Some("clean build"),
            )
            .await;
        Ok(ToolOutput::text(format!("{d:?}")))
    }
}

#[tokio::test]
async fn approvals_are_audited_and_policy_never_rejects() {
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "escalate", "{}")]),
            text(M, "x"),
            tool_calls(M, &[("c2", "escalate", "{}")]),
            text(M, "y"),
        ],
        vec![Arc::new(AskingTool)],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("one")])
        .await
        .unwrap();
    let asked = &of_kind(&h.log, "approval/asked")[0];
    assert_eq!(asked.data["toolName"], "escalate");
    assert_eq!(asked.data["callId"], "c1");
    assert_eq!(asked.data["reason"], "clean build");
    assert_eq!(
        of_kind(&h.log, "approval/decided")[0].data["outcome"],
        "allowed-once"
    );
    h.agent
        .set_permissions(SandboxMode::DangerFullAccess, ApprovalPolicy::Never)
        .await
        .unwrap();
    h.agent
        .run_turn(vec![ContentBlock::text("two")])
        .await
        .unwrap();
    assert_eq!(
        of_kind(&h.log, "approval/decided")[1].data["outcome"],
        "rejected"
    );
    assert_eq!(
        of_kind(&h.log, "permission/preset").last().unwrap().data,
        json!({"preset": "danger-full-access"})
    );
    assert_eq!(
        of_kind(&h.log, "sandbox/mode").last().unwrap().data,
        json!({"mode": "danger-full-access"})
    );
    assert_eq!(
        of_kind(&h.log, "approval/policy").last().unwrap().data,
        json!({"policy": "never"})
    );
    let contexts: Vec<SessionEvent> = of_kind(&h.log, "user/message")
        .into_iter()
        .filter(|e| e.data["source"]["kind"] == "runtime-context")
        .collect();
    assert_eq!(contexts.len(), 2);
    assert!(
        contexts[1].data["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("danger-full-access")
    );
    assert_eq!(
        of_kind(&h.log, "tool/result")[1].data["message"]["content"][0]["text"],
        "Rejected"
    );
    let _ = ApprovalDecision::Rejected;
    validate(&h.log);
}
