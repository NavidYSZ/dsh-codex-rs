//! Model providers in `dsh-codex web`: the Models page routes, keys in
//! `credentials.toml`, and one chat that switches between DeepSeek, an
//! Anthropic-compatible and an OpenAI-compatible endpoint (all mocks).
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
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

fn start(deepseek: &MockDeepSeek, home: &std::path::Path) -> Server {
    std::fs::write(
        home.join("config.toml"),
        "[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n",
    )
    .unwrap();
    let mut child = Command::new(BIN)
        .args(["web", "--port", "0", "--no-open"])
        .env("DSHX_HOME", home)
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", deepseek.base_url())
        // Keys of the machine running the tests must not leak in.
        .env_remove("ANTHROPIC_API_KEY")
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

    async fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, v) = self.call(method.clone(), path, body).await;
        assert_eq!(status, 200, "{method} {path}: {v}");
        v
    }

    async fn idle_after(&self, id: &str, turns: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let s = self
                .ok(reqwest::Method::GET, &format!("/sessions/{id}"), None)
                .await;
            let events = s["events"].as_array().cloned().unwrap_or_default();
            let ended = events.iter().filter(|e| e["type"] == "turn/end").count();
            if ended >= turns && s["status"] == "idle" {
                return events;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn {turns} did not end: {s}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn system_text(r: &RecordedRequest) -> String {
    match &r.body["system"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

fn chat_stream(text: &str) -> String {
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        )
    };
    [
        chunk(json!({"reasoning_content": "thinking"}), Value::Null),
        chunk(json!({"content": text}), Value::Null),
        chunk(json!({}), json!("stop")),
        format!(
            "data: {}\n\n",
            json!({"choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 5}})
        ),
        "data: [DONE]\n\n".into(),
    ]
    .concat()
}

fn assistant_sources(events: &[Value]) -> Vec<(String, String)> {
    events
        .iter()
        .filter(|e| e["type"] == "assistant/message")
        .map(|e| {
            let s = &e["data"]["message"]["source"];
            (
                s["provider"].as_str().unwrap_or_default().to_owned(),
                s["model"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_routes_and_a_chat_across_three_routes() {
    let deepseek = MockDeepSeek::start().await;
    deepseek.set_responder(|r| {
        if r.system().is_some_and(|s| s.contains("concise title")) {
            Reply::text("Titel")
        } else {
            Reply::text("DeepSeek here")
        }
    });
    let anthropic = MockDeepSeek::start().await;
    anthropic.set_responder(|r| {
        if system_text(r).contains("concise title") {
            Reply::text("Titel")
        } else {
            Reply::stream()
                .thinking("plan", "sig-claude")
                .text("Claude here")
                .into()
        }
    });
    let relay = MockDeepSeek::start().await;
    relay.set_responder(|_| Reply::RawSse(chat_stream("Relay here")));
    relay.set_models(json!({"object": "list", "data": [
        {"id": "glm-5.3", "object": "model"}, {"id": "kimi-k3", "object": "model", "context_length": 1048576}
    ]}));
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&deepseek, home.path());
    use reqwest::Method;

    // DeepSeek is built in; Anthropic waits in the catalog.
    let list = server.ok(Method::GET, "/providers", None).await;
    assert_eq!(list["providers"].as_array().unwrap().len(), 1);
    assert_eq!(list["providers"][0]["id"], "deepseek-official");
    assert_eq!(list["providers"][0]["key"]["source"], "env");
    let anthropic_entry = list["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "anthropic")
        .unwrap();
    assert_eq!(anthropic_entry["added"], false);

    // Validation.
    let (status, err) = server
        .call(
            Method::PUT,
            "/providers/Bad",
            Some(json!({"api": "openai-completions"})),
        )
        .await;
    assert_eq!(status, 400, "{err}");
    let (status, err) = server
        .call(
            Method::PUT,
            "/providers/relay",
            Some(
                json!({"api": "openai-completions", "baseUrl": "ftp://x", "models": [{"id": "m"}]}),
            ),
        )
        .await;
    assert_eq!(status, 400, "{err}");
    assert!(err["error"].as_str().unwrap().contains("base URL"));
    let (status, _) = server
        .call(Method::DELETE, "/providers/deepseek-official", None)
        .await;
    assert_eq!(status, 400);

    // Anthropic from the catalog, pointed at the mock, with a stored key.
    let row = server
        .ok(
            Method::PUT,
            "/providers/anthropic",
            Some(json!({"baseUrl": anthropic.base_url(), "apiKey": "sk-ant-mock"})),
        )
        .await;
    assert_eq!(row["key"]["configured"], true);
    assert_eq!(row["key"]["source"], "stored");
    assert_eq!(row["modelsCustomized"], false);
    assert_eq!(row["models"].as_array().unwrap().len(), 4);
    assert!(
        !row.to_string().contains("sk-ant-mock"),
        "keys never come back"
    );
    let creds = std::fs::read_to_string(home.path().join("credentials.toml")).unwrap();
    assert!(creds.contains("[providers.anthropic]") && creds.contains("sk-ant-mock"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join("credentials.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        config.contains("[providers.anthropic]") && !config.contains("sk-ant"),
        "{config}"
    );

    // A custom OpenAI-compatible endpoint; its model list comes from the mock.
    let relay_base = format!("http://{}/v1", relay.addr());
    let fetched = server
        .ok(
            Method::POST,
            "/providers/relay/models/fetch",
            Some(json!({"api": "openai-completions", "baseUrl": relay_base, "apiKey": "sk-relay"})),
        )
        .await;
    let ids: Vec<&str> = fetched["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["glm-5.3", "kimi-k3"]);
    let row = server
        .ok(
            Method::PUT,
            "/providers/relay",
            Some(
                json!({"api": "openai-completions", "baseUrl": relay_base, "displayName": "Relay",
                "apiKey": "sk-relay", "models": [{"id": "glm-5.3", "contextWindow": 1000000,
                "reasoningEfforts": ["low", "high"], "cost": {"input": 0.5, "output": 2}}]}),
            ),
        )
        .await;
    assert_eq!(row["name"], "Relay");
    assert_eq!(row["models"][0]["ref"], "relay/glm-5.3");

    let info = server.ok(Method::GET, "/info", None).await;
    let refs: Vec<&str> = info["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(refs.contains(&"deepseek-flash") && refs.contains(&"anthropic/claude-sonnet-5-5"));
    assert!(refs.contains(&"relay/glm-5.3"));
    let sonnet = info["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "anthropic/claude-sonnet-5-5")
        .unwrap();
    assert_eq!(sonnet["cost"]["input"], 2.0);
    assert_eq!(sonnet["providerName"], "Anthropic");
    let groups: Vec<&str> = info["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(groups, ["deepseek-official", "anthropic", "relay"]);

    // One chat across three routes.
    let created = server
        .ok(Method::POST, "/sessions", Some(json!({"cwd": ws.path()})))
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let prompt = |text: &str, model: &str| json!({"text": text, "model": model, "effort": "high"});
    let (status, err) = server
        .call(
            Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(prompt("Hi Claude", "anthropic/claude-sonnet-5-5")),
        )
        .await;
    assert_eq!(status, 202, "{err}");
    server.idle_after(&id, 1).await;
    let requests = anthropic.requests();
    let turn = requests
        .iter()
        .find(|r| !system_text(r).contains("concise title"))
        .unwrap();
    assert_eq!(turn.path, "/anthropic/v1/messages");
    assert_eq!(turn.header("x-api-key"), Some("sk-ant-mock"));
    assert_eq!(turn.body["model"], "claude-sonnet-5-5");
    assert!(turn.raw_body.windows(13).any(|w| w == b"cache_control"));
    // The title went to the provider's cheap title model.
    let title = anthropic
        .wait_for_requests(2)
        .await
        .into_iter()
        .find(|r| system_text(r).contains("concise title"))
        .unwrap();
    assert_eq!(title.body["model"], "claude-haiku-5-5");

    server
        .call(
            Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(prompt("Hi relay", "relay/glm-5.3")),
        )
        .await;
    server.idle_after(&id, 2).await;
    let relay_request = &relay.requests()[0];
    assert_eq!(relay_request.path, "/v1/chat/completions");
    assert_eq!(
        relay_request.header("authorization"),
        Some("Bearer sk-relay")
    );
    // Claude's signed thinking does not travel to another provider.
    assert!(!String::from_utf8_lossy(&relay_request.raw_body).contains("sig-claude"));

    server
        .call(
            Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(prompt("Hi DeepSeek", "deepseek-flash")),
        )
        .await;
    let events = server.idle_after(&id, 3).await;
    assert_eq!(
        assistant_sources(&events),
        [
            ("anthropic".into(), "claude-sonnet-5-5".into()),
            ("relay".into(), "glm-5.3".into()),
            ("deepseek-official".into(), "deepseek-flash".into()),
        ]
    );
    let contexts: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "request/context")
        .map(|e| e["data"]["provider"].as_str().unwrap())
        .collect();
    assert_eq!(contexts, ["anthropic", "relay", "deepseek-official"]);
    let ds = deepseek
        .requests()
        .into_iter()
        .find(|r| !r.system().is_some_and(|s| s.contains("concise title")))
        .unwrap();
    assert!(!String::from_utf8_lossy(&ds.raw_body).contains("sig-claude"));

    // Without its key the provider refuses prompts up front.
    let row = server
        .ok(
            Method::PUT,
            "/providers/anthropic/key",
            Some(json!({"key": null})),
        )
        .await;
    assert_eq!(row["key"]["configured"], false);
    let (status, err) = server
        .call(
            Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(prompt("again", "anthropic/claude-sonnet-5-5")),
        )
        .await;
    assert_eq!(status, 400);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("No API key for Anthropic"),
        "{err}"
    );

    // Removing a provider removes its key and its table.
    let (status, _) = server.call(Method::DELETE, "/providers/relay", None).await;
    assert_eq!(status, 204);
    let creds = std::fs::read_to_string(home.path().join("credentials.toml")).unwrap_or_default();
    assert!(!creds.contains("relay"), "{creds}");
    let list = server.ok(Method::GET, "/providers", None).await;
    assert_eq!(list["providers"].as_array().unwrap().len(), 2);
    let (status, _) = server.call(Method::DELETE, "/providers/relay", None).await;
    assert_eq!(status, 404);
}
