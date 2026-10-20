//! HTTP and WebSocket behaviour against a fake backend.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, NotFound, PromptRequest, StreamMessage, serve};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::broadcast;

struct Fake {
    events: Mutex<Vec<SessionEvent>>,
    prompts: Mutex<Vec<String>>,
    tx: broadcast::Sender<StreamMessage>,
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
}

async fn start() -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        events: Mutex::new(vec![event(0, "turn/start"), event(1, "step/start")]),
        prompts: Mutex::new(vec![]),
        tx: broadcast::channel(64).0,
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
