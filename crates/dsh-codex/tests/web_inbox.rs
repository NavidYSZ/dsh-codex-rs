//! Busy-chat input through the real HTTP endpoint, including durable ACKs.
use dshx_testkit::{MockDeepSeek, Reply};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
impl Server {
    fn start(mock: &MockDeepSeek, home: &std::path::Path) -> Self {
        std::fs::write(home.join("config.toml"), "[codex.browser]\nenabled = false\n[codex.computerUse]\nenabled = false\n[codex.apps]\nenabled = false\n[codex.compaction]\nmode = 'off'\n").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_dsh-codex"))
            .args(["web", "--port", "0", "--no-open"])
            .env("DSHX_HOME", home)
            .env_remove("DSH_HOME")
            .env("DEEPSEEK_API_KEY", "sk-mock")
            .env("DEEPSEEK_BASE_URL", mock.base_url())
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut server = Self {
            child,
            base: String::new(),
            token: String::new(),
        };
        loop {
            let mut line = String::new();
            assert!(stderr.read_line(&mut line).unwrap() > 0, "server exited");
            if let Some(url) = line.trim().strip_prefix("dsh-codex web: ") {
                let (base, token) = url.split_once("/?token=").unwrap();
                server.base = base.into();
                server.token = token.into();
                std::thread::spawn(move || for _ in stderr.lines() {});
                return server;
            }
        }
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = reqwest::Client::new()
            .post(format!("{}/api{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        (
            response.status().as_u16(),
            response.json().await.unwrap_or(Value::Null),
        )
    }
    async fn session(&self, id: &str) -> Value {
        reqwest::Client::new()
            .get(format!("{}/api/sessions/{id}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn until(&self, id: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let session = self.session(id).await;
                if predicate(&session) {
                    return session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("session did not reach expected state")
    }
    async fn create(&self, cwd: &std::path::Path) -> String {
        let (status, body) = self
            .post("/sessions", json!({"cwd": cwd, "preset": "standard"}))
            .await;
        assert_eq!(status, 200, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }
}

fn gate_model(mock: &MockDeepSeek, gate: &std::path::Path) {
    let gate = gate.to_str().unwrap().replace('\'', "'\\''");
    mock.set_responder(move |request| {
        if request.system().is_some_and(|s| s.contains("concise title")) { return Reply::text("Inbox test"); }
        if request.tool_results() == 0 {
            return Reply::tool_call("gate", "bash", json!({"command": format!("while [ ! -f '{gate}' ]; do sleep 0.02; done"), "description": "Wait for the test gate"}));
        }
        Reply::text("done")
    });
}

fn has_event(session: &Value, kind: &str) -> bool {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["type"] == kind)
}

fn user_texts(session: &Value) -> Vec<String> {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "user/message" && e["data"]["source"]["kind"] == "user")
        .map(|e| e["data"]["content"][0]["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_steering_and_queue_are_durable_before_ack_and_run_once() {
    let mock = MockDeepSeek::start().await;
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let gate = workspace.path().join("release");
    gate_model(&mock, &gate);
    let server = Server::start(&mock, home.path());
    let id = server.create(workspace.path()).await;
    let path = format!("/sessions/{id}/prompt");
    assert_eq!(
        server
            .post(
                &path,
                json!({"text": "start", "sandbox": "danger-full-access"})
            )
            .await
            .0,
        202
    );
    server.until(&id, |s| has_event(s, "tool/call")).await;

    let submitted = futures::future::join_all((0..8).map(|i| {
        let path = &path;
        let server = &server;
        async move {
            let text = format!("message-{i}");
            let result = server.post(path, json!({"text": text, "steer": i % 2 == 0, "messageId": format!("client-{i}")})).await;
            assert_eq!(result.0, 202, "{}", result.1);
            // Retrying an unchanged request during the same running turn is
            // idempotent; changing content under its ID is rejected.
            assert_eq!(server.post(path, json!({"text": text, "steer": i % 2 == 0, "messageId": format!("client-{i}")})).await.0, 202);
            assert_eq!(server.post(path, json!({"text": "different", "messageId": format!("client-{i}")})).await.0, 400);
            text
        }
    })).await;
    let store = dshx_session::SessionStore::new(home.path().join("sessions"));
    let durable = store.read(&id).unwrap();
    for text in &submitted {
        assert!(
            durable
                .events
                .iter()
                .filter(|e| e.kind == "agent/inbox/spliced")
                .flat_map(|e| e.data["inserted"].as_array().into_iter().flatten())
                .any(|message| message["content"][0]["text"] == *text),
            "ACK was not durable: {text}"
        );
    }
    let busy = server.session(&id).await;
    assert!(
        !has_event(&busy, "turn/end"),
        "{}",
        busy["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "tool/result" || e["type"] == "turn/end")
            .cloned()
            .collect::<Vec<_>>()
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(gate, "release").unwrap();
    let done = server
        .until(&id, |s| s["status"] == "idle" && user_texts(s).len() == 9)
        .await;
    let mut actual = user_texts(&done);
    actual.sort();
    let mut expected = submitted;
    expected.push("start".into());
    expected.sort();
    assert_eq!(actual, expected);
    assert_eq!(
        done["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "turn/start")
            .count(),
        5
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn acknowledged_cancel_does_not_resume_a_queued_prompt_after_restart() {
    let mock = MockDeepSeek::start().await;
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    gate_model(&mock, &workspace.path().join("never-release"));
    let server = Server::start(&mock, home.path());
    let id = server.create(workspace.path()).await;
    let path = format!("/sessions/{id}/prompt");
    assert_eq!(
        server
            .post(
                &path,
                json!({"text": "start", "sandbox": "danger-full-access"})
            )
            .await
            .0,
        202
    );
    server.until(&id, |s| has_event(s, "tool/call")).await;
    assert_eq!(
        server
            .post(&path, json!({"text": "must stay canceled"}))
            .await
            .0,
        202
    );
    assert_eq!(
        server
            .post(&format!("/sessions/{id}/cancel"), json!({}))
            .await
            .0,
        202
    );
    // Read immediately, before the periodic flush, then kill the process.
    let disk = dshx_session::SessionStore::new(home.path().join("sessions"))
        .read(&id)
        .unwrap();
    assert!(
        disk.events
            .iter()
            .any(|e| e.kind == "agent/inbox/spliced" && e.data["outcome"] == "canceled")
    );
    drop(server);
    let server = Server::start(&mock, home.path());
    assert_eq!(
        server.post(&path, json!({"text": "after restart"})).await.0,
        202
    );
    let done = server
        .until(&id, |s| {
            s["status"] == "idle" && user_texts(s).contains(&"after restart".to_owned())
        })
        .await;
    assert_eq!(user_texts(&done), ["start", "after restart"]);
}
