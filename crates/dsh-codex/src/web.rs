//! `dsh-codex web`: the Codex-style UI on a local port, backed by live agents.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_agent::{Agent, AgentObserver, AgentStatus, StreamDelta};
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ContentBlock, ImageAttachmentRef, NewImage,
    SandboxMode, SessionEvent, SessionLog, now_ms,
};
use dshx_server::{Backend, NotFound, PromptRequest, StreamMessage};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

use crate::chats::ChatState;
use crate::runtime::{Overrides, Runtime};

pub async fn run(
    runtime: Arc<Runtime>,
    port: u16,
    open: bool,
    ready_file: Option<PathBuf>,
) -> anyhow::Result<i32> {
    let backend = Arc::new(WebBackend::new(runtime.clone()));
    let token = uuid::Uuid::new_v4().simple().to_string();
    let (addr, server) = dshx_server::serve(backend.clone(), token.clone(), port).await?;
    let url = format!("http://127.0.0.1:{}/?token={token}", addr.port());
    eprintln!("dsh-codex web: {url}");
    if let Some(path) = &ready_file {
        write_ready_file(path, &url, &token, addr.port())
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    }
    if open {
        open_browser(&url);
    }
    tokio::select! {
        _ = server => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminated() => {}
    }
    if let Some(path) = &ready_file {
        let _ = std::fs::remove_file(path);
    }
    backend.close_all().await;
    runtime.shutdown().await;
    Ok(0)
}

/// Resolves on SIGTERM, which a supervising app sends on quit, so the agents
/// and MCP servers shut down cleanly.
async fn terminated() {
    #[cfg(unix)]
    if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        sig.recv().await;
        return;
    }
    std::future::pending::<()>().await
}

/// Writes `{url, token, port}` atomically with mode 0600 so a supervising app
/// learns where the UI listens without parsing stderr.
fn write_ready_file(path: &Path, url: &str, token: &str, port: u16) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    let body = json!({ "url": url, "token": token, "port": port });
    file.write_all(body.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Best effort: headless servers simply have no opener.
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// A session with a live agent in this process.
struct OpenSession {
    agent: Arc<Agent>,
    log: Arc<dyn SessionLog>,
    cwd: PathBuf,
    status: Arc<Mutex<String>>,
    /// MCP tool-set version the agent last received.
    tools_version: std::sync::atomic::AtomicU64,
}

pub struct WebBackend {
    runtime: Arc<Runtime>,
    open: tokio::sync::Mutex<HashMap<String, Arc<OpenSession>>>,
    approvals: Arc<Approvals>,
    tx: broadcast::Sender<StreamMessage>,
    workspaces_file: PathBuf,
    chats: ChatState,
}

impl WebBackend {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        let (tx, _) = broadcast::channel(4096);
        let workspaces_file = runtime.home.join("workspaces.json");
        let chats = ChatState::load(&runtime.home);
        runtime.live.enable_viewers();
        WebBackend {
            approvals: Arc::new(Approvals {
                pending: Mutex::new(HashMap::new()),
                tx: tx.clone(),
            }),
            runtime,
            open: tokio::sync::Mutex::new(HashMap::new()),
            tx,
            workspaces_file,
            chats,
        }
    }

    async fn close_all(&self) {
        let sessions: Vec<_> = self.open.lock().await.drain().map(|(_, s)| s).collect();
        for s in sessions {
            self.runtime.close_agent(&s.agent).await;
        }
    }

    /// The live session, opening (and repairing) it from disk on first use.
    async fn session_handle(
        &self,
        id: &str,
        overrides: &Overrides,
    ) -> anyhow::Result<Arc<OpenSession>> {
        let mut open = self.open.lock().await;
        if let Some(s) = open.get(id) {
            return Ok(s.clone());
        }
        if self.runtime.store.locate(id)?.is_none() {
            return Err(NotFound(id.to_owned()).into());
        }
        let log: Arc<dyn SessionLog> = Arc::new(self.runtime.store.resume(id).await?);
        let cwd = PathBuf::from(log.header().cwd);
        let s = self.start_agent(id, log, cwd, overrides);
        open.insert(id.to_owned(), s.clone());
        Ok(s)
    }

    fn start_agent(
        &self,
        id: &str,
        log: Arc<dyn SessionLog>,
        cwd: PathBuf,
        overrides: &Overrides,
    ) -> Arc<OpenSession> {
        let status = Arc::new(Mutex::new("idle".to_owned()));
        let observer = Arc::new(WebObserver {
            session_id: id.to_owned(),
            tx: self.tx.clone(),
            status: status.clone(),
        });
        let approver = Arc::new(SessionApprover {
            session_id: id.to_owned(),
            approvals: self.approvals.clone(),
        });
        let config = self.runtime.agent_config(&cwd, overrides);
        let agent = Arc::new(
            self.runtime
                .new_agent(config, log.clone(), approver, Some(observer)),
        );
        // Forward every appended event to the UI feed.
        let mut feed = log.subscribe();
        let tx = self.tx.clone();
        let session_id = id.to_owned();
        tokio::spawn(async move {
            loop {
                match feed.recv().await {
                    Ok(event) => {
                        let _ = tx.send(StreamMessage::Event {
                            session_id: session_id.clone(),
                            event,
                        });
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Arc::new(OpenSession {
            agent,
            log,
            cwd,
            status,
            tools_version: self.runtime.tools_version().into(),
        })
    }

    fn info_value(&self) -> Value {
        let config = self.runtime.config();
        let (browser, app, computer) = self.runtime.tool_flags();
        let models: Vec<Value> = dshx_core::LlmClient::models(self.runtime.llm.as_ref())
            .into_iter()
            .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
            .collect();
        let effort = config
            .reasoning_effort
            .clone()
            .unwrap_or_else(|| self.runtime.route(&config.model).default_effort);
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
            "locale": config.codex.computer_use.locale.clone().unwrap_or_else(dshx_browser::system_locale),
            "models": models,
            "settings": {
                "model": config.model,
                "effort": effort,
                "sandbox": config.sandbox,
                "approval": config.approval,
            },
            "apiKeyConfigured": self.runtime.api_key_configured(),
            "tools": { "browser": browser, "app": app, "computer": computer },
        })
    }

    fn saved_workspaces(&self) -> Vec<Value> {
        std::fs::read(&self.workspaces_file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
            .unwrap_or_default()
    }

    fn save_workspaces(&self, list: &[Value]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.runtime.home)?;
        std::fs::write(&self.workspaces_file, serde_json::to_vec_pretty(list)?)?;
        Ok(())
    }
}

fn workspace_row(path: &str, last_used: u64) -> Value {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    json!({ "path": path, "name": name, "lastUsed": last_used })
}

fn parse_sandbox(v: Option<&str>) -> anyhow::Result<Option<SandboxMode>> {
    v.map(|s| {
        serde_json::from_value(Value::String(s.to_owned()))
            .map_err(|_| anyhow::anyhow!("unknown sandbox mode {s:?}"))
    })
    .transpose()
}

#[async_trait]
impl Backend for WebBackend {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(self.info_value())
    }

    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value> {
        if let Some(key) = patch.get("apiKey") {
            self.runtime.set_api_key(key.as_str())?;
        }
        let sandbox = parse_sandbox(patch.get("sandbox").and_then(Value::as_str))?;
        self.runtime.update_config(|c| {
            if let Some(m) = patch.get("model").and_then(Value::as_str) {
                c.model = m.to_owned();
            }
            if let Some(e) = patch.get("effort").and_then(Value::as_str) {
                c.reasoning_effort = Some(e.to_owned());
            }
            if let Some(s) = sandbox {
                c.sandbox = s;
            }
            if let Some(a) = patch.get("approval").and_then(Value::as_str) {
                c.approval = a.to_owned();
            }
        })?;
        Ok(self.info_value())
    }

    async fn workspaces(&self) -> anyhow::Result<Value> {
        let mut by_path: HashMap<String, u64> = HashMap::new();
        for w in self.saved_workspaces() {
            if let (Some(p), Some(t)) = (w["path"].as_str(), w["lastUsed"].as_u64()) {
                by_path.insert(p.to_owned(), t);
            }
        }
        for (path, used) in self.runtime.store.workspaces().unwrap_or_default() {
            let e = by_path.entry(path).or_insert(0);
            *e = (*e).max(used);
        }
        let mut rows: Vec<(String, u64)> = by_path
            .into_iter()
            .filter(|(p, _)| Path::new(p).is_dir() && !self.chats.is_scratch(p))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        Ok(Value::Array(
            rows.iter().map(|(p, t)| workspace_row(p, *t)).collect(),
        ))
    }

    async fn add_workspace(&self, path: String) -> anyhow::Result<Value> {
        let expanded = match path.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => PathBuf::from(&path),
        };
        anyhow::ensure!(expanded.is_absolute(), "path must be absolute");
        let dir = expanded
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("{}: {e}", expanded.display()))?;
        anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
        let path = dir.display().to_string();
        let mut list = self.saved_workspaces();
        list.retain(|w| w["path"].as_str() != Some(path.as_str()));
        let row = workspace_row(&path, now_ms());
        list.insert(0, row.clone());
        self.save_workspaces(&list)?;
        Ok(row)
    }

    async fn list_sessions(&self, cwd: Option<String>) -> anyhow::Result<Value> {
        let mut rows = match &cwd {
            Some(c) => self.runtime.store.list_in(c)?,
            None => self.runtime.store.list()?,
        };
        rows.retain(|r| r.parent_session.is_none());
        let open = self.open.lock().await;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut v = serde_json::to_value(&r)?;
            if let Some(s) = open.get(&r.id) {
                v["status"] = json!(s.status.lock().unwrap().clone());
            }
            self.chats.decorate(&mut v);
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    async fn create_session(&self, cwd: String) -> anyhow::Result<String> {
        // No folder: the chat gets a private one that never shows up as a project.
        let scratch = cwd.trim().is_empty();
        let dir = if scratch {
            self.chats.new_chat_dir()?
        } else {
            PathBuf::from(&cwd)
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("{cwd}: {e}"))?
        };
        anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
        let log: Arc<dyn SessionLog> =
            Arc::new(self.runtime.store.create(&dir.display().to_string())?);
        let id = log.id().to_owned();
        let s = self.start_agent(&id, log, dir.clone(), &Overrides::default());
        self.open.lock().await.insert(id.clone(), s);
        if !scratch {
            let _ = self.add_workspace(dir.display().to_string()).await;
        }
        let _ = self.chats.update(&id, &json!({ "seen": true }), now_ms());
        Ok(id)
    }

    fn live(&self) -> Option<broadcast::Receiver<StreamMessage>> {
        Some(self.runtime.live.subscribe())
    }

    fn live_latest(&self, id: &str) -> Option<StreamMessage> {
        self.runtime.live.latest(id)
    }

    async fn session_root(&self, id: &str) -> anyhow::Result<PathBuf> {
        if let Some(s) = self.open.lock().await.get(id) {
            return Ok(s.cwd.clone());
        }
        let row = self.runtime.store.list()?.into_iter().find(|r| r.id == id);
        Ok(PathBuf::from(
            row.ok_or_else(|| NotFound(id.to_owned()))?.cwd,
        ))
    }

    async fn update_session(&self, id: &str, patch: Value) -> anyhow::Result<Value> {
        if self.runtime.store.locate(id)?.is_none() && !self.open.lock().await.contains_key(id) {
            return Err(NotFound(id.to_owned()).into());
        }
        self.chats.update(id, &patch, now_ms())?;
        let rows = self.list_sessions(None).await?;
        Ok(rows
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["id"] == id).cloned())
            .unwrap_or_else(|| json!({ "id": id })))
    }

    async fn session(&self, id: &str) -> anyhow::Result<Value> {
        if let Some(s) = self.open.lock().await.get(id) {
            let status = s.status.lock().unwrap().clone();
            return Ok(
                json!({ "header": s.log.header(), "events": s.log.events(), "status": status }),
            );
        }
        if self.runtime.store.locate(id)?.is_none() {
            return Err(NotFound(id.to_owned()).into());
        }
        let stored = self.runtime.store.read(id)?;
        Ok(json!({ "header": stored.header, "events": stored.events, "status": "idle" }))
    }

    async fn events_after(
        &self,
        id: &str,
        after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let events = match self.open.lock().await.get(id) {
            Some(s) => s.log.events(),
            None => {
                if self.runtime.store.locate(id)?.is_none() {
                    return Err(NotFound(id.to_owned()).into());
                }
                self.runtime.store.read(id)?.events
            }
        };
        Ok(events
            .into_iter()
            .filter(|e| after.is_none_or(|a| e.seq > a))
            .collect())
    }

    async fn status(&self, id: &str) -> Option<String> {
        let open = self.open.lock().await;
        Some(
            open.get(id)
                .map_or_else(|| "idle".to_owned(), |s| s.status.lock().unwrap().clone()),
        )
    }

    async fn prompt(&self, id: &str, request: PromptRequest) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.runtime.api_key_configured(),
            "No DeepSeek API key yet. Add it in Settings."
        );
        let overrides = Overrides {
            model: request.model.clone(),
            effort: request.effort.clone(),
            sandbox: parse_sandbox(request.sandbox.as_deref())?,
        };
        let s = self.session_handle(id, &overrides).await?;
        // The composer's pickers apply to this chat from now on.
        let route = self.runtime.agent_config(&s.cwd, &overrides);
        s.agent.update_config(|c| {
            c.model = route.model.clone();
            c.effort = route.effort.clone();
            c.route = route.route.clone();
        });
        self.runtime.refresh_agent(&s.agent, &s.tools_version);
        if let Some(mode) = overrides.sandbox {
            let approval = s.agent.config().approval;
            s.agent.set_permissions(mode, approval).await?;
        }
        let mut content = Vec::new();
        if !request.text.is_empty() {
            content.push(ContentBlock::text(request.text));
        }
        for image in request.images {
            content.push(ContentBlock::Image {
                attachment: image,
                offloaded: false,
            });
        }
        anyhow::ensure!(!content.is_empty(), "empty prompt");
        let agent = s.agent.clone();
        let tx = self.tx.clone();
        let session_id = id.to_owned();
        let steer = request.steer;
        tokio::spawn(async move {
            let result = if steer {
                agent.steer(content).await.map(|_| ())
            } else {
                agent.run_turn(content).await.map(|_| ())
            };
            if let Err(e) = result {
                let _ = tx.send(StreamMessage::Status {
                    session_id,
                    status: "error".into(),
                    error: Some(format!("{e:#}")),
                });
            }
        });
        Ok(())
    }

    async fn cancel(&self, id: &str) -> anyhow::Result<()> {
        let s = self.open.lock().await.get(id).cloned();
        if let Some(s) = s {
            self.approvals.cancel_session(id);
            s.agent.cancel().await;
        }
        Ok(())
    }

    async fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        if let Some(s) = self.open.lock().await.remove(id) {
            self.runtime.close_agent(&s.agent).await;
        }
        if self.runtime.store.locate(id)?.is_none() {
            return Err(NotFound(id.to_owned()).into());
        }
        self.runtime.store.delete(id)?;
        self.chats.remove(id);
        Ok(())
    }

    async fn approve(&self, call_id: &str, decision: &str) -> anyhow::Result<()> {
        let decision = if decision == "allowed-once" {
            ApprovalDecision::AllowedOnce
        } else {
            ApprovalDecision::Rejected
        };
        anyhow::ensure!(
            self.approvals.decide(call_id, decision),
            NotFound(call_id.to_owned())
        );
        Ok(())
    }

    async fn save_attachment(
        &self,
        data: Vec<u8>,
        media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        self.runtime
            .attachments
            .save_image(NewImage {
                data,
                media_type,
                name: None,
                width: 0,
                height: 0,
            })
            .await
    }

    async fn load_attachment(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.runtime
            .attachments
            .load(id)
            .await
            .map_err(|_| NotFound(id.to_owned()).into())
    }

    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
}

/// Live deltas and status changes of one session's agent.
struct WebObserver {
    session_id: String,
    tx: broadcast::Sender<StreamMessage>,
    status: Arc<Mutex<String>>,
}

impl AgentObserver for WebObserver {
    fn on_delta(&self, delta: &StreamDelta) {
        let block_type = serde_json::to_value(delta.block_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned));
        let _ = self.tx.send(StreamMessage::Delta {
            session_id: self.session_id.clone(),
            turn: delta.turn,
            step: delta.step,
            index: delta.index,
            block_type: block_type.unwrap_or_else(|| "text".into()),
            text: delta.text.clone(),
        });
    }

    fn on_status(&self, status: &AgentStatus) {
        *self.status.lock().unwrap() = status.as_str().to_owned();
        let error = match status {
            AgentStatus::Error { error } => Some(error.clone()),
            _ => None,
        };
        let _ = self.tx.send(StreamMessage::Status {
            session_id: self.session_id.clone(),
            status: status.as_str().to_owned(),
            error,
        });
    }
}

/// Pending approval questions, answered from the UI.
struct Approvals {
    pending: Mutex<HashMap<String, (String, oneshot::Sender<ApprovalDecision>)>>,
    tx: broadcast::Sender<StreamMessage>,
}

impl Approvals {
    fn decide(&self, call_id: &str, decision: ApprovalDecision) -> bool {
        match self.pending.lock().unwrap().remove(call_id) {
            Some((_, answer)) => answer.send(decision).is_ok(),
            None => false,
        }
    }

    fn cancel_session(&self, session_id: &str) {
        let mut pending = self.pending.lock().unwrap();
        let ids: Vec<String> = pending
            .iter()
            .filter(|(_, (s, _))| s == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some((_, answer)) = pending.remove(&id) {
                let _ = answer.send(ApprovalDecision::Cancelled);
            }
        }
    }
}

struct SessionApprover {
    session_id: String,
    approvals: Arc<Approvals>,
}

#[async_trait]
impl Approver for SessionApprover {
    async fn approve(
        &self,
        call_id: &str,
        tool: &str,
        summary: &str,
        justification: Option<&str>,
    ) -> ApprovalDecision {
        let (answer, wait) = oneshot::channel();
        self.approvals
            .pending
            .lock()
            .unwrap()
            .insert(call_id.to_owned(), (self.session_id.clone(), answer));
        let _ = self.approvals.tx.send(StreamMessage::Approval {
            session_id: self.session_id.clone(),
            call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            justification: justification.map(str::to_owned),
        });
        wait.await.unwrap_or(ApprovalDecision::Cancelled)
    }
}
