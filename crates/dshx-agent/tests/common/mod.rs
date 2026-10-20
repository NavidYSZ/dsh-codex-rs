#![allow(dead_code)]
//! Shared setup for the agent tests: a scripted model, fake tools and a
//! `MemoryLog`, plus helpers to inspect and validate the produced log.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dshx_agent::testing::{
    FakeTool, FixedApprover, MemoryAttachments, RecordingObserver, Response, ScriptedLlm,
};
use dshx_agent::{Agent, AgentConfig, AgentDeps};
use dshx_core::{
    ApprovalDecision, SessionEvent, SessionLog, Tool, ToolError, ToolOutput, ToolRegistry,
};
use dshx_session::MemoryLog;
use serde_json::{Value, json};

pub struct Harness {
    pub agent: Agent,
    pub llm: Arc<ScriptedLlm>,
    pub log: Arc<MemoryLog>,
    pub observer: Arc<RecordingObserver>,
    pub tool_events: Arc<Mutex<Vec<String>>>,
    pub dir: Arc<tempfile::TempDir>,
}

/// Agent over a fresh `MemoryLog` in a temp workspace; homes point into the
/// temp dir so the user's real AGENTS.md files never leak in.
pub fn harness(
    script: Vec<Response>,
    tools: Vec<Arc<dyn Tool>>,
    tweak: impl FnOnce(&mut AgentConfig, &Path),
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let log = Arc::new(MemoryLog::new(cwd.to_str().unwrap()));
    with_log(
        ScriptedLlm::new(script),
        tools,
        Arc::new(dir),
        cwd,
        log,
        tweak,
    )
}

pub fn with_log(
    llm: Arc<ScriptedLlm>,
    tools: Vec<Arc<dyn Tool>>,
    dir: Arc<tempfile::TempDir>,
    cwd: std::path::PathBuf,
    log: Arc<MemoryLog>,
    tweak: impl FnOnce(&mut AgentConfig, &Path),
) -> Harness {
    let mut registry = ToolRegistry::new();
    for t in tools {
        registry.register(t);
    }
    let mut config = AgentConfig::new("deepseek-flash", &cwd);
    config.instructions.harness_home = dir.path().join("home/.dsh-codex");
    config.instructions.agents_home = dir.path().join("home/.agents");
    tweak(&mut config, dir.path());
    let observer = Arc::new(RecordingObserver::default());
    let agent = Agent::new(
        config,
        AgentDeps {
            llm: llm.clone(),
            tools: registry,
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::AllowedOnce)),
            observer: Some(observer.clone()),
            tool_state: None,
        },
    );
    Harness {
        agent,
        llm,
        log,
        observer,
        tool_events: Arc::new(Mutex::new(Vec::new())),
        dir,
    }
}

pub fn events(log: &MemoryLog) -> Vec<SessionEvent> {
    log.events()
}

/// Event types, without the asynchronous provider title.
pub fn kinds(log: &MemoryLog) -> Vec<String> {
    log.events()
        .into_iter()
        .filter(|e| !(e.kind == "session/title" && e.data["source"]["kind"] == "provider"))
        .map(|e| e.kind)
        .collect()
}

pub fn of_kind(log: &MemoryLog, kind: &str) -> Vec<SessionEvent> {
    log.events()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

/// Would the official dsh load this log? And does our derive agree with dshx-session's?
pub fn validate(log: &MemoryLog) {
    let events = log.events();
    if let Err(e) = dshx_session::validate::validate_log(&log.header(), &events) {
        for e in &events {
            eprintln!("{}", serde_json::to_string(e).unwrap());
        }
        panic!("log rejected: {e}");
    }
}

/// Waits until the asynchronous provider title landed.
pub async fn wait_title(log: &MemoryLog) -> SessionEvent {
    for _ in 0..200 {
        if let Some(e) = log
            .events()
            .into_iter()
            .find(|e| e.kind == "session/title" && e.data["source"]["kind"] == "provider")
        {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no provider title");
}

/// A tool echoing `out` (exclusive unless `safe`).
pub fn echo_tool(
    name: &str,
    safe: bool,
    out: &str,
    events: Arc<Mutex<Vec<String>>>,
) -> Arc<FakeTool> {
    let out = out.to_owned();
    FakeTool::new(
        name,
        json!({"type": "object", "properties": {"tag": {"type": "string"}, "command": {"type": "string"}, "description": {"type": "string"}}}),
        safe,
        events,
        Arc::new(move |_, _| {
            let out = out.clone();
            Box::pin(async move { Ok(ToolOutput::text(out)) })
        }),
    )
}

/// A tool sleeping `args.ms` milliseconds (cancellable), echoing `args.tag`.
pub fn timed_tool(name: &str, safe: bool, events: Arc<Mutex<Vec<String>>>) -> Arc<FakeTool> {
    FakeTool::new(
        name,
        json!({"type": "object", "properties": {"tag": {"type": "string"}, "ms": {"type": "number"}}, "required": ["tag"]}),
        safe,
        events,
        Arc::new(|args: Value, ctx| {
            Box::pin(async move {
                let ms = args["ms"].as_u64().unwrap_or(0);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
                    _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                }
                Ok(ToolOutput::text(format!(
                    "done {}",
                    args["tag"].as_str().unwrap_or("")
                )))
            })
        }),
    )
}

pub fn text_of_result(e: &SessionEvent) -> String {
    e.data["message"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
