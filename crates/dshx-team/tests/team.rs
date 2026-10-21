//! Agent Teams end to end: the real DeepSeek client against the scripted
//! mock, a file-backed store, the subagent runtime for teammates and the
//! Team tools in place of the subagent tools.
use std::sync::Arc;
use std::time::Duration;

use dshx_agent::testing::{FixedApprover, MemoryAttachments, ScriptedLlm};
use dshx_agent::{Agent, AgentConfig, AgentDeps, AgentStatus};
use dshx_core::{
    ApprovalDecision, ContentBlock, LlmClient, SessionEvent, SessionLog, ToolRegistry,
};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_session::{MemoryLog, SessionStore};
use dshx_subagent::{MemorySessions, RuntimeDeps, StoreSessions, SubagentRuntime};
use dshx_team::{TaskUpdate, TeamService, TeamSettings, team_tools};
use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
use serde_json::{Value, json};

/// Tool calls of the history as `(name, input, result text)`.
fn calls(r: &RecordedRequest) -> Vec<(String, Value, String)> {
    let mut out: Vec<(String, String, Value)> = Vec::new();
    let mut results = std::collections::HashMap::new();
    for m in r.messages() {
        for b in m["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_use" {
                out.push((
                    b["id"].as_str().unwrap().to_owned(),
                    b["name"].as_str().unwrap().to_owned(),
                    b["input"].clone(),
                ));
            }
            if b["type"] == "tool_result" {
                let t = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect(),
                    _ => String::new(),
                };
                results.insert(b["tool_use_id"].as_str().unwrap().to_owned(), t);
            }
        }
    }
    out.into_iter()
        .map(|(id, name, input)| (name, input, results.remove(&id).unwrap_or_default()))
        .collect()
}

fn script(r: &RecordedRequest) -> Reply {
    if r.system().is_some_and(|s| s.contains("concise title")) {
        return Reply::text("Titel");
    }
    let history = r.body["messages"].to_string();
    let done = calls(r);
    let has = |n: &str| done.iter().filter(|(name, _, _)| name == n).count();
    let id = format!("toolu_{}", done.len());
    if history.contains("You are teammate \\\"alice\\\"") {
        if has("team_task_list") == 0 {
            return Reply::tool_call(id, "team_task_list", json!({}));
        }
        let list: Value =
            serde_json::from_str(&done.iter().find(|c| c.0 == "team_task_list").unwrap().2)
                .unwrap();
        let task = &list["tasks"][0];
        return match has("team_task_update") {
            0 => Reply::tool_call(
                id,
                "team_task_update",
                json!({"task_id": task["id"], "expected_revision": task["revision"], "action": "claim"}),
            ),
            1 => {
                let claimed: Value = serde_json::from_str(
                    &done.iter().find(|c| c.0 == "team_task_update").unwrap().2,
                )
                .unwrap();
                Reply::tool_call(
                    id,
                    "team_task_update",
                    json!({"task_id": claimed["id"], "expected_revision": claimed["revision"], "action": "complete"}),
                )
            }
            _ if has("send_message") == 0 => Reply::tool_call(
                id,
                "send_message",
                json!({"target": "lead", "message": "alice finished"}),
            ),
            _ => Reply::text("alice done"),
        };
    }
    if has("team_task_create") == 0 {
        return Reply::tool_call(
            id,
            "team_task_create",
            json!({"subject": "Write notes", "description": "Write NOTES.md", "write_scopes": ["./docs/", "docs"]}),
        );
    }
    if has("spawn_teammate") == 0 {
        return Reply::tool_call(
            id,
            "spawn_teammate",
            json!({"name": "alice", "description": "Writes notes", "prompt": "ALICE-TASK write the notes"}),
        );
    }
    if has("send_message") == 0 {
        return Reply::tool_call(
            id,
            "send_message",
            json!({"target": "alice", "message": "hello alice"}),
        );
    }
    if has("list_agents") == 0 {
        return Reply::tool_call(id, "list_agents", json!({}));
    }
    Reply::text("lead done")
}

struct Harness {
    _mock: MockDeepSeek,
    _dir: tempfile::TempDir,
    store: Arc<SessionStore>,
    runtime: Arc<SubagentRuntime>,
    teams: Arc<TeamService>,
    lead: Agent,
    log: Arc<dyn SessionLog>,
}

async fn harness() -> Harness {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(script);
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
    let sessions = Arc::new(StoreSessions::new(store.clone()));
    let runtime = SubagentRuntime::new(RuntimeDeps {
        llm: llm.clone(),
        sessions: sessions.clone(),
        attachments: Arc::new(MemoryAttachments::default()),
        approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
    });
    let teams = TeamService::new(TeamSettings::default(), runtime.clone(), sessions);
    let hook = teams.clone();
    runtime.set_child_agent(Arc::new(move |child: &Agent| hook.on_child(child)));
    let log: Arc<dyn SessionLog> = Arc::new(store.create(cwd.to_str().unwrap()).unwrap());
    let mut c = AgentConfig::new("deepseek-flash", &cwd);
    c.instructions.harness_home = dir.path().join("home/.dsh-codex");
    c.instructions.agents_home = dir.path().join("home/.agents");
    let mut tools = ToolRegistry::new();
    for t in dshx_tools::default_tools(&dshx_core::config::Config::default()) {
        tools.register(t);
    }
    for t in team_tools(&teams) {
        tools.register(t);
    }
    let lead = Agent::new(
        c,
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
    runtime.attach(&lead);
    teams.attach_lead(&lead);
    Harness {
        _mock: mock,
        _dir: dir,
        store,
        runtime,
        teams,
        lead,
        log,
    }
}

fn of<'a>(events: &'a [SessionEvent], kind: &str) -> Vec<&'a SessionEvent> {
    events.iter().filter(|e| e.kind == kind).collect()
}

fn result_value(events: &[SessionEvent], name: &str) -> Value {
    let call = events
        .iter()
        .find(|e| e.kind == "tool/call" && e.data["name"] == name)
        .unwrap_or_else(|| panic!("no {name} call"));
    let id = &call.data["callId"];
    let result = events
        .iter()
        .find(|e| e.kind == "tool/result" && e.data["message"]["toolCallId"] == *id)
        .unwrap();
    let text = result.data["message"]["content"][0]["text"]
        .as_str()
        .unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_and_a_teammate_share_tasks_and_messages() {
    let h = harness().await;
    h.lead
        .run_turn(vec![ContentBlock::text("Use Agent Teams: spawn alice")])
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let events = h.log.events();
        let delivered = of(&events, "team/message/delivered").len();
        let thanked = events
            .iter()
            .filter(|e| e.kind == "user/message" && e.data["source"]["kind"] == "team-message")
            .count();
        if delivered == 2
            && thanked == 1
            && h.lead.status() == AgentStatus::Idle
            && h.lead.is_idle()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    h.runtime.wait_idle(&h.lead).await;
    let events = h.log.events();
    let lead_id = h.lead.session_id();

    // Roster: provisioning, then active, both v2 Lead-log facts.
    let members = of(&events, "team/member");
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].data["version"], 2);
    assert_eq!(members[0].data["teamId"], lead_id.as_str());
    assert_eq!(members[0].data["member"]["phase"], "provisioning");
    assert_eq!(members[1].data["member"]["phase"], "active");
    assert_eq!(members[1].data["member"]["name"], "alice");
    let alice = members[1].data["member"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        result_value(&events, "spawn_teammate"),
        json!({"member": {"target": "alice", "role": "teammate", "status": result_value(&events, "spawn_teammate")["member"]["status"], "description": "Writes notes", "provider": "spawn", "context": "fresh", "model": "deepseek-flash", "diagnostics": []}})
    );

    // Task board: created by the Lead, claimed and completed by alice.
    let tasks = of(&events, "team/task");
    assert_eq!(tasks.len(), 3);
    assert_eq!(
        tasks[0].data["task"],
        json!({"id": "task-1", "revision": 1, "subject": "Write notes", "description": "Write NOTES.md", "status": "pending", "blockedBy": [], "writeScopes": ["docs"]})
    );
    let claim = &tasks[1].data["task"];
    assert_eq!(claim["status"], "in_progress");
    assert_eq!(claim["ownerId"], alice.as_str());
    let keys: Vec<&str> = claim
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys.last(),
        Some(&"ownerId"),
        "a newly set owner comes last like dsh's spread"
    );
    assert_eq!(tasks[2].data["task"]["status"], "completed");
    assert_eq!(tasks[2].data["task"]["revision"], 3);
    let created = result_value(&events, "team_task_create");
    assert_eq!(created["ready"], true);
    assert_eq!(created["writeScopeWarnings"], json!([]));

    // Mailbox: both directions queued and acknowledged.
    let queued = of(&events, "team/message/queued");
    assert_eq!(queued.len(), 2);
    let to_alice = &queued[0].data["message"];
    assert_eq!(to_alice["senderName"], "lead");
    assert_eq!(to_alice["targetId"], alice.as_str());
    assert_eq!(
        to_alice["content"],
        json!([{"type": "text", "text": "hello alice"}])
    );
    let send = result_value(&events, "send_message");
    assert_eq!(send["messageId"], to_alice["id"]);
    let delivered: Vec<&str> = of(&events, "team/message/delivered")
        .iter()
        .map(|e| e.data["messageId"].as_str().unwrap())
        .collect();
    assert!(delivered.contains(&to_alice["id"].as_str().unwrap()));
    let from_alice = events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "team-message")
        .unwrap();
    assert_eq!(from_alice.data["source"]["senderName"], "alice");
    assert_eq!(from_alice.data["source"]["teamId"], lead_id.as_str());
    assert_eq!(from_alice.data["content"][1]["text"], "alice finished");
    assert!(
        from_alice.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Team message team-message-")
    );
    assert!(
        from_alice.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(" from alice:")
    );

    // The teammate's own session.
    let stored = h.store.read(&alice).unwrap();
    assert_eq!(stored.header.extra["parentSession"], lead_id.as_str());
    let descriptor = stored
        .events
        .iter()
        .find(|e| e.kind == "subagent/descriptor")
        .unwrap();
    assert_eq!(descriptor.data["mode"], "continuable");
    assert_eq!(descriptor.data["provider"], "spawn");
    assert_eq!(descriptor.data["label"], "Writes notes");
    let prompt = stored
        .events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "user")
        .unwrap();
    let content = prompt.data["content"].as_array().unwrap();
    assert_eq!(
        content.len(),
        2,
        "no subagent return guidance for teammates"
    );
    assert_eq!(
        content[0]["text"],
        "<system-reminder>\nYou are teammate \"alice\".\nYour Team Lead is named \"lead\".\nUse list_agents({}) to find your teammates and their names.\nTo message your Team Lead, use send_message({ target: \"lead\", message: \"...\" }).\nTo message another teammate, use send_message({ target: \"<teammate name>\", message: \"...\" }).\n</system-reminder>\n\n"
    );
    assert_eq!(content[1]["text"], "ALICE-TASK write the notes");
    let got = stored
        .events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "team-message")
        .unwrap();
    assert_eq!(got.data["source"]["senderName"], "lead");
    assert_eq!(got.data["content"][1]["text"], "hello alice");

    // Roster from list_agents and the UI view.
    let roster = result_value(&events, "list_agents");
    assert_eq!(roster[0]["target"], "lead");
    assert_eq!(roster[0]["role"], "lead");
    assert_eq!(roster[1]["target"], "alice");
    let view = h.teams.team_view(&lead_id).unwrap().unwrap();
    assert_eq!(view["tasks"][0]["ownerName"], "alice");
    assert_eq!(view["tasks"][0]["status"], "completed");
    assert_eq!(view["members"][1]["phase"], "active");
    assert_eq!(
        dshx_team::stored_team(&lead_id, &events)["tasks"],
        view["tasks"]
    );
}

#[tokio::test]
async fn the_task_board_enforces_dsh_transitions() {
    let sessions = Arc::new(MemorySessions::new());
    let runtime = SubagentRuntime::new(RuntimeDeps {
        llm: ScriptedLlm::new(vec![]),
        sessions: sessions.clone(),
        attachments: Arc::new(MemoryAttachments::default()),
        approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
    });
    let teams = TeamService::new(TeamSettings::default(), runtime.clone(), sessions);
    let log = Arc::new(MemoryLog::new("/w"));
    let lead = Agent::new(
        AgentConfig::new("deepseek-flash", "/w"),
        AgentDeps {
            llm: ScriptedLlm::new(vec![]),
            tools: ToolRegistry::new(),
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: None,
            tool_state: None,
        },
    );
    runtime.attach(&lead);
    teams.attach_lead(&lead);
    let m = teams.membership(&lead.session_id()).unwrap();
    assert!(m.is_lead);
    let t1 = teams
        .create_task(&m, "One", "first", None, None)
        .await
        .unwrap();
    let t2 = teams
        .create_task(
            &m,
            "Two",
            "second",
            Some(vec!["task-1".into()]),
            Some(vec!["src/a".into()]),
        )
        .await
        .unwrap();
    assert_eq!(
        (t1["id"].as_str(), t2["id"].as_str()),
        (Some("task-1"), Some("task-2"))
    );
    assert_eq!(t2["ready"], false);
    let code = |r: Result<Value, dshx_team::TeamError>| r.unwrap_err().code.unwrap();
    let update = |id: &str, rev: i64, action: &str| TaskUpdate {
        task_id: id.into(),
        expected_revision: rev,
        action: action.into(),
        ..Default::default()
    };
    assert_eq!(
        code(teams.update_task(&m, update("task-2", 1, "claim")).await),
        "TEAM_TASK_BLOCKED"
    );
    assert_eq!(
        code(teams.update_task(&m, update("task-1", 7, "claim")).await),
        "TEAM_TASK_STALE_REVISION"
    );
    let cycle = TaskUpdate {
        blocked_by: Some(vec!["task-2".into()]),
        ..update("task-1", 1, "set_dependencies")
    };
    assert_eq!(
        code(teams.update_task(&m, cycle).await),
        "TEAM_TASK_DEPENDENCY_CYCLE"
    );
    assert_eq!(
        code(teams.update_task(&m, update("task-1", 1, "delete")).await),
        "TEAM_TASK_HAS_DEPENDENTS"
    );
    let claimed = teams
        .update_task(&m, update("task-1", 1, "claim"))
        .await
        .unwrap();
    assert_eq!(claimed["ownerName"], "lead");
    assert_eq!(claimed["status"], "in_progress");
    let done = teams
        .update_task(&m, update("task-1", 2, "complete"))
        .await
        .unwrap();
    assert_eq!(done["status"], "completed");
    let list = teams.list_tasks(&m).unwrap();
    assert_eq!(
        list[1]["ready"], true,
        "task-2 is ready once its blocker completed"
    );
    assert_eq!(
        code(teams.create_task(&m, "  ", "x", None, None).await),
        "TEAM_INVALID_ARGUMENT"
    );
    assert_eq!(
        teams
            .create_task(&m, "x", "y", None, Some(vec!["../out".into()]))
            .await
            .unwrap_err()
            .message,
        "invalid workspace-relative write scope \"../out\""
    );
    assert_eq!(
        code(teams.send(&m, "lead", "self").await),
        "TEAM_SELF_MESSAGE"
    );
    assert_eq!(
        code(teams.send(&m, "bob", "hi").await),
        "TEAM_MEMBER_NOT_FOUND"
    );
    let wait = teams
        .wait(&m, 10_000, &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(wait["noProgress"]["reason"], "no-active-peer");
    let events = log.events();
    assert_eq!(events.iter().filter(|e| e.kind == "team/task").count(), 4);
}
