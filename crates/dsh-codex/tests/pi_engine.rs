//! Pi chats through the web API (`POST /api/sessions {engine: "pi"}`).
//!
//! Without a runtime the engine reports itself unavailable and creation is
//! refused with the reason. With the pinned runtime in `DSHX_PI_TEST_RUNTIME`
//! (e.g. `desktop/build/pi` from `desktop/fetch-pi.py`) and Node in
//! `DSHX_PI_TEST_NODE`, a Pi chat is created, answers a prompt with the faux
//! model (`DSHX_PI_TEST=1`, no network) and is deleted again.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

struct Web {
    child: Child,
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Web {
    fn start(home: &Path, env: &[(&str, &Path)], test: bool) -> Web {
        let mut cmd = Command::new(BIN);
        cmd.args(["web", "--port", "0", "--no-open"])
            .env("DSHX_HOME", home)
            .env("DSH_HOME", home.join("dsh"))
            .env("DSH_AGENTS_HOME", home.join("agents"))
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("DSHX_PI")
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        if test {
            cmd.env("DSHX_PI_TEST", "1");
        }
        let mut child = cmd.spawn().unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let url = loop {
            let line = lines.next().expect("web server line").unwrap();
            if let Some(url) = line.strip_prefix("dsh-codex web: ") {
                break url.to_owned();
            }
        };
        std::thread::spawn(move || for _ in lines {});
        let (base, token) = url.split_once("/?token=").unwrap();
        Web {
            child,
            base: base.to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    async fn raw(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, value) = self.raw(method.clone(), path, body).await;
        assert!(status < 300, "{method} {path}: {status} {value}");
        value
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pi_row(info: &Value) -> Value {
    info["engines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "pi")
        .cloned()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn pi_chats_are_created_through_the_api() {
    use reqwest::Method;
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let cwd = ws.path().canonicalize().unwrap();

    // No runtime: Pi is unavailable with a reason; DSH chats are unchanged.
    let missing = home.path().join("no-pi-runtime");
    {
        let web = Web::start(home.path(), &[("DSHX_PI", &missing)], false);
        let info = web.ok(Method::GET, "/info", None).await;
        let pi = pi_row(&info);
        assert_eq!(pi["available"], false, "{pi}");
        assert!(pi["reason"].as_str().unwrap().contains("DSHX_PI"), "{pi}");
        assert_eq!(info["settings"]["defaultEngine"], "dsh");
        let (status, body) = web
            .raw(
                Method::POST,
                "/sessions",
                Some(json!({"cwd": cwd, "engine": "pi"})),
            )
            .await;
        assert_eq!(status, 400, "{body}");
        assert!(body.to_string().contains("Pi is not available"), "{body}");
        let dsh = web
            .ok(
                Method::POST,
                "/sessions",
                Some(json!({"cwd": cwd, "preset": "standard", "engine": "dsh"})),
            )
            .await;
        let id = dsh["id"].as_str().unwrap();
        let session = web.ok(Method::GET, &format!("/sessions/{id}"), None).await;
        assert!(session["header"].get("engine").is_none(), "{session}");
        assert_eq!(session["header"]["agentPreset"], "standard");
    }

    let (Some(runtime), Some(node)) = (
        std::env::var_os("DSHX_PI_TEST_RUNTIME"),
        std::env::var_os("DSHX_PI_TEST_NODE"),
    ) else {
        eprintln!(
            "Pi runtime not given (DSHX_PI_TEST_RUNTIME, DSHX_PI_TEST_NODE): live part skipped"
        );
        return;
    };
    let (runtime, node) = (Path::new(&runtime), Path::new(&node));
    let web = Web::start(
        home.path(),
        &[("DSHX_PI", runtime), ("DSHX_NODE", node)],
        true,
    );
    let info = web.ok(Method::GET, "/info", None).await;
    let pi = pi_row(&info);
    assert_eq!(pi["available"], true, "{pi}");
    assert_eq!(pi["version"], "1.5.0");
    assert!(
        info["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "pi-faux/faux-1")
    );
    // The default engine applies when neither engine nor preset is given.
    web.ok(
        Method::PUT,
        "/settings",
        Some(json!({"defaultEngine": "pi"})),
    )
    .await;
    let created = web
        .ok(Method::POST, "/sessions", Some(json!({"cwd": cwd})))
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let session = web.ok(Method::GET, &format!("/sessions/{id}"), None).await;
    assert_eq!(session["header"]["engine"], "pi", "{session}");
    assert_eq!(session["header"]["engineVersion"], "1.5.0");
    assert!(session["header"].get("agentPreset").is_none());

    web.ok(
        Method::POST,
        &format!("/sessions/{id}/prompt"),
        Some(json!({"text": "hello from the api", "model": "pi-faux/faux-1"})),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let events = loop {
        let s = web.ok(Method::GET, &format!("/sessions/{id}"), None).await;
        let events = s["events"].as_array().unwrap().clone();
        if s["status"] != "running" && events.iter().any(|e| e["type"] == "turn/end") {
            break events;
        }
        assert!(tokio::time::Instant::now() < deadline, "no answer: {s}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let answer = events
        .iter()
        .find(|e| e["type"] == "assistant/message")
        .expect("assistant/message");
    assert_eq!(
        answer["data"]["message"]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["text"],
        "Echo: hello from the api"
    );
    let end = events.iter().find(|e| e["type"] == "turn/end").unwrap();
    assert_eq!(end["data"]["reason"]["kind"], "completed", "{end}");

    let rows = web.ok(Method::GET, "/sessions", None).await;
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["engine"], "pi", "{row}");
    let stats = web
        .ok(Method::GET, &format!("/sessions/{id}/stats"), None)
        .await;
    assert!(stats.is_object(), "{stats}");
    let mcp = web
        .ok(Method::GET, &format!("/sessions/{id}/mcp"), None)
        .await;
    assert_eq!(mcp["engine"], "pi");

    let pi_sessions = home.path().join("pi").join("sessions");
    let files = || {
        std::fs::read_dir(&pi_sessions)
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(files(), 1, "Pi keeps the chat's model context");
    web.ok(Method::DELETE, &format!("/sessions/{id}"), None)
        .await;
    assert_eq!(files(), 0, "deleting the chat removes Pi's session file");
}
