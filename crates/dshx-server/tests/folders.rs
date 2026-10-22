//! `/api/fs/…`: the project folder browser against a fake backend.
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, NotFound, PromptRequest, StreamMessage, serve};
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Only the folder roots matter; `None` = a backend without folder browsing.
struct Fake {
    roots: Option<Vec<PathBuf>>,
    tx: broadcast::Sender<StreamMessage>,
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({}))
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
        Ok(json!([]))
    }
    async fn create_session(&self, _cwd: String) -> anyhow::Result<String> {
        anyhow::bail!("no sessions")
    }
    async fn session(&self, id: &str) -> anyhow::Result<Value> {
        Err(NotFound(id.into()).into())
    }
    async fn events_after(
        &self,
        _id: &str,
        _after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(vec![])
    }
    async fn prompt(&self, _id: &str, _request: PromptRequest) -> anyhow::Result<()> {
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
        _data: Vec<u8>,
        _media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        anyhow::bail!("no attachments")
    }
    async fn load_attachment(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Err(NotFound(id.into()).into())
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
    async fn folder_roots(&self) -> anyhow::Result<Vec<PathBuf>> {
        match &self.roots {
            Some(roots) => Ok(roots.clone()),
            None => Err(NotFound("folders".into()).into()),
        }
    }
}

async fn start(roots: Option<Vec<PathBuf>>) -> String {
    let (tx, _) = broadcast::channel(16);
    let (addr, _server) = serve(Arc::new(Fake { roots, tx }), "tok".into(), 0)
        .await
        .unwrap();
    format!("http://{addr}")
}

async fn get(base: &str, path: &str) -> (u16, Value) {
    let res = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth("tok")
        .send()
        .await
        .unwrap();
    (
        res.status().as_u16(),
        res.json().await.unwrap_or(Value::Null),
    )
}

async fn post(base: &str, path: &str, body: Value) -> (u16, Value) {
    let res = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .bearer_auth("tok")
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        res.status().as_u16(),
        res.json().await.unwrap_or(Value::Null),
    )
}

fn query(path: &std::path::Path) -> String {
    path.display()
        .to_string()
        .replace('%', "%25")
        .replace(' ', "%20")
}

#[tokio::test]
async fn browses_and_creates_inside_the_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let projects = root.join("projects");
    std::fs::create_dir_all(projects.join("app/.git")).unwrap();
    std::fs::create_dir_all(projects.join("site")).unwrap();
    std::fs::create_dir_all(root.join("private")).unwrap();
    // The nested root collapses into its parent.
    let base = start(Some(vec![projects.clone(), projects.join("app")])).await;

    let (status, body) = get(&base, &format!("/api/fs/dirs?path={}", query(&projects))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["path"], json!(projects.display().to_string()));
    assert_eq!(body["parent"], Value::Null, "the root has no parent here");
    assert_eq!(body["roots"], json!([projects.display().to_string()]));
    assert_eq!(body["entries"][0]["name"], "app");
    assert_eq!(body["entries"][0]["git"], true);
    assert_eq!(body["entries"][1]["git"], false);
    assert_eq!(body["truncated"], false);

    let (status, body) = get(
        &base,
        &format!("/api/fs/dirs?path={}", query(&root.join("private"))),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("outside-roots"))
    );
    let (status, body) = get(
        &base,
        &format!("/api/fs/dirs?path={}", query(&projects.join("gone"))),
    )
    .await;
    assert_eq!((status, body["code"].as_str()), (404, Some("not-found")));

    let parent = projects.display().to_string();
    let (status, body) = post(
        &base,
        "/api/fs/mkdir",
        json!({ "path": parent, "name": "New app" }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert!(projects.join("New app").is_dir());
    let (status, body) = post(
        &base,
        "/api/fs/mkdir",
        json!({ "path": parent, "name": "New app" }),
    )
    .await;
    assert_eq!((status, body["code"].as_str()), (409, Some("exists")));
    let (status, _) = post(
        &base,
        "/api/fs/mkdir",
        json!({ "path": parent, "name": "../escape" }),
    )
    .await;
    assert_eq!(status, 400);

    let (status, body) = get(&base, "/api/fs/suggestions").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["repos"],
        json!([{ "name": "app", "path": projects.join("app").display().to_string() }])
    );
}

#[tokio::test]
async fn without_roots_the_browser_is_unavailable() {
    let base = start(None).await;
    let (status, body) = get(&base, "/api/fs/dirs").await;
    assert_eq!((status, body["code"].as_str()), (404, Some("unsupported")));
    let res = reqwest::get(format!("{base}/api/fs/dirs")).await.unwrap();
    assert_eq!(res.status().as_u16(), 401, "the token guards the routes");
}

/// Only a Mac at its own screen has the native dialog (never opened in tests).
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn pick_needs_a_mac() {
    let base = start(None).await;
    let (status, body) = post(&base, "/api/fs/pick", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (501, Some("unsupported")));
}
