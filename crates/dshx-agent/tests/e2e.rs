//! End to end: the real `DeepSeekClient` against the scripted `MockDeepSeek`
//! server, persisted through `FileLog` and resumed through `SessionStore`.
mod common;

use std::sync::{Arc, Mutex};

use common::echo_tool;
use dshx_agent::testing::{FixedApprover, MemoryAttachments, RecordingObserver};
use dshx_agent::{Agent, AgentConfig, AgentDeps, TurnEndReason};
use dshx_core::{ApprovalDecision, ContentBlock, LlmClient, SessionLog, ToolRegistry};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_session::SessionStore;
use dshx_testkit::{MockDeepSeek, Reply, scenarios::is_title_request};
use serde_json::json;

fn agent(
    llm: Arc<dyn LlmClient>,
    log: Arc<dyn SessionLog>,
    cwd: &std::path::Path,
    home: &std::path::Path,
) -> Agent {
    let mut config = AgentConfig::new("deepseek-flash", cwd);
    config.instructions.harness_home = home.join(".dsh-codex");
    config.instructions.agents_home = home.join(".agents");
    let mut tools = ToolRegistry::new();
    tools.register(echo_tool(
        "bash",
        false,
        "host-1\n",
        Arc::new(Mutex::new(Vec::new())),
    ));
    Agent::new(
        config,
        AgentDeps {
            llm,
            tools,
            log,
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: Some(Arc::new(RecordingObserver::default())),
            tool_state: None,
        },
    )
}

#[tokio::test]
async fn real_client_mock_server_file_log_and_resume() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|request| {
        if is_title_request(request) {
            return Reply::text("Hostname prüfen");
        }
        match request.tool_results() {
            0 => Reply::stream()
                .thinking("Run hostname.", "sig-1")
                .tool_call("toolu_1", "bash", json!({"command": "hostname"}))
                .into(),
            _ => Reply::text("It is host-1."),
        }
    });
    let mut config = DeepSeekConfig {
        base_url: mock.base_url(),
        ..DeepSeekConfig::default()
    };
    config.send_session_id = false;
    let client: Arc<dyn LlmClient> = Arc::new(
        DeepSeekClient::builder(config)
            .api_key(Arc::new(StaticApiKey("sk-mock".into())))
            .build()
            .unwrap(),
    );

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = SessionStore::new(dir.path().join("sessions"));
    let log = Arc::new(store.create(cwd.to_str().unwrap()).unwrap());
    let id = log.id().to_owned();
    let a = agent(client.clone(), log.clone(), &cwd, dir.path());
    let outcome = a
        .run_turn(vec![ContentBlock::text("Which host is this?")])
        .await
        .unwrap();
    assert_eq!(outcome.reason, TurnEndReason::Completed);
    for _ in 0..200 {
        if log
            .events()
            .iter()
            .any(|e| e.kind == "session/title" && e.data["source"]["kind"] == "provider")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    log.flush().await.unwrap();
    drop(a);
    drop(log);

    // The wire: system prompt from the tool set, thinking replay, title request.
    let requests = mock.requests();
    let main: Vec<_> = requests.iter().filter(|r| !is_title_request(r)).collect();
    assert_eq!(main.len(), 2);
    let system = main[0].system().unwrap();
    assert!(system.starts_with("You are an AI agent powered by DeepSeek Harness.\n\nYou are a coding agent powered by the deepseek-flash model.\n\nCheck the [exit code: N] marker"));
    assert!(system.ends_with(&format!("Your working directory is {}.", cwd.display())));
    assert_eq!(main[0].body["output_config"]["effort"], "high");
    assert_eq!(main[0].body["tools"][0]["name"], "bash");
    let replayed = main[1].messages()[1]["content"][0].clone();
    assert_eq!(replayed["type"], "thinking");
    assert_eq!(replayed["signature"], "sig-1");
    let title = requests.iter().find(|r| is_title_request(r)).unwrap();
    assert_eq!(title.body["max_tokens"], 64);
    assert_eq!(title.body["thinking"]["type"], "disabled");

    // What landed on disk is a log the official dsh accepts.
    let stored = store.read(&id).unwrap();
    dshx_session::validate::validate_log(&stored.header, &stored.events).unwrap();
    let kinds: Vec<&str> = stored.events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"tool/result"));
    assert_eq!(
        stored
            .events
            .iter()
            .rfind(|e| e.kind == "session/title")
            .unwrap()
            .data["title"],
        "Hostname prüfen"
    );

    // A new process resumes it.
    let log = Arc::new(store.resume(&id).await.unwrap());
    let b = agent(client, log.clone(), &cwd, dir.path());
    let outcome = b
        .run_turn(vec![ContentBlock::text("And again?")])
        .await
        .unwrap();
    assert_eq!(outcome.turn, Some(2));
    log.flush().await.unwrap();
    let stored = store.read(&id).unwrap();
    dshx_session::validate::validate_log(&stored.header, &stored.events).unwrap();
    let headers: Vec<_> = stored
        .events
        .iter()
        .filter(|e| e.kind == "request/header")
        .collect();
    assert_eq!(headers.last().unwrap().data["reason"], "resume");
    assert!(stored.events.iter().any(|e| e.kind == "session/end-seed"));
    let last = mock
        .requests()
        .into_iter()
        .rfind(|r| !is_title_request(r))
        .unwrap();
    assert_eq!(last.messages().len(), 5);
}

/// Writes a session with tools, a retry and an overflow compaction into
/// `$DSHX_INTEROP_ROOT` (run manually; the official dsh then resumes it).
#[tokio::test(start_paused = true)]
#[ignore]
async fn write_interop_session() {
    use dshx_agent::testing::{ScriptedLlm, failure, text, tool_calls};
    let root =
        std::path::PathBuf::from(std::env::var("DSHX_INTEROP_ROOT").expect("DSHX_INTEROP_ROOT"));
    let cwd =
        std::path::PathBuf::from(std::env::var("DSHX_INTEROP_CWD").expect("DSHX_INTEROP_CWD"));
    let m = "deepseek-flash";
    let llm = ScriptedLlm::new(vec![
        tool_calls(m, &[("toolu_a1", "bash", "{\"command\": \"hostname\"}")]),
        failure("SERVER", "boom"),
        text(m, "It is host-1."),
        failure("CONTEXT_WINDOW_EXCEEDED", "maximum context length exceeded"),
        text(m, "Compacted and done."),
    ]);
    let store = SessionStore::new(root);
    let log = Arc::new(store.create(cwd.to_str().unwrap()).unwrap());
    let a = agent(llm, log.clone(), &cwd, &cwd.join("no-home"));
    let long = "Please consider this long request about the parser. ".repeat(60);
    a.run_turn(vec![ContentBlock::text(long)]).await.unwrap();
    a.run_turn(vec![ContentBlock::text("next")]).await.unwrap();
    log.flush().await.unwrap();
    println!("SESSION {}", log.id());
}
