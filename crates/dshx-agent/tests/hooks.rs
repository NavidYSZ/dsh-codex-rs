//! Pre-step hooks (placement after instructions and runtime context, order,
//! history access), literal prompt sections and MCP argument pass-through.
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use dshx_agent::testing::{FakeTool, text, tool_calls};
use dshx_agent::{ExtraSection, PreStep, PreStepHook, PromptConfig, render_system_prompt};
use dshx_core::{ContentBlock, Message, MessageSource, Role, ToolOutput};
use serde_json::{Value, json};

const M: &str = "deepseek-flash";

/// Appends one marker message per step that claimed input, and records what it saw.
struct Marker {
    tag: &'static str,
    seen: Mutex<Vec<Value>>,
}

#[async_trait]
impl PreStepHook for Marker {
    async fn pre_step(
        &self,
        step: &PreStep<'_>,
        entering: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        let visible_markers = step.read_history(|events, visible| {
            events
                .iter()
                .filter(|e| e.data["source"]["kind"] == self.tag && visible(e.seq))
                .count()
        });
        self.seen.lock().unwrap().push(json!({
            "claimed": step.claimed.iter().map(Message::plain_text).collect::<Vec<_>>(),
            "entering": entering.iter().map(|m| m.source.clone()).collect::<Vec<_>>(),
            "tools": step.tools,
            "cwd": step.cwd,
            "visible_markers": visible_markers,
        }));
        if !step.claimed.is_empty() {
            entering.push(Message {
                id: dshx_core::new_id(),
                role: Role::User,
                content: vec![ContentBlock::text(format!("marker {}", self.tag))],
                source: MessageSource::Other(json!({"kind": self.tag})),
                tool_call_id: None,
                is_error: None,
                extra: Default::default(),
            });
        }
        Ok(())
    }
}

fn source_kind(m: &Message) -> String {
    match &m.source {
        MessageSource::User => "user".into(),
        MessageSource::SystemPrompt => "system-prompt".into(),
        MessageSource::Other(v) => v["kind"].as_str().unwrap_or_default().to_owned(),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn hooks_follow_instructions_and_runtime_context_in_order() {
    let tool_events = Arc::new(Mutex::new(Vec::new()));
    let h = harness(
        vec![tool_calls(M, &[("c1", "echo", "{}")]), text(M, "done")],
        vec![echo_tool("echo", false, "ok", tool_events)],
        |config, _| {
            std::fs::create_dir_all(config.cwd.join(".git")).unwrap();
            std::fs::write(config.cwd.join("AGENTS.md"), "Be nice.\n").unwrap();
        },
    );
    let a = Arc::new(Marker {
        tag: "test-a",
        seen: Mutex::new(Vec::new()),
    });
    let b = Arc::new(Marker {
        tag: "test-b",
        seen: Mutex::new(Vec::new()),
    });
    h.agent.add_pre_step_hook(a.clone());
    h.agent.add_pre_step_hook(b.clone());
    h.agent
        .run_turn(vec![ContentBlock::text("hi")])
        .await
        .unwrap();

    let requests = h.llm.requests.lock().unwrap().clone();
    let first: Vec<String> = requests[0].messages.iter().map(source_kind).collect();
    assert_eq!(first[1], "user");
    assert!(requests[0].messages[2].plain_text().contains("Be nice."));
    assert_eq!(&first[3..], ["runtime-context", "test-a", "test-b"]);

    // Hook b saw a's message; both saw the claimed prompt and the tool set.
    let a_seen = a.seen.lock().unwrap().clone();
    let b_seen = b.seen.lock().unwrap().clone();
    assert_eq!(a_seen[0]["claimed"], json!(["hi"]));
    assert_eq!(a_seen[0]["tools"], json!(["echo"]));
    assert_eq!(a_seen[0]["cwd"], json!(h.dir.path().join("work")));
    assert_eq!(
        b_seen[0]["entering"].as_array().unwrap().last().unwrap()["kind"],
        "test-a"
    );
    // Step 2 claims nothing; the history shows step 1's marker on the surface.
    assert_eq!(a_seen.len(), 2);
    assert_eq!(a_seen[1]["claimed"], json!([]));
    assert_eq!(a_seen[1]["visible_markers"], 1);
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|m| source_kind(m) == "test-a")
            .count(),
        1
    );

    // The markers are logged like the agent's own context messages.
    let logged: Vec<String> = of_kind(&h.log, "user/message")
        .iter()
        .map(|e| {
            e.data["source"]["kind"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        &logged[logged.len() - 3..],
        ["runtime-context", "test-a", "test-b"]
    );
    validate(&h.log);
}

#[test]
fn literal_sections_skip_interpolation() {
    let config = PromptConfig {
        extra_sections: vec![
            ExtraSection {
                name: "mcp:t".into(),
                order: 3100,
                text: "### MCP server: t\n\nUse {{model}} and {{unknown}} as written.".into(),
                literal: true,
            },
            ExtraSection {
                name: "product".into(),
                order: 3200,
                text: "Model: {{model}}".into(),
                literal: false,
            },
        ],
        ..Default::default()
    };
    let p = render_system_prompt(&config, &[], "deepseek-official", M, "/w").unwrap();
    assert!(p.contains("### MCP server: t\n\nUse {{model}} and {{unknown}} as written.\n\nModel: deepseek-flash\n\nYour working directory is /w."), "{p}");
}

#[tokio::test]
async fn mcp_tool_arguments_reach_the_server_unvalidated() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let schema =
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]});
    let echo = |name: &str| {
        FakeTool::new(
            name,
            schema.clone(),
            false,
            events.clone(),
            Arc::new(|args: Value, _| {
                Box::pin(async move { Ok(ToolOutput::text(args.to_string())) })
            }),
        )
    };
    let h = harness(
        vec![
            tool_calls(M, &[("c1", "mcp__srv__echo", "{}"), ("c2", "strict", "{}")]),
            text(M, "done"),
        ],
        vec![echo("mcp__srv__echo"), echo("strict")],
        |_, _| {},
    );
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let results = of_kind(&h.log, "tool/result");
    assert_eq!(text_of_result(&results[0]), "{}");
    assert_eq!(results[0].data["message"]["isError"], false);
    assert_eq!(
        text_of_result(&results[1]),
        "Error: invalid arguments: missing required property \"text\""
    );
}
