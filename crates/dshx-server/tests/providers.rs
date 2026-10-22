//! The Models page routes against a fake backend: what reaches
//! [`Providers`], the token check, and 404 without providers.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, PromptRequest, Providers, StreamMessage, serve};
use serde_json::{Value, json};
use tokio::sync::broadcast;

#[derive(Default)]
struct FakeProviders {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl Providers for FakeProviders {
    async fn list(&self) -> anyhow::Result<Value> {
        Ok(json!({"providers": [{"id": "deepseek-official"}], "catalog": []}))
    }
    async fn save(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(format!("save {id} {body}"));
        anyhow::ensure!(id != "Bad", "invalid provider id");
        Ok(json!({"id": id}))
    }
    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("remove {id}"));
        Ok(())
    }
    async fn set_key(&self, id: &str, key: Option<String>) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(format!("key {id} {key:?}"));
        Ok(json!({"id": id, "key": {"configured": key.is_some()}}))
    }
    async fn fetch_models(&self, id: &str, probe: Value) -> anyhow::Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("fetch {id} {probe}"));
        Ok(json!({"models": [{"id": "m"}]}))
    }
}

struct Fake {
    tx: broadcast::Sender<StreamMessage>,
    providers: Option<Arc<FakeProviders>>,
}

fn no() -> anyhow::Error {
    anyhow::anyhow!("not used in this test")
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
    async fn update_settings(&self, _: Value) -> anyhow::Result<Value> {
        Err(no())
    }
    async fn workspaces(&self) -> anyhow::Result<Value> {
        Err(no())
    }
    async fn add_workspace(&self, _: String) -> anyhow::Result<Value> {
        Err(no())
    }
    async fn list_sessions(&self, _: Option<String>) -> anyhow::Result<Value> {
        Err(no())
    }
    async fn create_session(&self, _: String) -> anyhow::Result<String> {
        Err(no())
    }
    async fn session(&self, _: &str) -> anyhow::Result<Value> {
        Err(no())
    }
    async fn events_after(&self, _: &str, _: Option<u64>) -> anyhow::Result<Vec<SessionEvent>> {
        Err(no())
    }
    async fn prompt(&self, _: &str, _: PromptRequest) -> anyhow::Result<()> {
        Err(no())
    }
    async fn cancel(&self, _: &str) -> anyhow::Result<()> {
        Err(no())
    }
    async fn delete_session(&self, _: &str) -> anyhow::Result<()> {
        Err(no())
    }
    async fn approve(&self, _: &str, _: &str) -> anyhow::Result<()> {
        Err(no())
    }
    async fn save_attachment(&self, _: Vec<u8>, _: String) -> anyhow::Result<ImageAttachmentRef> {
        Err(no())
    }
    async fn load_attachment(&self, _: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Err(no())
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
    fn providers(&self) -> Option<Arc<dyn Providers>> {
        self.providers.clone().map(|p| p as Arc<dyn Providers>)
    }
}

async fn start(providers: Option<Arc<FakeProviders>>) -> String {
    let (tx, _) = broadcast::channel(8);
    let (addr, _server) = serve(Arc::new(Fake { tx, providers }), "tok".into(), 0)
        .await
        .unwrap();
    format!("http://{addr}/api")
}

async fn call(
    method: reqwest::Method,
    url: String,
    body: Option<Value>,
    token: &str,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn provider_routes_reach_the_backend() {
    use reqwest::Method;
    let fake = Arc::new(FakeProviders::default());
    let base = start(Some(fake.clone())).await;
    let (status, _) = call(Method::GET, format!("{base}/providers"), None, "wrong").await;
    assert_eq!(status, 401);
    let (status, list) = call(Method::GET, format!("{base}/providers"), None, "tok").await;
    assert_eq!(
        (status, list["providers"][0]["id"].as_str()),
        (200, Some("deepseek-official"))
    );
    let (status, row) = call(
        Method::PUT,
        format!("{base}/providers/relay"),
        Some(json!({"api": "openai-completions"})),
        "tok",
    )
    .await;
    assert_eq!((status, row["id"].as_str()), (200, Some("relay")));
    let (status, _) = call(
        Method::PUT,
        format!("{base}/providers/Bad"),
        Some(json!({})),
        "tok",
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = call(
        Method::PUT,
        format!("{base}/providers/relay"),
        Some(json!([1])),
        "tok",
    )
    .await;
    assert_eq!(status, 400);
    let (_, row) = call(
        Method::PUT,
        format!("{base}/providers/relay/key"),
        Some(json!({"key": "  "})),
        "tok",
    )
    .await;
    assert_eq!(row["key"]["configured"], false);
    call(
        Method::PUT,
        format!("{base}/providers/relay/key"),
        Some(json!({"key": "sk"})),
        "tok",
    )
    .await;
    let (status, fetched) = call(
        Method::POST,
        format!("{base}/providers/relay/models/fetch"),
        None,
        "tok",
    )
    .await;
    assert_eq!(
        (status, fetched["models"][0]["id"].as_str()),
        (200, Some("m"))
    );
    let (status, _) = call(
        Method::DELETE,
        format!("{base}/providers/relay"),
        None,
        "tok",
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            r#"save relay {"api":"openai-completions"}"#,
            "save Bad {}",
            "key relay None",
            r#"key relay Some("sk")"#,
            "fetch relay null",
            "remove relay",
        ]
    );

    // A backend without providers answers 404.
    let base = start(None).await;
    let (status, _) = call(Method::GET, format!("{base}/providers"), None, "tok").await;
    assert_eq!(status, 404);
}
