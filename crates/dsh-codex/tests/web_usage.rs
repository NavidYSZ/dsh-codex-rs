//! `dsh-codex web`: what a chat cost (`/api/sessions/{id}/stats`), the usage of
//! all chats (`/api/usage`) and the display preferences kept for every browser.
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
}

fn close(a: &Value, b: f64) -> bool {
    a.as_f64().is_some_and(|a| (a - b).abs() < 1e-12)
}

#[tokio::test(flavor = "multi_thread")]
async fn web_prices_chats_and_keeps_display_preferences() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r| {
        if r.system().is_some_and(|s| s.contains("concise title")) {
            return Reply::text("Titel");
        }
        Reply::stream()
            .text("Done.")
            .usage(1000, 200)
            .cache(9000, 0)
            .into()
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());

    let (_, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, _) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(json!({"text": "hello"})),
        )
        .await;
    assert_eq!(status, 202);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let s = server.get(&format!("/sessions/{id}")).await;
        let ended = s["events"]
            .as_array()
            .is_some_and(|e| e.iter().any(|e| e["type"] == "turn/end"));
        if ended {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no turn/end: {s}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let stats = server.get(&format!("/sessions/{id}/stats")).await;
    let totals = &stats["totals"];
    assert_eq!(stats["currency"], "USD");
    assert_eq!(totals["requests"], 1, "{stats}");
    assert_eq!(
        (
            &totals["inputTokens"],
            &totals["cacheReadTokens"],
            &totals["outputTokens"]
        ),
        (&json!(1000), &json!(9000), &json!(200))
    );
    // deepseek-flash at peak: 0.30 miss, 0.006 hit, 1.20 output per 1M; off-peak half.
    let peak = (1000.0 * 0.30 + 9000.0 * 0.006 + 200.0 * 1.20) / 1e6;
    let expected = if totals["offPeakRequests"] == 1 {
        peak / 2.0
    } else {
        peak
    };
    assert!(close(&totals["costUsd"], expected), "{stats}");
    assert_eq!(stats["turnCount"], 1);
    assert!(close(&stats["turns"][0]["costUsd"], expected));
    assert_eq!(stats["turns"][0]["status"], "completed");
    assert_eq!(stats["context"]["usedTokens"], 10_200);
    assert!(
        stats["context"]["compaction"]["thresholdTokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(stats["timings"]["llmMs"].is_u64());
    assert_eq!(
        server
            .get(&format!("/sessions/{id}/stats?fromTurn=2"))
            .await["turns"],
        json!([])
    );
    let (status, _) = server
        .call(reqwest::Method::GET, "/sessions/nope/stats", None)
        .await;
    assert_eq!(status, 404);

    // The index reads the logs on disk, which trail the live session by up to 200 ms.
    let usage = loop {
        let usage = server.get("/usage?since=0&tz=120").await;
        if usage["totals"]["requests"] == 1 {
            break usage;
        }
        assert!(tokio::time::Instant::now() < deadline, "{usage}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(usage["days"].as_array().unwrap().len(), 401);
    assert!(close(&usage["totals"]["costUsd"], expected));
    assert_eq!(usage["topChats"][0]["id"], id.as_str());
    assert_eq!(usage["models"][0]["model"], "deepseek-flash");
    assert!(home.path().join("usage-index.json").is_file());

    // Display preferences: in /api/info, changed through /api/settings.
    let info = server.get("/info").await;
    assert_eq!(
        info["ui"],
        json!({
            "workDetails": "standard", "performanceUsage": "detailed",
            "sidebarGroup": "projects", "sidebarOrder": "activity", "sidebarArchived": "hide",
            "confirmFullAccess": true,
        })
    );
    let (status, info) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"ui": {"workDetails": "compact"}})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(info["ui"]["workDetails"], "compact");
    let (status, _) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"ui": {"workDetails": "everything"}})),
        )
        .await;
    assert_eq!(status, 400);
    let state: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("ui-state.json")).unwrap()).unwrap();
    assert_eq!(state["ui"]["workDetails"], "compact");
}
