//! `dsh-codex web`: branching a chat after a turn (`POST /api/sessions/{id}/fork`).
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, Reply};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

struct Server {
    child: Child,
    base: String,
    token: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(mock: &MockDeepSeek, home: &std::path::Path) -> Server {
    std::fs::write(
        home.join("config.toml"),
        "[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n",
    )
    .unwrap();
    let mut child = Command::new(BIN)
        .args(["web", "--port", "0", "--no-open"])
        .env("DSHX_HOME", home)
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .env_remove("DSH_HOME")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut server = Server {
        child,
        base: String::new(),
        token: String::new(),
    };
    let mut line = String::new();
    loop {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "web exited");
        if let Some(url) = line.trim().strip_prefix("dsh-codex web: ") {
            let (base, token) = url.split_once("/?token=").unwrap();
            server.base = base.to_owned();
            server.token = token.to_owned();
            std::thread::spawn(move || for _ in stderr.lines() {});
            return server;
        }
    }
}

impl Server {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = reqwest::Client::new()
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {v}");
        v
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    /// Sends a prompt and waits for the `turn/end` of `turn`.
    async fn run_turn(&self, id: &str, text: &str, turn: u64) -> Vec<Value> {
        let (status, v) = self
            .post(&format!("/sessions/{id}/prompt"), json!({ "text": text }))
            .await;
        assert_eq!(status, 202, "{v}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let s = self.get(&format!("/sessions/{id}")).await;
            let events = s["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["type"] == "turn/end" && e["data"]["turn"] == turn)
            {
                return events;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no turn/end {turn}: {s}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn seq_of_turn_end(events: &[Value], turn: u64) -> usize {
    events
        .iter()
        .position(|e| e["type"] == "turn/end" && e["data"]["turn"] == turn)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn branches_copy_the_log_through_a_turn_and_run_on_their_own() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r| {
        if r.system().is_some_and(|s| s.contains("concise title")) {
            return Reply::text("Zwei Fragen");
        }
        Reply::stream().text("Done.").usage(1000, 200).into()
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());

    let (_, created) = server
        .post(
            "/sessions",
            json!({ "cwd": ws.path(), "preset": "minimal" }),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    server.run_turn(&id, "first", 1).await;
    let source = server.run_turn(&id, "second", 2).await;
    let end = seq_of_turn_end(&source, 1);

    // After turn 1: its events, keeping seq and time, then the inherited cut.
    let (status, fork) = server
        .post(
            &format!("/sessions/{id}/fork"),
            json!({ "turn": 1, "title": "Erste Frage (Abzweig)" }),
        )
        .await;
    assert_eq!(status, 200, "{fork}");
    let child = fork["id"].as_str().unwrap().to_owned();
    assert_ne!(child, id);
    let s = server.get(&format!("/sessions/{child}")).await;
    let header = &s["header"];
    assert_eq!(header["parentSession"], id.as_str(), "linked to its source");
    assert_eq!(header["isSeeded"], true);
    assert_eq!(header["agentPreset"], "minimal", "the mode stays");
    assert!(header.get("origin").is_none(), "a branch is no subagent");
    let ws_path = ws.path().canonicalize().unwrap();
    assert_eq!(header["cwd"], ws_path.display().to_string());
    let events = s["events"].as_array().unwrap();
    assert_eq!(&events[..=end], &source[..=end]);
    assert_eq!(events[end + 1]["type"], "session/end-seed");
    assert_eq!(events[end + 1]["data"], json!({ "inherited": true }));
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "turn/start" && e["data"]["turn"] == 2),
        "nothing of turn 2"
    );

    // The branch is a chat of its own in the sidebar, under its new title.
    let rows = server.get("/sessions").await;
    let rows = rows.as_array().unwrap();
    let row = rows.iter().find(|r| r["id"] == child.as_str()).unwrap();
    assert_eq!(row["title"], "Erste Frage (Abzweig)");
    assert_eq!(row["preset"], "minimal");
    assert_eq!(row["unread"], false);
    assert!(rows.iter().any(|r| r["id"] == id.as_str()));

    // It continues with turn 2 of its own; the source is untouched.
    let after = server.run_turn(&child, "other second", 2).await;
    let prompts: Vec<_> = after
        .iter()
        .filter(|e| e["type"] == "user/message" && e["data"]["source"]["kind"] == "user")
        .map(|e| e["data"]["content"][0]["text"].clone())
        .collect();
    assert_eq!(prompts, [json!("first"), json!("other second")]);
    assert_eq!(
        server.get(&format!("/sessions/{id}")).await["events"]
            .as_array()
            .unwrap()
            .len(),
        source.len()
    );

    // Requests count where they were made: 2 in the source, 1 in the branch.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let usage = server.get("/usage?since=0").await;
        if usage["totals"]["requests"] == 3 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{usage}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Turn 0 branches before the first prompt; the title counts up like dsh.
    let (status, empty) = server
        .post(&format!("/sessions/{id}/fork"), json!({ "turn": 0 }))
        .await;
    assert_eq!(status, 200, "{empty}");
    let empty = empty["id"].as_str().unwrap().to_owned();
    let s = server.get(&format!("/sessions/{empty}")).await;
    assert_eq!(s["events"].as_array().unwrap().len(), 1);
    assert_eq!(s["header"]["agentPreset"], "minimal");
    let rows = server.get("/sessions").await;
    let title = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == empty.as_str())
        .unwrap()["title"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(title.ends_with(" (1)"), "{title}");

    let (status, v) = server
        .post(&format!("/sessions/{id}/fork"), json!({ "turn": 9 }))
        .await;
    assert_eq!(status, 400, "{v}");
    let (status, _) = server
        .post("/sessions/session-nope/fork", json!({ "turn": 1 }))
        .await;
    assert_eq!(status, 404);
    let (status, _) = server
        .post(&format!("/sessions/{id}/fork"), json!({}))
        .await;
    assert_eq!(status, 400);
}
