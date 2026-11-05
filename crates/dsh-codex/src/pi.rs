//! The Pi engine inside `dsh-codex web`: which chats are Pi chats, their
//! open [`PiChat`]s, the pinned runtime's availability (`/api/info.engines`),
//! model and provider mapping, and the live feed to the UI. DSH chats never
//! pass through here.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dshx_core::{
    AttachmentStore, ContentBlock, Message, MessageSource, Role, SessionLog, Tool, kinds,
};
use dshx_pi::chat::Live;
use dshx_pi::{
    ModelChoice, PI_VERSION, PiChat, PiDirs, PiEnv, PiHost, QueueChange, TurnEndCompaction,
};
use dshx_server::{NotFound, PromptRequest, StreamMessage};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::runtime::Runtime;

/// `DSHX_PI_TEST=1`: Pi also loads the scripted faux model (`pi-faux/faux-1`).
fn test_mode() -> bool {
    std::env::var("DSHX_PI_TEST").as_deref() == Ok("1")
}

const FAUX: &str = "pi-faux/faux-1";

/// The engine of a header: `pi`, else DSH.
pub fn is_pi_header(header: &dshx_core::SessionHeader) -> bool {
    header.extra.get("engine").and_then(Value::as_str) == Some("pi")
}

/// Aether's compaction mode for Pi's turn-end compaction.
pub fn turn_end(settings: &dshx_core::config::CompactionSettings) -> TurnEndCompaction {
    use dshx_core::config::CompactionMode;
    match settings.mode {
        CompactionMode::Threshold => TurnEndCompaction::Threshold(settings.threshold_tokens),
        CompactionMode::EveryTurn => TurnEndCompaction::EveryTurn,
        CompactionMode::Off => TurnEndCompaction::Off,
    }
}

pub struct PiEngine {
    runtime: Arc<Runtime>,
    tx: broadcast::Sender<StreamMessage>,
    env: OnceLock<Result<Arc<PiEnv>, String>>,
    chats: Mutex<HashMap<String, PiChat>>,
    /// The open chats' logs, to release their locks on delete and shutdown.
    files: Mutex<HashMap<String, Arc<dshx_session::FileLog>>>,
    /// Session id → Pi chat (the engine is fixed per chat, so this caches).
    kinds: Mutex<HashMap<String, bool>>,
    /// Serializes opening a chat.
    opening: tokio::sync::Mutex<()>,
}

struct Host {
    runtime: Arc<Runtime>,
    tx: broadcast::Sender<StreamMessage>,
}

impl PiHost for Host {
    fn live(&self, session: &str, live: Live) {
        let message = match live {
            Live::Delta(d) => StreamMessage::Delta {
                session_id: session.to_owned(),
                turn: d.turn,
                step: d.step,
                index: d.index,
                block_type: d.block_type.to_owned(),
                text: d.text,
            },
            Live::Status { status, error } => StreamMessage::Status {
                session_id: session.to_owned(),
                status,
                error,
            },
        };
        let _ = self.tx.send(message);
    }

    fn providers(&self) -> Vec<Value> {
        self.runtime.pi_providers()
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(dshx_tools::todo::TodoWriteTool)]
    }

    fn attachments(&self) -> Arc<dyn AttachmentStore> {
        self.runtime.attachments.clone()
    }

    fn approver(&self) -> Arc<dyn dshx_core::Approver> {
        Arc::new(crate::runtime::NoApprover)
    }
}

impl PiEngine {
    pub fn new(runtime: Arc<Runtime>, tx: broadcast::Sender<StreamMessage>) -> Arc<PiEngine> {
        let engine = Arc::new(PiEngine {
            runtime,
            tx,
            env: OnceLock::new(),
            chats: Mutex::new(HashMap::new()),
            files: Mutex::new(HashMap::new()),
            kinds: Mutex::new(HashMap::new()),
            opening: tokio::sync::Mutex::new(()),
        });
        // Resolve the runtime off the request path (spawns Node once).
        let me = Arc::downgrade(&engine);
        std::thread::spawn(move || {
            if let Some(e) = me.upgrade() {
                let _ = e.env();
            }
        });
        // Idle processes end after `[codex.pi] idle_secs`.
        let me = Arc::downgrade(&engine);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let Some(e) = me.upgrade() else { break };
                    let idle = e.runtime.config().codex.pi.idle_secs;
                    if let Some(Ok(env)) = e.env.get() {
                        env.pool.reap(Duration::from_secs(idle)).await;
                    }
                }
            });
        }
        engine
    }

    fn dirs(&self) -> PiDirs {
        PiDirs::new(self.runtime.home.join("pi"))
    }

    /// The prepared engine, or why Pi cannot run on this host.
    fn env(&self) -> Result<Arc<PiEnv>, String> {
        self.env
            .get_or_init(|| {
                let install = dshx_pi::install::resolve(&self.runtime.home)?;
                let max = self.runtime.config().codex.pi.max_processes;
                PiEnv::prepare(
                    install,
                    self.dirs(),
                    test_mode(),
                    dshx_tools::sandbox::host_runner(),
                    max,
                )
                .map(Arc::new)
                .map_err(|e| format!("preparing {}: {e:#}", self.dirs().root.display()))
            })
            .clone()
    }

    /// The `/api/info.engines` rows.
    pub fn engines(&self) -> Value {
        // Resolved once (the background check usually finished already).
        let pi = match self.env() {
            Ok(env) => json!({
                "id": "pi", "name": "Pi", "available": true,
                "version": env.install.version,
                "processes": env.pool.running(), "maxProcesses": env.pool.max(),
            }),
            Err(reason) => {
                json!({"id": "pi", "name": "Pi", "available": false, "version": PI_VERSION, "reason": reason})
            }
        };
        let mut pi = pi;
        if test_mode() {
            pi["testModel"] = FAUX.into();
        }
        json!([
            {"id": "dsh", "name": "DSH", "available": true, "version": env!("CARGO_PKG_VERSION")},
            pi,
        ])
    }

    /// The faux model's picker row while `DSHX_PI_TEST=1`.
    pub fn test_model_row(&self) -> Option<Value> {
        test_mode().then(|| {
            json!({
                "id": FAUX, "provider": "pi-faux", "providerName": "Pi test", "model": "faux-1",
                "name": "Faux 1 (Pi test model)", "contextWindow": 64000, "maxTokens": 4096,
                "inputModalities": ["text", "image"], "reasoningEfforts": ["off", "high"],
                "defaultReasoningEffort": "high", "available": true, "engines": ["pi"],
            })
        })
    }

    /// The open chat, if any.
    pub fn open_chat(&self, id: &str) -> Option<PiChat> {
        self.chats.lock().unwrap().get(id).cloned()
    }

    /// Status of an open Pi chat.
    pub fn status(&self, id: &str) -> Option<String> {
        self.open_chat(id).map(|c| c.status())
    }

    /// Whether `id` is a Pi chat (header `engine: "pi"`).
    pub fn is_pi(&self, id: &str) -> anyhow::Result<bool> {
        if let Some(known) = self.kinds.lock().unwrap().get(id) {
            return Ok(*known);
        }
        let Some(located) = self.runtime.store.locate(id)? else {
            return Ok(false);
        };
        // Only the header line: cheap even for a long DSH chat.
        let pi = is_pi_header(&dshx_session::reader::read_header_file(&located.path)?);
        self.kinds.lock().unwrap().insert(id.to_owned(), pi);
        Ok(pi)
    }

    fn host(&self) -> Arc<dyn PiHost> {
        Arc::new(Host {
            runtime: self.runtime.clone(),
            tx: self.tx.clone(),
        })
    }

    /// Forwards every appended event to the UI feed.
    fn forward(&self, id: &str, log: &Arc<dyn SessionLog>) {
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
    }

    fn start(
        &self,
        id: &str,
        file: Arc<dshx_session::FileLog>,
        cwd: PathBuf,
    ) -> anyhow::Result<PiChat> {
        let env = self
            .env()
            .map_err(|r| anyhow::anyhow!("Pi is not available: {r}"))?;
        let log: Arc<dyn SessionLog> = file.clone();
        self.forward(id, &log);
        self.files.lock().unwrap().insert(id.to_owned(), file);
        let config = self.runtime.config();
        let chat = PiChat::open(
            id,
            log,
            cwd,
            env,
            self.host(),
            config.sandbox,
            turn_end(&config.codex.compaction),
        );
        self.chats
            .lock()
            .unwrap()
            .insert(id.to_owned(), chat.clone());
        self.kinds.lock().unwrap().insert(id.to_owned(), true);
        Ok(chat)
    }

    /// A new Pi chat in `dir` (header `engine: "pi"`, `engineVersion`).
    pub fn create(&self, dir: &std::path::Path) -> anyhow::Result<String> {
        self.env()
            .map_err(|r| anyhow::anyhow!("Pi is not available: {r}"))?;
        let mut header = dshx_session::SessionStore::new_header(&dir.display().to_string());
        header.extra.insert("engine".into(), json!("pi"));
        header
            .extra
            .insert("engineVersion".into(), json!(PI_VERSION));
        let file = Arc::new(self.runtime.store.create_with_header(header)?);
        let id = file.id().to_owned();
        self.start(&id, file, dir.to_path_buf())?;
        Ok(id)
    }

    /// The chat, opened (and repaired) from disk on first use.
    pub async fn chat(&self, id: &str) -> anyhow::Result<PiChat> {
        if let Some(c) = self.open_chat(id) {
            return Ok(c);
        }
        let _guard = self.opening.lock().await;
        if let Some(c) = self.open_chat(id) {
            return Ok(c);
        }
        if self.runtime.store.locate(id)?.is_none() {
            return Err(NotFound(id.to_owned()).into());
        }
        let file = Arc::new(self.runtime.store.resume(id).await?);
        let cwd = PathBuf::from(file.header().cwd);
        let chat = self.start(id, file, cwd)?;
        chat.resume_pending();
        Ok(chat)
    }

    /// Aether's model reference → Aether and Pi names.
    fn choice(&self, reference: &str, effort: Option<&str>) -> anyhow::Result<ModelChoice> {
        let config = self.runtime.config();
        if reference == FAUX {
            anyhow::ensure!(test_mode(), "the Pi test model needs DSHX_PI_TEST=1");
            return Ok(ModelChoice {
                provider: "pi-faux".into(),
                model: "faux-1".into(),
                effort: effort.unwrap_or("high").to_owned(),
                pi_provider: "pi-faux".into(),
                pi_model: "faux-1".into(),
                context_window: Some(64000),
            });
        }
        self.runtime
            .model_ready(reference)
            .map_err(anyhow::Error::msg)?;
        let (provider, model) = self.runtime.split_model(reference);
        let route = self.runtime.route(reference);
        let effort = effort
            .map(str::to_owned)
            .or(config.reasoning_effort.clone())
            .unwrap_or(route.default_effort.clone());
        let pi_provider = if provider == dshx_llm::PROVIDER {
            "deepseek".to_owned()
        } else {
            format!("aether-{provider}")
        };
        Ok(ModelChoice {
            provider,
            pi_model: model.clone(),
            model,
            effort,
            pi_provider,
            context_window: route.context_window,
        })
    }

    /// The chat's current model: the last `model/selection`, else the default.
    fn current_choice(&self, chat: &PiChat) -> anyhow::Result<ModelChoice> {
        let events = chat.log().events();
        let last = events
            .iter()
            .rev()
            .find(|e| e.kind == kinds::MODEL_SELECTION);
        match last {
            Some(e) => {
                let provider = e.data["provider"].as_str().unwrap_or_default();
                let model = e.data["model"].as_str().unwrap_or_default();
                let reference = crate::pi::model_ref(provider, model);
                self.choice(&reference, e.data["reasoningEffort"].as_str())
            }
            None => self.choice(&self.runtime.config().model, None),
        }
    }

    pub async fn prompt(&self, id: &str, request: PromptRequest) -> anyhow::Result<()> {
        if let Some(message_id) = &request.message_id {
            anyhow::ensure!(
                !message_id.is_empty()
                    && message_id.chars().count() <= 128
                    && !message_id.chars().any(char::is_control),
                "invalid messageId"
            );
        }
        let chat = self.chat(id).await?;
        let choice = match request.model.as_deref() {
            Some(reference) => self.choice(reference, request.effort.as_deref())?,
            None => {
                let mut c = self.current_choice(&chat)?;
                if let Some(e) = &request.effort {
                    c.effort = e.clone();
                }
                c
            }
        };
        let sandbox = request
            .sandbox
            .as_deref()
            .map(|s| {
                dshx_tools::sandbox::parse_mode(s)
                    .ok_or_else(|| anyhow::anyhow!("unknown sandbox mode {s:?}"))
            })
            .transpose()?;
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
        let source = match request
            .time_zone
            .as_deref()
            .filter(|z| !z.is_empty() && z.len() <= 64)
        {
            Some(zone) => MessageSource::Other(
                json!({ "kind": "user", "rpcId": dshx_core::new_id(), "clientTimeZone": zone }),
            ),
            None => MessageSource::User,
        };
        let message = Message {
            id: request.message_id.unwrap_or_else(dshx_core::new_id),
            role: Role::User,
            content,
            source,
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        };
        chat.prompt(message, request.steer, choice, sandbox).await
    }

    pub async fn cancel(&self, id: &str) -> anyhow::Result<()> {
        match self.open_chat(id) {
            Some(chat) => chat.cancel().await,
            None => Ok(()),
        }
    }

    pub async fn compact(&self, id: &str) -> anyhow::Result<Value> {
        let chat = self.chat(id).await?;
        let choice = self.current_choice(&chat)?;
        chat.compact(choice).await
    }

    pub async fn update_queue(&self, id: &str, item: &str, body: &Value) -> anyhow::Result<()> {
        let Some(chat) = self.open_chat(id) else {
            anyhow::bail!("queued item is no longer pending");
        };
        let change = match body["action"].as_str() {
            Some("edit") => QueueChange::Edit(body["text"].as_str().unwrap_or_default().to_owned()),
            Some("remove") => QueueChange::Remove,
            Some("steer") => QueueChange::Steer,
            _ => anyhow::bail!("action must be \"edit\", \"remove\" or \"steer\""),
        };
        chat.change_queued(item, change).await
    }

    /// New compaction settings reach open Pi chats at once.
    pub async fn set_compaction(&self, settings: &dshx_core::config::CompactionSettings) {
        let chats: Vec<PiChat> = self.chats.lock().unwrap().values().cloned().collect();
        for chat in chats {
            chat.set_compaction(turn_end(settings)).await;
        }
    }

    /// Before the log is deleted: the process ends and Pi's session file goes.
    pub async fn forget(&self, id: &str) {
        let chat = self.chats.lock().unwrap().remove(id);
        if let Some(chat) = chat {
            chat.close().await;
        }
        let file = self.files.lock().unwrap().remove(id);
        if let Some(file) = file {
            let _ = file.close().await;
        }
        if self.kinds.lock().unwrap().remove(id) == Some(true) {
            for file in self.dirs().session_files(id) {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    pub async fn close_all(&self) {
        let chats: Vec<PiChat> = self.chats.lock().unwrap().drain().map(|(_, c)| c).collect();
        for chat in chats {
            chat.close().await;
        }
        let files: Vec<_> = self.files.lock().unwrap().drain().map(|(_, f)| f).collect();
        for file in files {
            let _ = file.close().await;
        }
    }

    /// `session_stats` inputs of a Pi chat: its events and the context
    /// window of its latest request.
    pub fn stats_input(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<(Vec<dshx_core::SessionEvent>, Option<u64>)>> {
        if !self.is_pi(id)? {
            return Ok(None);
        }
        let events = match self.open_chat(id) {
            Some(c) => c.log().events(),
            None => self.runtime.store.read(id)?.events,
        };
        let window = events
            .iter()
            .rev()
            .find(|e| e.kind == kinds::REQUEST_CONTEXT)
            .and_then(|e| e.data["contextWindow"].as_u64());
        Ok(Some((events, window)))
    }
}

/// `/api/info.models` rows say which engines can run them; every Aether
/// provider reaches Pi through the bridge. The faux test model joins in
/// test mode.
pub fn tag_models(models: &mut Vec<Value>, test_row: Option<Value>) {
    for row in models.iter_mut() {
        row["engines"] = json!(["dsh", "pi"]);
    }
    models.extend(test_row);
}

/// `defaultEngine` of a settings patch.
pub fn parse_engine(value: Option<&Value>) -> anyhow::Result<Option<String>> {
    match value {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(e @ ("dsh" | "pi")) => Ok(Some(e.to_owned())),
            _ => anyhow::bail!("defaultEngine must be \"dsh\" or \"pi\""),
        },
    }
}

/// The picker's model reference: bare DeepSeek ids, `provider/model` otherwise.
pub fn model_ref(provider: &str, model: &str) -> String {
    if provider.is_empty() || provider == dshx_llm::PROVIDER {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    }
}
