//! One live run of the process layer against the pinned Pi with the faux
//! model (no network, no keys). Runs only with `DSHX_PI_LIVE=1` and a runtime
//! in `DSHX_PI` (e.g. `desktop/build/pi` from `desktop/fetch-pi.py`) plus
//! `DSHX_NODE` (e.g. `desktop/build/runtime/node`):
//!
//! ```sh
//! DSHX_PI_LIVE=1 DSHX_PI=$PWD/desktop/build/pi DSHX_NODE=$PWD/desktop/build/runtime/node \
//!   cargo test -p dshx-pi --test live -- --nocapture
//! ```
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, Message, SandboxMode, SessionEvent, SessionLog,
    Tool,
};
use dshx_pi::chat::Live;
use dshx_pi::{ModelChoice, PiChat, PiDirs, PiEnv, PiHost, TurnEndCompaction};
use dshx_session::{FileAttachments, SessionStore};
use serde_json::{Value, json};

struct Host {
    attachments: Arc<FileAttachments>,
    statuses: Mutex<Vec<String>>,
    deltas: Mutex<usize>,
}

struct No;

#[async_trait::async_trait]
impl Approver for No {
    async fn approve(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> ApprovalDecision {
        ApprovalDecision::Unavailable
    }
}

impl PiHost for Host {
    fn live(&self, _: &str, live: Live) {
        match live {
            Live::Status { status, .. } => self.statuses.lock().unwrap().push(status),
            Live::Delta(_) => *self.deltas.lock().unwrap() += 1,
        }
    }
    fn providers(&self) -> Vec<Value> {
        // Registers over Pi's built-in DeepSeek provider; never used here.
        vec![json!({"name": "deepseek", "apiKey": "sk-aether-live-test-not-a-key"})]
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(dshx_tools::todo::TodoWriteTool)]
    }
    fn attachments(&self) -> Arc<dyn AttachmentStore> {
        self.attachments.clone()
    }
    fn approver(&self) -> Arc<dyn Approver> {
        Arc::new(No)
    }
}

fn faux() -> ModelChoice {
    ModelChoice {
        provider: "pi-faux".into(),
        model: "faux-1".into(),
        effort: "high".into(),
        pi_provider: "pi-faux".into(),
        pi_model: "faux-1".into(),
        context_window: Some(64000),
    }
}

fn ends(events: &[SessionEvent]) -> usize {
    events.iter().filter(|e| e.kind == "turn/end").count()
}

async fn until(log: &Arc<dyn SessionLog>, turns: usize, chat: &PiChat) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while ends(&log.events()) < turns || chat.status() == "running" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for turn {turns}; status {}",
            chat.status()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn last_text(events: &[SessionEvent]) -> String {
    events
        .iter()
        .rev()
        .find(|e| e.kind == "assistant/message")
        .map(|e| {
            e.data["message"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|b| b["type"] == "text")
                .map(|b| b["text"].as_str().unwrap().to_owned())
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn tool_results(events: &[SessionEvent]) -> Vec<&SessionEvent> {
    events.iter().filter(|e| e.kind == "tool/result").collect()
}

struct Keys(Vec<Value>);

#[async_trait::async_trait]
impl dshx_pi::bridge::BridgeHost for Keys {
    fn config(&self) -> Value {
        json!({"tools": [], "providers": self.0})
    }
    fn gate(&self, _: &str, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn call(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: tokio_util::sync::CancellationToken,
    ) -> Value {
        json!({"content": [], "isError": true})
    }
}

/// Models Pi offers when the bridge hands it `providers` (no request is sent).
async fn offered(providers: Vec<Value>) -> Vec<String> {
    let home = tempfile::tempdir().unwrap();
    let dirs = PiDirs::new(home.path().join("pi"));
    let install = dshx_pi::install::resolve(home.path()).unwrap();
    let env = PiEnv::prepare(
        install,
        dirs.clone(),
        false,
        dshx_tools::sandbox::Runner::Unconfined,
        1,
    )
    .unwrap();
    dirs.create().unwrap();
    let socket = dirs.run().join("keys.sock");
    let bridge = dshx_pi::bridge::Bridge::start(&socket, Arc::new(Keys(providers))).unwrap();
    let spawn = dshx_pi::process::Spawn {
        install: env.install.clone(),
        dirs: dirs.clone(),
        cwd: home.path().to_path_buf(),
        session_id: "session-keys".into(),
        name: None,
        extensions: env.extensions.clone(),
        bridge: Some((socket, bridge.token().to_owned())),
        sandbox: SandboxMode::WorkspaceWrite,
        runner: dshx_tools::sandbox::Runner::Unconfined,
    };
    let (process, _events) = dshx_pi::process::PiProcess::spawn(&spawn).unwrap();
    let data = process
        .rpc
        .request(
            json!({"type": "get_available_models"}),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    process.shutdown().await;
    data["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            format!(
                "{}/{}",
                m["provider"].as_str().unwrap(),
                m["id"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_keys_enable_providers() {
    if std::env::var("DSHX_PI_LIVE").as_deref() != Ok("1") {
        return;
    }
    let without = offered(vec![]).await;
    assert!(
        !without.iter().any(|m| m == "deepseek/deepseek-flash"),
        "{without:?}"
    );
    let with = offered(vec![
        json!({"name": "deepseek", "apiKey": "sk-aether-live-test-not-a-key"}),
        json!({"name": "aether-zai", "baseUrl": "https://example.invalid/v1", "api": "openai-completions",
               "apiKey": "$$literal-not-env", "models": [{"id": "glm-x", "name": "GLM X", "reasoning": true,
               "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
               "contextWindow": 128000, "maxTokens": 8192}]}),
    ])
    .await;
    assert!(
        with.iter().any(|m| m == "deepseek/deepseek-flash"),
        "{with:?}"
    );
    assert!(with.iter().any(|m| m == "aether-zai/glm-x"), "{with:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pi_chat_runs_end_to_end() {
    if std::env::var("DSHX_PI_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set DSHX_PI_LIVE=1, DSHX_PI and DSHX_NODE");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path().canonicalize().unwrap();
    let install = dshx_pi::install::resolve(home.path()).expect("pinned Pi runtime");
    assert_eq!(install.version, dshx_pi::PI_VERSION);
    let dirs = PiDirs::new(home.path().join("pi"));
    let runner = dshx_tools::sandbox::host_runner();
    eprintln!("runner: {runner:?}");
    let env = Arc::new(PiEnv::prepare(install, dirs.clone(), true, runner, 2).unwrap());
    let host = Arc::new(Host {
        attachments: Arc::new(FileAttachments::new(home.path())),
        statuses: Mutex::default(),
        deltas: Mutex::default(),
    });
    let store = SessionStore::new(home.path().join("sessions"));
    let mut header = SessionStore::new_header(&cwd.display().to_string());
    header.extra.insert("engine".into(), json!("pi"));
    header
        .extra
        .insert("engineVersion".into(), json!(dshx_pi::PI_VERSION));
    let id = header.id.clone();
    let file = store.create_with_header(header).unwrap();
    file.checkpoint().await.unwrap();
    let log: Arc<dyn SessionLog> = Arc::new(file);
    let open = |log: Arc<dyn SessionLog>| {
        PiChat::open(
            &id,
            log,
            cwd.clone(),
            env.clone(),
            host.clone(),
            SandboxMode::WorkspaceWrite,
            TurnEndCompaction::Off,
        )
    };
    let chat = open(log.clone());
    let send = |text: &str| Message::user_text(text);

    // 1. A text answer streams and ends the turn.
    chat.prompt(send("hello pi"), false, faux(), None)
        .await
        .unwrap();
    until(&log, 1, &chat).await;
    assert_eq!(last_text(&log.events()), "Echo: hello pi");
    assert!(*host.deltas.lock().unwrap() > 0);

    // 2. bash sees no secrets and no bridge variables.
    chat.prompt(
        send("bash: env | grep -ciE 'KEY|TOKEN|SECRET|PASSWORD|AETHER_' || true"),
        false,
        faux(),
        None,
    )
    .await
    .unwrap();
    until(&log, 2, &chat).await;
    let events = log.events();
    let out = tool_results(&events).last().unwrap().data["message"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(out, "0", "secret-looking variables reached bash");

    // 3. todo_write runs in Aether through the bridge.
    chat.prompt(send("todo"), false, faux(), None)
        .await
        .unwrap();
    until(&log, 3, &chat).await;
    let events = log.events();
    let todo = events
        .iter()
        .find(|e| e.kind == "todo/write")
        .expect("todo/write");
    assert_eq!(todo.data["todos"][1]["status"], "in_progress");

    // 4. The write gate blocks a path outside the workspace with DSH's text.
    chat.prompt(
        send("write: /etc/aether-pi-denied.txt"),
        false,
        faux(),
        None,
    )
    .await
    .unwrap();
    until(&log, 4, &chat).await;
    let events = log.events();
    let denied = tool_results(&events).last().unwrap().data.clone();
    assert_eq!(denied["message"]["isError"], true);
    assert!(
        denied["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[sandbox: file access denied under workspace-write mode]"),
        "{denied}"
    );
    // Inside the workspace the write works.
    chat.prompt(send("write: inside.txt"), false, faux(), None)
        .await
        .unwrap();
    until(&log, 5, &chat).await;
    assert_eq!(
        std::fs::read_to_string(cwd.join("inside.txt")).unwrap(),
        "hello\n"
    );

    // 5. A steer joins the running turn.
    chat.prompt(send("bash: sleep 1.5; echo first"), false, faux(), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    chat.prompt(send("steered in"), true, faux(), None)
        .await
        .unwrap();
    until(&log, 6, &chat).await;
    let events = log.events();
    let start = events.iter().rposition(|e| e.kind == "turn/start").unwrap();
    let users: Vec<_> = events[start..]
        .iter()
        .filter(|e| e.kind == "user/message")
        .collect();
    assert_eq!(users.len(), 2, "the steer should join turn 6");
    assert_eq!(last_text(&events), "Echo: steered in");

    // 6. Stop with a held steer: it comes back to the queue, nothing continues.
    chat.prompt(send("bash: sleep 6; echo late"), false, faux(), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    chat.prompt(send("held steer"), true, faux(), None)
        .await
        .unwrap();
    chat.cancel().await.unwrap();
    until(&log, 7, &chat).await;
    let events = log.events();
    let end = events.iter().rev().find(|e| e.kind == "turn/end").unwrap();
    assert_eq!(end.data["reason"]["kind"], "aborted");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let events = log.events();
    assert_eq!(ends(&events), 7, "a stopped chat must not continue");
    let queued = dshx_pi::Mapper::resume(&cwd, &events);
    assert_eq!(queued.queued_turns().len(), 1);
    assert_eq!(
        dshx_pi::mapper::prompt_text(&queued.queued_turns()[0]),
        "held steer"
    );

    // 7. The next prompt runs the held steer first (FIFO), then itself.
    chat.prompt(send("after stop"), false, faux(), None)
        .await
        .unwrap();
    until(&log, 9, &chat).await;
    assert_eq!(last_text(&log.events()), "Echo: after stop");

    // 8. A crash closes the turn with ENGINE_EXITED; the next prompt restarts Pi.
    chat.prompt(send("bash: sleep 8"), false, faux(), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    // Pi is this test's only child (a sandboxed process hides its argv from ps).
    let ps = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
        .unwrap();
    let me = std::process::id().to_string();
    let pid: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (pid, ppid, comm) = (f.next()?, f.next()?, f.collect::<Vec<_>>().join(" "));
            // Pi names its process `pi`.
            (ppid == me && (comm == "pi" || comm.ends_with("node"))).then(|| pid.to_owned())
        })
        .collect();
    if pid.len() != 1 {
        for l in String::from_utf8_lossy(&ps.stdout).lines() {
            let mut f = l.split_whitespace();
            if f.nth(1) == Some(me.as_str()) {
                eprintln!("child: {l}");
            }
        }
    }
    assert_eq!(pid.len(), 1, "expected one Pi child process");
    for p in &pid {
        std::process::Command::new("kill")
            .args(["-9", p])
            .status()
            .unwrap();
    }
    until(&log, 10, &chat).await;
    let events = log.events();
    let end = events.iter().rev().find(|e| e.kind == "turn/end").unwrap();
    assert_eq!(
        end.data["reason"]["error"]["code"], "ENGINE_EXITED",
        "{end:?}"
    );
    assert_eq!(chat.status(), "error");
    chat.prompt(send("back again"), false, faux(), None)
        .await
        .unwrap();
    until(&log, 11, &chat).await;
    assert_eq!(last_text(&log.events()), "Echo: back again");

    // 9. Idle exit, then an Aether restart: the chat reopens from disk and
    //    Pi continues the same session file.
    env.pool.reap(Duration::ZERO).await;
    assert_eq!(env.pool.running(), 0);
    chat.close().await;
    drop(chat);
    drop(log);
    assert_eq!(dirs.session_files(&id).len(), 1);
    let reopened: Arc<dyn SessionLog> = Arc::new(store.resume(&id).await.unwrap());
    let chat = open(reopened.clone());
    chat.prompt(send("after restart"), false, faux(), None)
        .await
        .unwrap();
    until(&reopened, 12, &chat).await;
    assert_eq!(last_text(&reopened.events()), "Echo: after restart");
    assert_eq!(dirs.session_files(&id).len(), 1);
    chat.close().await;

    let stored = store.read(&id).unwrap();
    dshx_session::validate::validate_log(&stored.header, &stored.events).unwrap();
    eprintln!(
        "live run ok: {} events, statuses {:?}",
        stored.events.len(),
        host.statuses.lock().unwrap().len()
    );
}
