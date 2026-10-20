//! Scripted model behaviour for the subagent golden runs. The same responder
//! drives the official dsh (through `examples/subagent_mock.rs`) and our
//! runtime (integration tests), so both logs come from identical model output.
//!
//! The newest text block starting with `SCENARIO ` (a parent prompt) or
//! `CHILD-TASK ` (a child prompt) selects the script; the number of assistant
//! messages after it is the step. Ids the model needs are read from the
//! history: the child id from `started subagent <id>` and the parent id from
//! the continuable return guidance.
#![allow(dead_code)]

use std::time::Duration;

use dshx_testkit::{RecordedRequest, Reply};
use serde_json::{Value, json};

/// Delay of every child reply, so parent turns finish first.
pub const CHILD_DELAY: Duration = Duration::from_millis(400);

/// One flattened content block of the request history.
#[derive(Debug, Clone)]
struct Block {
    /// Index of the message holding the block.
    message: usize,
    role: String,
    value: Value,
}

fn blocks(request: &RecordedRequest) -> Vec<Block> {
    let mut out = Vec::new();
    for (message, m) in request.messages().iter().enumerate() {
        let role = m["role"].as_str().unwrap_or_default().to_owned();
        match &m["content"] {
            Value::String(s) => out.push(Block {
                message,
                role: role.clone(),
                value: json!({"type": "text", "text": s}),
            }),
            Value::Array(items) => {
                for b in items {
                    out.push(Block {
                        message,
                        role: role.clone(),
                        value: b.clone(),
                    })
                }
            }
            _ => {}
        }
    }
    out
}

fn text_of(b: &Block) -> Option<&str> {
    (b.value["type"] == "text").then(|| b.value["text"].as_str().unwrap_or_default())
}

fn tool_result_text(b: &Block) -> String {
    match &b.value["content"] {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// The script position of a request.
#[derive(Debug, Clone)]
pub struct Position {
    /// The word after `SCENARIO` / `CHILD-TASK`.
    pub task: String,
    pub child: bool,
    /// Assistant messages after the task block.
    pub step: usize,
    /// Every tool result text after the task block, in order.
    pub results: Vec<String>,
    /// Parent id from the return guidance (continuable children).
    pub parent_id: Option<String>,
}

pub fn position(request: &RecordedRequest) -> Option<Position> {
    let all = blocks(request);
    let start = all.iter().rposition(|b| {
        b.role == "user"
            && text_of(b)
                .is_some_and(|t| t.starts_with("SCENARIO ") || t.starts_with("CHILD-TASK "))
    })?;
    let text = text_of(&all[start]).unwrap_or_default();
    let child = text.starts_with("CHILD-TASK ");
    let task = text
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .trim_end_matches(':')
        .to_owned();
    let rest = &all[start + 1..];
    let first_message = all[start].message;
    let step = request
        .messages()
        .iter()
        .enumerate()
        .filter(|(i, m)| *i > first_message && m["role"] == "assistant")
        .count();
    let results = rest
        .iter()
        .filter(|b| b.value["type"] == "tool_result")
        .map(tool_result_text)
        .collect();
    let parent_id = rest.iter().filter_map(text_of).find_map(|t| {
        let rest = t.strip_prefix("Your parent agent id is \"")?;
        Some(rest.split('"').next()?.to_owned())
    });
    Some(Position {
        task,
        child,
        step,
        results,
        parent_id,
    })
}

/// The newest child id any `subagent` start result in the history reported.
fn any_started_child(request: &RecordedRequest) -> String {
    blocks(request)
        .iter()
        .filter(|b| b.value["type"] == "tool_result")
        .map(tool_result_text)
        .filter_map(|r| {
            r.strip_prefix("started subagent ")
                .map(|s| s.trim().to_owned())
        })
        .next_back()
        .unwrap_or_else(|| "missing-child".into())
}

/// The child id reported by a `subagent` start result.
fn started_child(results: &[String]) -> String {
    results
        .iter()
        .find_map(|r| r.strip_prefix("started subagent "))
        .unwrap_or("missing-child")
        .trim()
        .to_owned()
}

fn started_job(results: &[String]) -> String {
    results
        .iter()
        .find_map(|r| r.strip_prefix("started background subagent job "))
        .unwrap_or("missing-job")
        .trim()
        .to_owned()
}

fn call(id: String, name: &str, input: Value) -> Reply {
    Reply::tool_call(id, name, input)
}

fn text(t: &str) -> Reply {
    Reply::text(t)
}

/// Whether `request` is dsh's session-title request.
pub fn is_title_request(request: &RecordedRequest) -> bool {
    request
        .system()
        .is_some_and(|s| s.contains("concise title"))
}

/// The scripted reply for one request.
pub fn reply(request: &RecordedRequest) -> Reply {
    if is_title_request(request) {
        return Reply::text("Testtitel");
    }
    let Some(p) = position(request) else {
        return text("FERTIG");
    };
    let id = format!(
        "toolu_{}_{}_{:02}",
        if p.child { "c" } else { "p" },
        p.task,
        p.step + 1
    );
    if p.child {
        return delayed(child_reply(&p, id));
    }
    if p.task == "resume-send" {
        // Continue an existing child from an earlier session turn (cold resume).
        return match p.step {
            0 => call(
                id,
                "send_message",
                json!({"agent_id": any_started_child(request), "message": "One more thing."}),
            ),
            1 => call(id, "list_agents", json!({})),
            _ => text("FERTIG"),
        };
    }
    parent_reply(&p, id)
}

fn delayed(reply: Reply) -> Reply {
    match reply {
        Reply::Stream(s) => s.delay(CHILD_DELAY / 8).into(),
        other => other,
    }
}

fn parent_reply(p: &Position, id: String) -> Reply {
    match (p.task.as_str(), p.step) {
        ("warmup", _) => text("Warm."),
        // Background continuable spawn; the child reports with send_message.
        ("spawn-bg", 0) => call(
            id,
            "subagent",
            json!({"description": "Child alpha task", "prompt": "CHILD-TASK alpha: run a command and report"}),
        ),
        ("spawn-bg", 1) => text("Started the child."),
        ("spawn-bg", 2) => text("Noted the message."),
        // Foreground one-shot spawn whose child hits the depth limit.
        ("spawn-fg", 0) => call(
            id,
            "subagent",
            json!({"description": "Child beta task", "prompt": "CHILD-TASK beta: answer directly", "run_in_background": false}),
        ),
        // Foreground fork seeded with the completed warmup turn.
        ("fork-fg", 0) => call(
            id,
            "subagent_fork",
            json!({"description": "Fork review", "prompt": "CHILD-TASK fork: review the warmup"}),
        ),
        // Background fork as a job.
        ("fork-bg", 0) => call(
            id,
            "subagent_fork",
            json!({"description": "Fork background", "prompt": "CHILD-TASK forkbg: background work", "run_in_background": true}),
        ),
        ("fork-bg", 1) => text("Started the fork job."),
        ("fork-bg", 2) => call(id, "job_output", json!({"job_id": started_job(&p.results)})),
        // list_agents, interrupt_agent, then send_message cold-resumes the child.
        ("control", 0) => call(
            id,
            "subagent",
            json!({"description": "Slow child", "prompt": "CHILD-TASK slow: wait for a while"}),
        ),
        ("control", 1) => call(id, "list_agents", json!({})),
        ("control", 2) => call(id, "list_agents", json!({"scope": "descendants"})),
        ("control", 3) => Reply::stream()
            .tool_call(
                id,
                "interrupt_agent",
                json!({"agent_id": started_child(&p.results)}),
            )
            .delay(Duration::from_millis(250))
            .into(),
        ("control", 4) => text("Interrupted."),
        ("control", 5) => call(
            id,
            "send_message",
            json!({"agent_id": started_child(&p.results), "message": "Please finish now."}),
        ),
        ("control", 6) => text("Sent."),
        // A foreground child the test cancels through its parent's turn.
        ("cancel-fg", 0) => call(
            id,
            "subagent",
            json!({"description": "Slow foreground", "prompt": "CHILD-TASK slow: wait in the foreground", "run_in_background": false}),
        ),
        // A background child the host drains.
        ("drain", 0) => call(
            id,
            "subagent",
            json!({"description": "Slow child", "prompt": "CHILD-TASK slow: wait for a while"}),
        ),
        ("drain", 1) => text("Started."),
        // Refused control requests.
        ("refused", 0) => call(
            id,
            "send_message",
            json!({"agent_id": "no-such-agent", "message": "hi"}),
        ),
        ("refused", 1) => call(id, "interrupt_agent", json!({"agent_id": "no-such-agent"})),
        ("refused", 2) => call(
            id,
            "subagent_fork",
            json!({"description": "Fork job", "prompt": "CHILD-TASK fork: never runs", "run_in_background": true}),
        ),
        _ => text("FERTIG"),
    }
}

fn child_reply(p: &Position, id: String) -> Reply {
    let parent = p
        .parent_id
        .clone()
        .unwrap_or_else(|| "missing-parent".into());
    match (p.task.as_str(), p.step) {
        ("alpha", 0) => call(
            id,
            "bash",
            json!({"command": "echo child-alpha", "description": "Print a marker"}),
        ),
        ("alpha", 1) => call(
            id,
            "send_message",
            json!({"agent_id": parent, "message": "alpha result: ok"}),
        ),
        ("alpha", _) => text("CHILD ALPHA DONE"),
        ("beta", 0) => call(
            id,
            "subagent",
            json!({"description": "Grandchild", "prompt": "CHILD-TASK delta: never runs", "run_in_background": false}),
        ),
        ("beta", 1) => call(id, "list_agents", json!({})),
        ("beta", _) => text("CHILD BETA DONE"),
        ("fork", _) => text("FORK DONE"),
        ("forkbg", _) => text("FORK BG DONE"),
        ("slow", 0) => call(
            id,
            "bash",
            json!({"command": "sleep 5", "description": "Wait"}),
        ),
        ("slow", _) => text("SLOW CHILD RESUMED"),
        _ => text("CHILD DONE"),
    }
}
