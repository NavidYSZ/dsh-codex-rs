//! Test support: a fake Swift helper on a Unix socket and a minimal tool context.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ImageAttachmentRef, NewImage, SandboxMode,
    ToolContext,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

/// What the fake helper does with one request.
pub enum Reply {
    Json(Value),
    /// Close without answering.
    Close,
    /// Never answer (for timeouts).
    Hang,
    /// Send these bytes as the answer.
    Raw(&'static str),
}

type Responder = dyn Fn(&Value) -> Reply + Send + Sync;

/// Behaves like `Server.swift`: one JSON line in, one JSON line (with the
/// request's `id`) out, then close. Records every raw request line.
pub struct FakeHelper {
    pub path: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeHelper {
    pub fn start(
        path: &Path,
        respond: impl Fn(&Value) -> Reply + Send + Sync + 'static,
    ) -> FakeHelper {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let respond: Arc<Responder> = Arc::new(respond);
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let recorded = recorded.clone();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 65536];
                    while !buffer.contains(&b'\n') {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let end = buffer.iter().position(|b| *b == b'\n').unwrap();
                    let line = String::from_utf8(buffer[..end].to_vec()).unwrap();
                    recorded.lock().unwrap().push(line.clone());
                    let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    match respond(&request) {
                        Reply::Json(mut response) => {
                            if let (Some(obj), Some(id)) =
                                (response.as_object_mut(), request.get("id"))
                            {
                                obj.insert("id".into(), id.clone());
                            }
                            let mut out = serde_json::to_vec(&response).unwrap();
                            out.push(b'\n');
                            let _ = stream.write_all(&out).await;
                        }
                        Reply::Raw(text) => {
                            let _ = stream.write_all(text.as_bytes()).await;
                        }
                        Reply::Close => {}
                        Reply::Hang => tokio::time::sleep(Duration::from_secs(3600)).await,
                    }
                });
            }
        });
        FakeHelper {
            path: path.to_owned(),
            requests,
            task,
        }
    }

    /// Raw request lines so far.
    pub fn lines(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// Parsed requests without their `id`.
    pub fn requests(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                v.as_object_mut().unwrap().remove("id");
                v
            })
            .collect()
    }

    pub fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }
}

impl Drop for FakeHelper {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn shot(data: &str, width: u32, height: u32) -> Value {
    json!({ "ok": true, "mime": "image/jpeg", "data": data, "width": width, "height": height,
            "display": 1, "frontmostApp": "Finder", "cursor": { "x": 10, "y": 20 } })
}

fn app_state(app: &str) -> Value {
    if app == "Empty" {
        return json!({ "ok": true, "app": "Empty", "bundleId": "com.example.empty",
            "window": { "id": 7, "title": "", "width": 300, "height": 200 }, "otherWindows": [],
            "frontmost": true, "mime": "image/jpeg", "data": "AAAA", "width": 300, "height": 200, "tree": "", "elements": 0 });
    }
    json!({ "ok": true, "app": "Notes", "bundleId": "com.apple.Notes",
        "window": { "id": 42, "title": "Einkauf", "width": 800, "height": 600 }, "otherWindows": ["Notizen", "Ideen"],
        "frontmost": false, "mime": "image/jpeg", "data": "AAAAAAAA", "width": 800, "height": 600,
        "tree": "[0] AXButton \"Sichern\" @(10,20)\n[1] AXTextField \"Titel\" value=\"x\" @(30,40)\n[2] AXButton \"\" @(5,5)",
        "elements": 3 })
}

/// Deterministic answers for every command the tools send. Inputs containing
/// FAIL, CANCEL or SILENT produce helper errors.
pub fn respond(req: &Value) -> Reply {
    let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or_default();
    let has = |marker: &str| {
        req.as_object()
            .into_iter()
            .flatten()
            .any(|(k, v)| k != "activity" && v.as_str().is_some_and(|s| s.contains(marker)))
    };
    if has("FAIL") {
        return Reply::Json(json!({ "ok": false, "error": format!("boom: {cmd}") }));
    }
    if has("CANCEL") {
        return Reply::Json(json!({ "ok": false, "cancelled": true,
            "error": "The user stopped the agent (Esc or the Stop button). Stop operating the computer, browser and apps, and ask the user how to continue." }));
    }
    if has("SILENT") {
        return Reply::Json(json!({ "ok": false }));
    }
    let after = req.get("screenshotAfter") == Some(&json!(true));
    let state_after = req.get("stateAfter") == Some(&json!(true));
    Reply::Json(match cmd {
        "configure" | "reset" | "hide" => json!({ "ok": true }),
        "screenshot" => shot("AAAA", 1280, 800),
        "cursor_position" => json!({ "ok": true, "cursor": { "x": 5.4, "y": 6 } }),
        "drag" => {
            // No top-level cursor: the screenshot's cursor is used.
            json!({ "ok": true, "screenshot": shot("AAAA", 1280, 800) })
        }
        "type" => {
            let mut s = shot("AAAAAAAA", 1280, 800);
            s["frontmostApp"] = json!("");
            json!({ "ok": true, "cursor": { "x": 100, "y": 200 }, "screenshot": s })
        }
        "click" | "move" | "scroll" | "key" | "open_app" => {
            let mut res = json!({ "ok": true, "cursor": { "x": 100, "y": 200 } });
            if after {
                res["screenshot"] = shot("AAAA", 1280, 800);
            }
            res
        }
        "app_list" => json!({ "ok": true, "apps": [
            { "name": "Notes", "bundleId": "com.apple.Notes", "active": true, "hidden": false },
            { "name": "Rechner", "bundleId": "com.apple.calculator", "active": false, "hidden": true } ] }),
        "app_state" => app_state(req.get("app").and_then(Value::as_str).unwrap_or_default()),
        c if c.starts_with("app_") => {
            let mut res = json!({ "ok": true, "method": "AXPress" });
            if c == "app_menu" {
                let path = req["path"].as_array().unwrap();
                if path.len() == 1 {
                    res["method"] = json!("listed Ablage");
                    res["items"] = json!(["Neue Notiz", "Öffnen …"]);
                } else {
                    res["method"] = json!("accessibility (menu Neue Notiz)");
                }
            }
            if req.get("index").is_some() || req.get("x").is_some() {
                res["aim"] = json!({ "x": 12.5, "y": 30 });
            }
            if state_after {
                res["state"] = app_state("Notes");
            }
            res
        }
        c if c.starts_with("mini_") => {
            json!({ "ok": true, "cancelled": false, "requests": ["show"], "visible": false })
        }
        other => json!({ "ok": false, "error": format!("unknown command: {other}") }),
    })
}

/// Stores images in memory; a store that "downscales" anything over 3 bytes.
#[derive(Default)]
pub struct MemStore {
    pub saved: Mutex<Vec<Value>>,
}

#[async_trait]
impl AttachmentStore for MemStore {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        let mut saved = self.saved.lock().unwrap();
        saved.push(
            json!({ "mediaType": image.media_type, "name": image.name, "bytes": image.data.len() }),
        );
        let small = image.data.len() > 3;
        Ok(ImageAttachmentRef {
            attachment_id: format!("att-{}", saved.len()),
            media_type: image.media_type,
            bytes: image.data.len() as u64,
            width: if small { 640 } else { 1280 },
            height: if small { 400 } else { 800 },
            name: image.name,
            original_dimensions: None,
        })
    }

    async fn load(&self, _attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        anyhow::bail!("not stored")
    }
}

struct Deny;

#[async_trait]
impl Approver for Deny {
    async fn approve(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> ApprovalDecision {
        ApprovalDecision::Rejected
    }
}

pub fn context(store: Arc<MemStore>, images: bool) -> ToolContext {
    ToolContext {
        session_id: "session".into(),
        call_id: "call".into(),
        cwd: PathBuf::from("/"),
        sandbox: SandboxMode::WorkspaceWrite,
        cancel: CancellationToken::new(),
        attachments: store,
        model_accepts_images: images,
        approver: Arc::new(Deny),
        state: Arc::new(()),
        caller: None,
    }
}

/// A short temp dir under /tmp (socket paths are limited to ~104 bytes on macOS).
pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dshx")
        .tempdir_in("/tmp")
        .unwrap()
}
