//! Shared harness: the real DeepSeek client against the scripted mock, a
//! file-backed store in a temp dir, dsh-codex's default tools plus the
//! subagent tools, and helpers to read a parent's session family back.
#![allow(dead_code)]

pub mod scenarios;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dshx_agent::testing::{FixedApprover, MemoryAttachments};
use dshx_agent::{Agent, AgentConfig, AgentDeps, TurnOutcome};
use dshx_core::{
    ApprovalDecision, ContentBlock, LlmClient, SessionEvent, SessionHeader, SessionLog,
    ToolRegistry,
};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_session::SessionStore;
use dshx_subagent::{
    RuntimeDeps, StoreSessions, SubagentFinished, SubagentObserver, SubagentRuntime,
    SubagentStarted, subagent_tools,
};
use dshx_testkit::MockDeepSeek;
use serde_json::Value;

#[derive(Default)]
pub struct Recorder {
    pub started: Mutex<Vec<SubagentStarted>>,
    pub finished: Mutex<Vec<SubagentFinished>>,
}

impl SubagentObserver for Recorder {
    fn on_started(&self, e: &SubagentStarted) {
        self.started.lock().unwrap().push(e.clone());
    }
    fn on_finished(&self, e: &SubagentFinished) {
        self.finished.lock().unwrap().push(e.clone());
    }
}

pub struct Harness {
    pub mock: MockDeepSeek,
    pub dir: tempfile::TempDir,
    pub cwd: PathBuf,
    pub store: Arc<SessionStore>,
    pub runtime: Arc<SubagentRuntime>,
    pub agent: Agent,
    pub log: Arc<dyn SessionLog>,
    pub recorder: Arc<Recorder>,
}

pub async fn harness() -> Harness {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(scenarios::reply);
    let mut config = DeepSeekConfig {
        base_url: mock.base_url(),
        ..DeepSeekConfig::default()
    };
    config.send_session_id = false;
    let llm: Arc<dyn LlmClient> = Arc::new(
        DeepSeekClient::builder(config)
            .api_key(Arc::new(StaticApiKey("sk-mock".into())))
            .build()
            .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = Arc::new(SessionStore::new(dir.path().join("sessions")));
    let runtime = SubagentRuntime::new(RuntimeDeps {
        llm: llm.clone(),
        sessions: Arc::new(StoreSessions::new(store.clone())),
        attachments: Arc::new(MemoryAttachments::default()),
        approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
    });
    let recorder = Arc::new(Recorder::default());
    runtime.set_observer(recorder.clone());
    let log: Arc<dyn SessionLog> = Arc::new(store.create(cwd.to_str().unwrap()).unwrap());
    let mut agent_config = AgentConfig::new("deepseek-flash", &cwd);
    agent_config.instructions.harness_home = dir.path().join("home/.dsh-codex");
    agent_config.instructions.agents_home = dir.path().join("home/.agents");
    let mut tools = ToolRegistry::new();
    for tool in dshx_tools::default_tools(&dshx_core::config::Config::default()) {
        tools.register(tool);
    }
    for tool in subagent_tools(runtime.clone()) {
        tools.register(tool);
    }
    let agent = Agent::new(
        agent_config,
        AgentDeps {
            llm,
            tools,
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: None,
            tool_state: None,
        },
    );
    runtime.attach(&agent);
    Harness {
        mock,
        dir,
        cwd,
        store,
        runtime,
        agent,
        log,
        recorder,
    }
}

impl Harness {
    /// One prompt as one turn, then wait until the parent and its children are idle.
    pub async fn prompt(&self, text: &str) -> TurnOutcome {
        let outcome = self
            .agent
            .run_turn(vec![ContentBlock::text(text)])
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.runtime.wait_idle(&self.agent),
        )
        .await
        .expect("parent and children settle");
        outcome
    }

    pub fn parent_id(&self) -> String {
        self.agent.session_id()
    }

    pub fn parent_events(&self) -> Vec<SessionEvent> {
        self.agent.events()
    }

    /// A stored child session (flushed and closed once it was released).
    pub fn read(&self, id: &str) -> (SessionHeader, Vec<SessionEvent>) {
        let stored = self.store.read(id).unwrap();
        (stored.header, stored.events)
    }

    /// Child ids from the parent's catalog, in order.
    pub fn children(&self) -> Vec<String> {
        catalog_children(&self.parent_events())
    }

    /// Flushes the parent and checks every stored log with dsh's admission rules.
    pub async fn validate_all(&self) {
        self.log.flush().await.unwrap();
        let mut pending = vec![self.parent_id()];
        while let Some(id) = pending.pop() {
            let (header, events) = self.read(&id);
            dshx_session::validate::validate_log(&header, &events)
                .unwrap_or_else(|e| panic!("session {id} would not load in dsh: {e}"));
            pending.extend(catalog_children(&events));
        }
    }
}

pub fn catalog_children(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.kind == "subagent/catalog")
        .filter_map(|e| e.data["childId"].as_str().map(str::to_owned))
        .collect()
}

/// Event types without dsh-telemetry noise.
pub fn kinds(events: &[SessionEvent]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

/// Text of every tool result, in order.
pub fn tool_results(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.kind == "tool/result")
        .map(|e| {
            e.data["message"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<String>()
        })
        .collect()
}

/// User messages with the given source kind (surface or inbox insertions).
pub fn user_texts(events: &[SessionEvent], source: &str) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.kind == "user/message" && e.data["source"]["kind"] == source)
        .map(|e| {
            e.data["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

pub fn find<'a>(events: &'a [SessionEvent], kind: &str) -> Vec<&'a SessionEvent> {
    events.iter().filter(|e| e.kind == kind).collect()
}

pub fn turn_ends(events: &[SessionEvent]) -> Vec<Value> {
    find(events, "turn/end")
        .into_iter()
        .map(|e| e.data["reason"].clone())
        .collect()
}
