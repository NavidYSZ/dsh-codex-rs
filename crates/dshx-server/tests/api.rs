//! HTTP and WebSocket behaviour against a fake backend.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{
    Backend, Extensions, NotFound, OAuthCallback, PromptRequest, StreamMessage, UsageQuery, serve,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::broadcast;

struct Fake {
    events: Mutex<Vec<SessionEvent>>,
    prompts: Mutex<Vec<String>>,
    tx: broadcast::Sender<StreamMessage>,
    root: Mutex<Option<std::path::PathBuf>>,
    ext: Option<Arc<FakeExt>>,
}

/// Records the calls of the Plugins page routes.
#[derive(Default)]
struct FakeExt {
    calls: Mutex<Vec<String>>,
}

impl FakeExt {
    fn log(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

#[async_trait]
impl Extensions for FakeExt {
    async fn summary(&self) -> anyhow::Result<Value> {
        Ok(json!({ "plugins": 1, "skills": 2, "mcpServers": 3 }))
    }
    async fn plugins(&self) -> anyhow::Result<Value> {
        Ok(json!([{ "id": "linear@curated" }]))
    }
    async fn install_plugin(
        &self,
        marketplace: &str,
        name: &str,
        enable: bool,
    ) -> anyhow::Result<Value> {
        self.log(format!("install {name}@{marketplace} {enable}"));
        Ok(json!({ "id": format!("{name}@{marketplace}") }))
    }
    async fn set_plugin_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<Value> {
        self.log(format!("enable {id} {enabled}"));
        Ok(json!({ "id": id, "enabled": enabled }))
    }
    async fn plugin_asset(&self, id: &str, path: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.log(format!("asset {id} {path}"));
        Ok((b"<svg/>".to_vec(), "image/svg+xml".into()))
    }
    async fn add_marketplace(&self, source: &str, git_ref: Option<&str>) -> anyhow::Result<Value> {
        self.log(format!("market {source} {git_ref:?}"));
        Ok(json!({ "name": "curated" }))
    }
    async fn start_login(&self, server: &str, redirect_uri: &str) -> anyhow::Result<Value> {
        self.log(format!("login {server} {redirect_uri}"));
        Ok(json!({ "authorizationUrl": "https://as.example/authorize" }))
    }
    async fn finish_login(&self, params: OAuthCallback) -> anyhow::Result<String> {
        match params.state.as_deref() {
            Some("good") => Ok("linear".into()),
            _ => anyhow::bail!("unknown or expired login"),
        }
    }
    async fn set_token(&self, server: &str, token: Option<String>) -> anyhow::Result<Value> {
        self.log(format!("token {server} {token:?}"));
        Ok(json!({ "name": server }))
    }
    async fn mcp_server(&self, name: &str) -> anyhow::Result<Value> {
        match name {
            "fx" => Ok(json!({ "name": name })),
            _ => Err(NotFound(name.into()).into()),
        }
    }
    async fn update_mcp_server(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        self.log(format!("update {name} {body}"));
        Ok(json!({ "name": name }))
    }
    async fn test_mcp_server(&self, body: Value) -> anyhow::Result<Value> {
        self.log(format!("test {body}"));
        Ok(json!({ "ok": true, "tools": [] }))
    }
    async fn mcp_server_log(&self, name: &str) -> anyhow::Result<Value> {
        Ok(json!({ "name": name, "lines": [] }))
    }
    async fn skill(
        &self,
        name: &str,
        cwd: Option<String>,
        path: Option<String>,
    ) -> anyhow::Result<Value> {
        self.log(format!("skill {name} {cwd:?} {path:?}"));
        Ok(json!({ "name": name }))
    }
    async fn create_skill(&self, body: Value) -> anyhow::Result<Value> {
        self.log(format!("create skill {body}"));
        Ok(json!({ "name": body["name"] }))
    }
}

fn event(seq: u64, kind: &str) -> SessionEvent {
    SessionEvent {
        kind: kind.into(),
        seq,
        time: 0,
        data: json!({}),
        extra: Default::default(),
    }
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({ "version": "test" }))
    }
    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value> {
        Ok(patch)
    }
    async fn workspaces(&self) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn add_workspace(&self, path: String) -> anyhow::Result<Value> {
        Ok(json!({ "path": path }))
    }
    async fn list_sessions(&self, _cwd: Option<String>) -> anyhow::Result<Value> {
        Ok(json!([{ "id": "s1" }]))
    }
    async fn create_session(&self, _cwd: String) -> anyhow::Result<String> {
        Ok("s2".into())
    }
    async fn session(&self, id: &str) -> anyhow::Result<Value> {
        if id != "s1" {
            return Err(NotFound(id.into()).into());
        }
        Ok(json!({ "events": self.events.lock().unwrap().clone() }))
    }
    async fn events_after(
        &self,
        _id: &str,
        after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| after.is_none_or(|a| e.seq > a))
            .cloned()
            .collect())
    }
    async fn prompt(&self, _id: &str, request: PromptRequest) -> anyhow::Result<()> {
        self.prompts.lock().unwrap().push(request.text);
        Ok(())
    }
    async fn cancel(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_session(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn approve(&self, _call_id: &str, _decision: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn save_attachment(
        &self,
        data: Vec<u8>,
        media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        Ok(ImageAttachmentRef {
            attachment_id: "a1".into(),
            media_type,
            bytes: data.len() as u64,
            width: 1,
            height: 1,
            name: None,
            original_dimensions: None,
        })
    }
    async fn load_attachment(&self, _id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Ok((vec![1, 2, 3], "image/png".into()))
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
    async fn session_root(&self, id: &str) -> anyhow::Result<std::path::PathBuf> {
        let root = self.root.lock().unwrap().clone();
        root.filter(|_| id == "s1")
            .ok_or_else(|| NotFound(id.to_owned()).into())
    }
    fn extensions(&self) -> Option<Arc<dyn Extensions>> {
        self.ext.clone().map(|e| e as Arc<dyn Extensions>)
    }
    async fn session_stats(&self, id: &str, from_turn: Option<u64>) -> anyhow::Result<Value> {
        if id != "s1" {
            return Err(NotFound(id.into()).into());
        }
        Ok(json!({ "currency": "USD", "fromTurn": from_turn }))
    }
    async fn usage(&self, q: UsageQuery) -> anyhow::Result<Value> {
        Ok(json!({ "since": q.since, "until": q.until, "tz": q.tz, "top": q.top }))
    }
}

async fn start() -> (String, Arc<Fake>) {
    start_with(None).await
}

async fn start_with(ext: Option<Arc<FakeExt>>) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        events: Mutex::new(vec![event(0, "turn/start"), event(1, "step/start")]),
        prompts: Mutex::new(vec![]),
        tx: broadcast::channel(64).0,
        root: Mutex::new(None),
        ext,
    });
    let (addr, _handle) = serve(fake.clone(), "secret".into(), 0).await.unwrap();
    (format!("127.0.0.1:{}", addr.port()), fake)
}

#[tokio::test]
async fn api_requires_token_and_localhost() {
    let (addr, _) = start().await;
    let http = reqwest::Client::new();
    let r = http
        .get(format!("http://{addr}/api/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .get(format!("http://{addr}/api/info"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .get(format!("http://{addr}/api/info"))
        .header("host", "evil.example")
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .get(format!("http://{addr}/api/info?token=secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["version"], "test");
}

#[tokio::test]
async fn routes_map_to_backend() {
    let (addr, fake) = start().await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}/api{p}");
    let r = http
        .post(url("/sessions/s1/prompt"))
        .bearer_auth("secret")
        .json(&json!({ "text": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    assert_eq!(fake.prompts.lock().unwrap().as_slice(), ["hi"]);
    let r = http
        .get(url("/sessions/nope"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = http
        .post(url("/approvals/c1"))
        .bearer_auth("secret")
        .json(&json!({ "decision": "maybe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = http
        .post(url("/attachments"))
        .bearer_auth("secret")
        .header("content-type", "text/plain")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = http
        .post(url("/attachments"))
        .bearer_auth("secret")
        .header("content-type", "image/png")
        .body(vec![0u8; 10])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["bytes"], 10);
    // A chat without a project folder: no cwd in the body.
    let r = http
        .post(url("/sessions"))
        .bearer_auth("secret")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Backends without sidebar state refuse PATCH instead of pretending.
    let r = http
        .patch(url("/sessions/s1"))
        .bearer_auth("secret")
        .json(&json!({ "pinned": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn stats_and_usage_routes_pass_their_queries() {
    let (addr, _) = start().await;
    let http = reqwest::Client::new();
    let get = |p: &str| {
        http.get(format!("http://{addr}/api{p}"))
            .bearer_auth("secret")
            .send()
    };
    let r = get("/sessions/s1/stats?fromTurn=3").await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({ "currency": "USD", "fromTurn": 3 })
    );
    let r = get("/sessions/s1/stats").await.unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["fromTurn"], Value::Null);
    assert_eq!(get("/sessions/nope/stats").await.unwrap().status(), 404);
    let r = get("/usage?since=1000&tz=-120&top=3").await.unwrap();
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({ "since": 1000, "until": null, "tz": -120, "top": 3 })
    );
    assert_eq!(get("/usage?since=soon").await.unwrap().status(), 400);
}

#[tokio::test]
async fn websocket_sends_backlog_then_live_without_duplicates() {
    let (addr, fake) = start().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/stream?session=s1&after=0&token=secret"
    ))
    .await
    .unwrap();
    let next = |m: Option<Result<tokio_tungstenite::tungstenite::Message, _>>| -> Value {
        serde_json::from_str(m.unwrap().unwrap().to_text().unwrap()).unwrap()
    };
    let first = next(ws.next().await);
    assert_eq!(first["kind"], "event");
    assert_eq!(first["event"]["seq"], 1);
    // Duplicate of the backlog (seq 1) is dropped, other sessions are filtered, seq 2 arrives.
    fake.tx
        .send(StreamMessage::Event {
            session_id: "s1".into(),
            event: event(1, "step/start"),
        })
        .unwrap();
    fake.tx
        .send(StreamMessage::Event {
            session_id: "other".into(),
            event: event(5, "x"),
        })
        .unwrap();
    fake.tx
        .send(StreamMessage::Event {
            session_id: "s1".into(),
            event: event(2, "step/end"),
        })
        .unwrap();
    let live = next(ws.next().await);
    assert_eq!(live["event"]["seq"], 2);
    assert_eq!(live["sessionId"], "s1");
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn serves_ui_with_csp_and_accepts_after_minus_one() {
    let (addr, _) = start().await;
    let r = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(r.status(), 200);
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("'sha256-"), "{csp}");
    let r = reqwest::get(format!("http://{addr}/vendor/preact.mjs"))
        .await
        .unwrap();
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    let r = reqwest::get(format!("http://{addr}/dev/shoot.py"))
        .await
        .unwrap();
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("html"),
        "dev files are not embedded"
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/stream?session=s1&after=-1&token=secret"
    ))
    .await
    .unwrap();
    let first: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(first["event"]["seq"], 0);
}

#[tokio::test]
async fn skill_and_mcp_page_routes_map_to_extensions() {
    let ext = Arc::new(FakeExt::default());
    let (addr, _) = start_with(Some(ext.clone())).await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}/api{p}");
    let send = |r: reqwest::RequestBuilder| async move {
        let r = r.bearer_auth("secret").send().await.unwrap();
        (
            r.status().as_u16(),
            r.json::<Value>().await.unwrap_or(Value::Null),
        )
    };
    let (status, body) = send(http.get(url("/mcp/servers/fx"))).await;
    assert_eq!((status, body["name"].as_str()), (200, Some("fx")));
    assert_eq!(send(http.get(url("/mcp/servers/nope"))).await.0, 404);
    let (status, _) = send(
        http.put(url("/mcp/servers/fx"))
            .json(&json!({ "config": { "url": "https://x/mcp" } })),
    )
    .await;
    assert_eq!(status, 200);
    let (_, probe) = send(http.post(url("/mcp/test")).json(&json!({ "config": {} }))).await;
    assert_eq!(probe["ok"], true);
    let (_, log) = send(http.get(url("/mcp/servers/fx/log"))).await;
    assert_eq!(log["lines"], json!([]));
    send(http.get(url("/skills/brand-kit?cwd=/w&path=/w/SKILL.md"))).await;
    let (_, made) = send(
        http.post(url("/skills"))
            .json(&json!({ "name": "notes", "scope": "personal" })),
    )
    .await;
    assert_eq!(made["name"], "notes");
    let calls = ext.calls.lock().unwrap().clone();
    let has = |c: &str| calls.iter().any(|x| x == c);
    assert!(
        has("update fx {\"config\":{\"url\":\"https://x/mcp\"}}"),
        "{calls:?}"
    );
    assert!(
        has("skill brand-kit Some(\"/w\") Some(\"/w/SKILL.md\")"),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("create skill ")),
        "{calls:?}"
    );
    // Without extensions the new routes are 404 as well.
    let (addr, _) = start_with(None).await;
    let r = http
        .post(format!("http://{addr}/api/mcp/test"))
        .bearer_auth("secret")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn plugin_routes_map_to_extensions() {
    let ext = Arc::new(FakeExt::default());
    let (addr, _) = start_with(Some(ext.clone())).await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}/api{p}");
    let info: Value = http
        .get(url("/info"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["version"], "test");
    assert_eq!(info["extensions"]["mcpServers"], 3);
    let r = http.get(url("/plugins")).send().await.unwrap();
    assert_eq!(r.status(), 401, "the plugin routes need the token");
    let list: Value = http
        .get(url("/plugins"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list[0]["id"], "linear@curated");
    let r = http
        .post(url("/plugins"))
        .bearer_auth("secret")
        .json(&json!({ "marketplace": "curated", "name": "linear" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["id"], "linear@curated");
    http.post(url("/plugins/linear@curated/disable"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    let r = http
        .get(url("/plugins/linear@curated/assets/assets/logo.svg"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["content-type"], "image/svg+xml");
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    http.post(url("/marketplaces"))
        .bearer_auth("secret")
        .json(&json!({ "source": "openai/plugins", "ref": "main" }))
        .send()
        .await
        .unwrap();
    // The callback URL uses the port the browser used (an SSH tunnel's).
    let r = http
        .post(url("/mcp/servers/linear/login"))
        .bearer_auth("secret")
        .header("host", "localhost:3082")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.json::<Value>().await.unwrap()["authorizationUrl"],
        "https://as.example/authorize"
    );
    http.put(url("/mcp/servers/github/token"))
        .bearer_auth("secret")
        .json(&json!({ "token": "ghp_x" }))
        .send()
        .await
        .unwrap();
    // Not implemented by the fake: 404.
    let r = http
        .get(url("/connectors"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(
        ext.calls.lock().unwrap().as_slice(),
        [
            "install linear@curated true",
            "enable linear@curated false",
            "asset linear@curated assets/logo.svg",
            "market openai/plugins Some(\"main\")",
            "login linear http://127.0.0.1:3082/api/oauth/callback",
            "token github Some(\"ghp_x\")",
        ]
    );
}

#[tokio::test]
async fn oauth_callback_needs_no_token_but_a_pending_state() {
    let (addr, _) = start_with(Some(Arc::new(FakeExt::default()))).await;
    let http = reqwest::Client::new();
    let r = http
        .get(format!(
            "http://{addr}/api/oauth/callback?code=c&state=good"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    assert!(r.text().await.unwrap().contains("Connected: linear"));
    let r = http
        .get(format!(
            "http://{addr}/api/oauth/callback?code=c&state=forged"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("unknown or expired login"));
    let r = http
        .get(format!("http://{addr}/api/oauth/callback?state=good"))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // Without extensions the routes are 404.
    let (addr, _) = start().await;
    let r = http
        .get(format!("http://{addr}/api/plugins"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn previews_serve_only_the_chats_folder() {
    let (addr, fake) = start().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("site")).unwrap();
    std::fs::write(root.join("site/index.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(root.join("notes.md"), "# notes").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "no").unwrap();
    *fake.root.lock().unwrap() = Some(root);
    let http = reqwest::Client::new();
    let api = |p: &str| format!("http://{addr}/api{p}");

    let grant: Value = http
        .post(api("/sessions/s1/preview"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let base = format!("http://{addr}{}", grant["base"].as_str().unwrap());

    // A folder serves its index.html, sandboxed into an opaque origin.
    let r = http.get(format!("{base}site/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.starts_with("sandbox allow-scripts"), "{csp}");
    assert_eq!(r.text().await.unwrap(), "<h1>hi</h1>");
    let r = http.get(format!("{base}notes.md")).send().await.unwrap();
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert!(r.headers().get("content-security-policy").is_none());

    // Nothing outside the folder, no made-up grants, no foreign hosts.
    let r = http
        .get(format!("{base}..%2Fsecret.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = http
        .get(format!("http://{addr}/preview/nope/notes.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = http
        .get(format!("{base}notes.md"))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let list: Value = http
        .get(api("/sessions/s1/files?dir="))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<_> = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["site", "notes.md"], "folders first");
    let r = http
        .get(api("/sessions/s1/files?dir=.."))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn session_presets_need_backend_support() {
    let (addr, _) = start().await;
    let http = reqwest::Client::new();
    let create = |body: Value| {
        http.post(format!("http://{addr}/api/sessions"))
            .bearer_auth("secret")
            .json(&body)
            .send()
    };
    // Standard is what every backend runs; other presets need support.
    let r = create(json!({ "cwd": "/w", "preset": "standard" }))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["id"], "s2");
    let r = create(json!({ "preset": "minimal" })).await.unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "agent presets are not supported"
    );
}
