//! One Pi chat: its Aether log, the queue, the Pi process with its bridge,
//! and the driver that turns prompts, steers, Stop and crashes into log
//! events through the [`Mapper`].
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use dshx_core::{
    Approver, AttachmentStore, ContentBlock, Message, NewEvent, NewImage, SandboxMode, SessionLog,
    Tool, ToolContext, ToolResultPart,
};
use dshx_tools::sandbox::{self, Runner};
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bridge::{Bridge, BridgeHost};
use crate::install::Install;
use crate::mapper::{Delta, Mapper, QueueChange, Route, Settled, Target, TurnEnd, prompt_text};
use crate::pool::{Pool, Stopper};
use crate::process::{Output, PiDirs, PiProcess, Spawn};

/// The bridge extension, written to `$DSHX_HOME/pi/ext/aether-bridge-<sha8>.ts`.
pub const BRIDGE_TS: &str = include_str!("../extension/aether-bridge.ts");
/// The scripted test model (`pi-faux/faux-1`), only with `DSHX_PI_TEST=1`.
pub const FAUX_TS: &str = include_str!("../extension/aether-faux.ts");

const START_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ABORT_TIMEOUT: Duration = Duration::from_secs(10);
const COMPACT_TIMEOUT: Duration = Duration::from_secs(600);

/// Writes `source` as `<dir>/<name>-<sha8>.ts` (content-addressed, so a new
/// build never runs an old file) and returns the path.
pub fn write_extension(dir: &Path, name: &str, source: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let digest = sha2::Sha256::digest(source.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{name}-{short}.ts"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(source) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, source)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// Engine-wide settings shared by every Pi chat.
pub struct PiEnv {
    pub install: Install,
    pub dirs: PiDirs,
    /// `-e` files: the bridge, plus the faux model in tests.
    pub extensions: Vec<PathBuf>,
    pub runner: Runner,
    pub pool: Arc<Pool>,
}

impl PiEnv {
    /// Creates the folders and writes the extensions.
    pub fn prepare(
        install: Install,
        dirs: PiDirs,
        faux: bool,
        runner: Runner,
        max_processes: usize,
    ) -> anyhow::Result<PiEnv> {
        dirs.create()?;
        let mut extensions = vec![write_extension(&dirs.ext(), "aether-bridge", BRIDGE_TS)?];
        if faux {
            extensions.push(write_extension(&dirs.ext(), "aether-faux", FAUX_TS)?);
        }
        Ok(PiEnv {
            install,
            dirs,
            extensions,
            runner,
            pool: Pool::new(max_processes),
        })
    }
}

/// Live output for the UI.
#[derive(Debug, Clone)]
pub enum Live {
    Delta(Delta),
    Status {
        status: String,
        error: Option<String>,
    },
}

/// What a chat needs from Aether.
pub trait PiHost: Send + Sync {
    fn live(&self, session: &str, live: Live);
    /// `pi.registerProvider` configs, keys included (bridge only).
    fn providers(&self) -> Vec<Value>;
    /// Aether tools offered through the bridge.
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn attachments(&self) -> Arc<dyn AttachmentStore>;
    fn approver(&self) -> Arc<dyn Approver>;
}

/// The model of the next prompt, in Aether's and in Pi's names.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    /// The composer's effort; mapped onto Pi's thinking levels.
    pub effort: String,
    pub pi_provider: String,
    pub pi_model: String,
    pub context_window: Option<u64>,
}

/// Aether's turn-end compaction mode (`[codex.compaction]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TurnEndCompaction {
    Off,
    EveryTurn,
    Threshold(u64),
}

const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The thinking level for `effort` among `available` (the nearest lower
/// one, else the lowest).
pub fn thinking_level(effort: &str, available: &[String]) -> String {
    let wanted = match effort {
        "none" | "disabled" => "off",
        other => other,
    };
    if available.iter().any(|l| l == wanted) {
        return wanted.to_owned();
    }
    let rank = |l: &str| LEVELS.iter().position(|x| *x == l);
    let target = rank(wanted).unwrap_or(3);
    available
        .iter()
        .filter(|l| rank(l).is_some_and(|r| r <= target))
        .max_by_key(|l| rank(l))
        .or_else(|| available.iter().min_by_key(|l| rank(l)))
        .cloned()
        .unwrap_or_else(|| "off".into())
}

struct Running {
    process: Arc<PiProcess>,
    bridge: Bridge,
    generation: u64,
}

struct State {
    mapper: Mapper,
    process: Option<Running>,
    /// Stop was pressed: nothing continues by itself until the next prompt.
    stopping: bool,
    sandbox: SandboxMode,
    restart: bool,
    want: Option<ModelChoice>,
    applied: Option<(String, String, String)>,
    selection: Option<(String, String, String)>,
    compaction: TurnEndCompaction,
    compacting: bool,
    context_tokens: u64,
}

struct Inner {
    id: String,
    cwd: PathBuf,
    log: Arc<dyn SessionLog>,
    env: Arc<PiEnv>,
    host: Arc<dyn PiHost>,
    state: tokio::sync::Mutex<State>,
    status: Mutex<String>,
    sandbox: Arc<Mutex<SandboxMode>>,
    generation: AtomicU64,
    /// The last turn of the log was stopped (no automatic resume).
    stopped_before: bool,
}

/// A Pi chat.
#[derive(Clone)]
pub struct PiChat {
    inner: Arc<Inner>,
}

impl PiChat {
    /// Opens the chat over its (repaired) log.
    pub fn open(
        id: &str,
        log: Arc<dyn SessionLog>,
        cwd: PathBuf,
        env: Arc<PiEnv>,
        host: Arc<dyn PiHost>,
        sandbox: SandboxMode,
        compaction: TurnEndCompaction,
    ) -> PiChat {
        let events = log.events();
        let stopped_before = events
            .iter()
            .rev()
            .find(|e| e.kind == dshx_core::kinds::TURN_END)
            .is_some_and(|e| e.data.pointer("/reason/kind") == Some(&json!("aborted")));
        let selection = events
            .iter()
            .rev()
            .find(|e| e.kind == dshx_core::kinds::MODEL_SELECTION)
            .map(|e| {
                let s = |k: &str| e.data[k].as_str().unwrap_or_default().to_owned();
                (s("provider"), s("model"), s("reasoningEffort"))
            });
        let sandbox = events
            .iter()
            .rev()
            .find(|e| e.kind == dshx_core::kinds::SANDBOX_MODE)
            .and_then(|e| e.data["mode"].as_str().and_then(sandbox::parse_mode))
            .unwrap_or(sandbox);
        let mapper = Mapper::resume(cwd.clone(), &events);
        PiChat {
            inner: Arc::new(Inner {
                id: id.to_owned(),
                cwd,
                log,
                env,
                host,
                state: tokio::sync::Mutex::new(State {
                    mapper,
                    process: None,
                    stopping: false,
                    sandbox,
                    restart: false,
                    want: None,
                    applied: None,
                    selection,
                    compaction,
                    compacting: false,
                    context_tokens: 0,
                }),
                status: Mutex::new("idle".into()),
                sandbox: Arc::new(Mutex::new(sandbox)),
                generation: AtomicU64::new(0),
                stopped_before,
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    pub fn log(&self) -> Arc<dyn SessionLog> {
        self.inner.log.clone()
    }

    pub fn status(&self) -> String {
        self.inner.status.lock().unwrap().clone()
    }

    /// The access mode the next prompt runs under.
    pub fn sandbox(&self) -> SandboxMode {
        *self.inner.sandbox.lock().unwrap()
    }

    /// Context tokens of the latest request (for the stats page).
    pub async fn context_tokens(&self) -> u64 {
        self.inner.state.lock().await.context_tokens
    }

    /// Continues queued prompts a restart left behind (not after a Stop).
    pub fn resume_pending(&self) {
        if !self.inner.stopped_before {
            tokio::spawn(self.inner.clone().drive());
        }
    }

    pub async fn set_compaction(&self, mode: TurnEndCompaction) {
        self.inner.state.lock().await.compaction = mode;
    }

    /// A prompt from the composer: queued durably, then started, steered
    /// into the running turn (`steer`) or left in the queue.
    pub async fn prompt(
        &self,
        message: Message,
        steer: bool,
        model: ModelChoice,
        sandbox: Option<SandboxMode>,
    ) -> anyhow::Result<()> {
        self.inner
            .clone()
            .prompt(message, steer, model, sandbox)
            .await
    }

    /// Stop: Pi's queued steers come back to Aether's queue first, then
    /// Pi aborts (killed after 10 s); nothing continues by itself.
    pub async fn cancel(&self) -> anyhow::Result<()> {
        self.inner.clone().cancel().await
    }

    /// `/compact` between turns.
    pub async fn compact(&self, model: ModelChoice) -> anyhow::Result<Value> {
        self.inner.clone().compact(model).await
    }

    pub async fn change_queued(&self, item: &str, change: QueueChange) -> anyhow::Result<()> {
        self.inner.clone().change_queued(item, change).await
    }

    /// Ends the process (and a running turn) for good.
    pub async fn close(&self) {
        self.inner.clone().close().await
    }

    /// Stops the process if the chat is idle; `true` when nothing runs now.
    pub async fn stop_if_idle(&self) -> bool {
        self.inner.clone().stop_if_idle().await
    }

    /// Re-sends the provider configs (a key changed).
    pub async fn push_providers(&self) {
        let st = self.inner.state.lock().await;
        if let Some(r) = &st.process {
            r.bridge
                .push(json!({"type": "providers", "providers": self.inner.host.providers()}));
        }
    }
}

impl Inner {
    async fn append(&self, events: Vec<NewEvent>) {
        if events.is_empty() {
            return;
        }
        if let Err(e) = self.log.append(events).await {
            tracing::warn!(session = %self.id, "pi chat: log append failed: {e:#}");
        }
    }

    async fn flush(&self) {
        if let Err(e) = self.log.flush().await {
            tracing::warn!(session = %self.id, "pi chat: log flush failed: {e:#}");
        }
    }

    fn set_status(&self, status: &str, error: Option<String>) {
        *self.status.lock().unwrap() = status.to_owned();
        self.host.live(
            &self.id,
            Live::Status {
                status: status.to_owned(),
                error,
            },
        );
    }

    fn stopper(self: &Arc<Self>) -> Stopper {
        let me = Arc::downgrade(self);
        Arc::new(move || {
            let me: Weak<Inner> = me.clone();
            Box::pin(async move {
                match me.upgrade() {
                    Some(inner) => inner.stop_if_idle().await,
                    None => true,
                }
            })
        })
    }

    async fn images(&self, message: &Message) -> Vec<Value> {
        let store = self.host.attachments();
        let mut out = Vec::new();
        for block in &message.content {
            if let ContentBlock::Image { attachment, .. } = block
                && let Ok((bytes, media)) = store.load(&attachment.attachment_id).await
            {
                out.push(json!({
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "mimeType": media,
                }));
            }
        }
        out
    }

    async fn prompt(
        self: Arc<Self>,
        message: Message,
        steer: bool,
        model: ModelChoice,
        sandbox: Option<SandboxMode>,
    ) -> anyhow::Result<()> {
        let mut st = self.state.lock().await;
        st.stopping = false;
        if let Some(mode) = sandbox
            && mode != st.sandbox
        {
            st.sandbox = mode;
            *self.sandbox.lock().unwrap() = mode;
            let events = st.mapper.sandbox_mode(sandbox::mode_name(mode));
            self.append(events).await;
            // The fence wraps the whole process: Pi restarts (and resumes).
            st.restart = st.process.is_some();
        }
        st.want = Some(model);
        if st.mapper.running().is_some() {
            if steer {
                let text = prompt_text(&message);
                let images = self.images(&message).await;
                let events = st.mapper.enqueue(Target::NextStep, message);
                self.append(events).await;
                self.flush().await;
                if let Some(r) = &st.process {
                    let mut cmd =
                        json!({"type": "prompt", "message": text, "streamingBehavior": "steer"});
                    if !images.is_empty() {
                        cmd["images"] = images.into();
                    }
                    if let Err(e) = r.process.rpc.request(cmd, COMMAND_TIMEOUT).await {
                        // It stays queued and becomes the next turn's prompt.
                        tracing::warn!(session = %self.id, "pi steer: {e:#}");
                    }
                }
            } else {
                let events = st.mapper.enqueue(Target::NextTurn, message);
                self.append(events).await;
                self.flush().await;
            }
            return Ok(());
        }
        let events = st.mapper.enqueue(Target::NextTurn, message);
        self.append(events).await;
        self.flush().await;
        drop(st);
        tokio::spawn(self.clone().drive());
        Ok(())
    }

    /// Starts the next queued turn when the chat is free (boxed: the
    /// process it may start feeds events that drive again).
    fn drive(self: Arc<Self>) -> crate::pool::BoxFuture<()> {
        Box::pin(self.drive_now())
    }

    async fn drive_now(self: Arc<Self>) {
        let need_slot = {
            let st = self.state.lock().await;
            if st.stopping
                || st.compacting
                || st.mapper.running().is_some()
                || st.mapper.queued_turns().is_empty()
            {
                return;
            }
            st.process.is_none() || st.restart
        };
        if need_slot {
            self.env.pool.acquire(&self.id, self.stopper()).await;
        }
        let mut st = self.state.lock().await;
        if st.stopping
            || st.compacting
            || st.mapper.running().is_some()
            || st.mapper.queued_turns().is_empty()
        {
            if st.process.is_none() {
                self.env.pool.release(&self.id);
            }
            return;
        }
        self.env.pool.set_busy(&self.id, true);
        let ready = match self.ensure_process(&mut st).await {
            Ok(()) => self.ensure_model(&mut st).await,
            Err(e) => Err(e),
        };
        let Some((events, message)) = st.mapper.begin_turn() else {
            return;
        };
        self.append(events).await;
        if let Err(e) = ready {
            let message = format!("{e:#}");
            let events = st.mapper.close_turn(TurnEnd::Error {
                code: "ENGINE_UNAVAILABLE".into(),
                message: message.clone(),
            });
            self.append(events).await;
            self.flush().await;
            self.env.pool.set_busy(&self.id, false);
            self.set_status("error", Some(message));
            return;
        }
        self.set_status("running", None);
        let text = prompt_text(&message);
        let images = self.images(&message).await;
        let mut cmd = json!({"type": "prompt", "message": text});
        if !images.is_empty() {
            cmd["images"] = images.into();
        }
        let process = st.process.as_ref().map(|r| r.process.clone());
        let answer = match process {
            Some(p) => p.rpc.request(cmd, COMMAND_TIMEOUT).await,
            None => Err(anyhow::anyhow!("the Pi engine is not running")),
        };
        match answer {
            Ok(data) if data["disposition"] == "handled" => {
                let events = st.mapper.close_turn(TurnEnd::Interrupted);
                self.append(events).await;
                self.env.pool.set_busy(&self.id, false);
                self.set_status("idle", None);
            }
            Ok(_) => {}
            Err(e) => {
                let message = format!("{e:#}");
                let events = st.mapper.close_turn(TurnEnd::Error {
                    code: "ENGINE_REFUSED".into(),
                    message: message.clone(),
                });
                self.append(events).await;
                self.flush().await;
                self.env.pool.set_busy(&self.id, false);
                self.set_status("error", Some(message));
            }
        }
    }

    async fn ensure_process(self: &Arc<Self>, st: &mut State) -> anyhow::Result<()> {
        if st.restart {
            st.restart = false;
            if let Some(r) = st.process.take() {
                r.bridge.stop();
                r.process.shutdown().await;
            }
            st.applied = None;
        }
        if st.process.as_ref().is_some_and(|r| !r.process.has_exited()) {
            return Ok(());
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let digest = sha2::Sha256::digest(self.id.as_bytes());
        let short: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let socket = self.env.dirs.run().join(format!("{short}.sock"));
        let host: Arc<dyn BridgeHost> = Arc::new(ChatBridge {
            inner: Arc::downgrade(self),
        });
        let bridge = Bridge::start(&socket, host)?;
        let spawn = Spawn {
            install: self.env.install.clone(),
            dirs: self.env.dirs.clone(),
            cwd: self.cwd.clone(),
            session_id: self.id.clone(),
            name: None,
            extensions: self.env.extensions.clone(),
            bridge: Some((socket, bridge.token().to_owned())),
            sandbox: st.sandbox,
            runner: self.env.runner.clone(),
        };
        let (process, events) = PiProcess::spawn(&spawn)?;
        let process = Arc::new(process);
        tokio::spawn(self.clone().events(generation, events));
        st.process = Some(Running {
            process: process.clone(),
            bridge,
            generation,
        });
        st.applied = None;
        // Ready once Pi answers (extensions loaded, bridge configured).
        if let Err(e) = process
            .rpc
            .request(json!({"type": "get_state"}), START_TIMEOUT)
            .await
        {
            let tail = process.stderr_tail(1500);
            anyhow::bail!(
                "Pi did not start: {e:#}{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("\n{tail}")
                }
            );
        }
        Ok(())
    }

    async fn ensure_model(&self, st: &mut State) -> anyhow::Result<()> {
        let want = st
            .want
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no model chosen for this Pi chat"))?;
        let process = st
            .process
            .as_ref()
            .map(|r| r.process.clone())
            .ok_or_else(|| anyhow::anyhow!("the Pi engine is not running"))?;
        let key = (
            want.pi_provider.clone(),
            want.pi_model.clone(),
            want.effort.clone(),
        );
        if st.applied.as_ref() == Some(&key) {
            return Ok(());
        }
        process
            .rpc
            .request(
                json!({"type": "set_model", "provider": want.pi_provider, "modelId": want.pi_model}),
                COMMAND_TIMEOUT,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Pi cannot use {}/{}: {e:#}",
                    want.pi_provider,
                    want.pi_model
                )
            })?;
        let levels: Vec<String> = process
            .rpc
            .request(
                json!({"type": "get_available_thinking_levels"}),
                COMMAND_TIMEOUT,
            )
            .await
            .ok()
            .and_then(|d| serde_json::from_value(d["levels"].clone()).ok())
            .unwrap_or_default();
        let level = thinking_level(&want.effort, &levels);
        if let Err(e) = process
            .rpc
            .request(
                json!({"type": "set_thinking_level", "level": level}),
                COMMAND_TIMEOUT,
            )
            .await
        {
            tracing::debug!("pi set_thinking_level {level}: {e:#}");
        }
        st.mapper.set_route(
            &want.pi_provider,
            &want.pi_model,
            Route {
                provider: want.provider.clone(),
                model: want.model.clone(),
                context_window: want.context_window,
            },
        );
        st.applied = Some(key);
        let selection = (want.provider.clone(), want.model.clone(), level.clone());
        if st.selection.as_ref() != Some(&selection) {
            let events = st
                .mapper
                .model_selection(&want.provider, &want.model, &level);
            self.append(events).await;
            st.selection = Some(selection);
        }
        Ok(())
    }

    async fn events(self: Arc<Self>, generation: u64, mut rx: mpsc::UnboundedReceiver<Output>) {
        while let Some(output) = rx.recv().await {
            match output {
                Output::Record(record) => self.clone().on_record(generation, record).await,
                Output::Exited { code, stderr } => {
                    self.on_exit(generation, code, stderr).await;
                    break;
                }
            }
        }
    }

    /// Pi's base64 images in a tool result become stored attachments.
    async fn store_images(&self, record: &mut Value) {
        let Some(blocks) = record
            .pointer_mut("/result/content")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        let store = self.host.attachments();
        for block in blocks.iter_mut() {
            if block["type"] != "image" {
                continue;
            }
            let data = block["data"].as_str().unwrap_or_default();
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
                continue;
            };
            let media = block["mimeType"].as_str().unwrap_or("image/png").to_owned();
            if let Ok(attachment) = store
                .save_image(NewImage {
                    data: bytes,
                    media_type: media,
                    name: None,
                    width: 0,
                    height: 0,
                })
                .await
            {
                *block = json!({"type": "aether-image", "attachment": attachment});
            }
        }
    }

    async fn on_record(self: Arc<Self>, generation: u64, mut record: Value) {
        if record["type"] == "tool_execution_end" {
            self.store_images(&mut record).await;
        }
        if record["type"] == "extension_error" {
            tracing::warn!(session = %self.id, "pi extension error: {}", record["error"]);
        }
        let mut st = self.state.lock().await;
        let Some(process) = st
            .process
            .as_ref()
            .filter(|r| r.generation == generation)
            .map(|r| r.process.clone())
        else {
            return;
        };
        if record["type"] == "message_end"
            && record["message"]["role"] == "assistant"
            && let Some(u) = record["message"]["usage"].as_object()
        {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let tokens = n("input") + n("cacheRead") + n("cacheWrite") + n("output");
            if tokens > 0 {
                st.context_tokens = tokens;
            }
        }
        let out = st.mapper.on_record(&record);
        self.append(out.events).await;
        for delta in out.deltas {
            self.host.live(&self.id, Live::Delta(delta));
        }
        if let Some(id) = out.dialog {
            let _ = process
                .rpc
                .send(&json!({"type": "extension_ui_response", "id": id, "cancelled": true}))
                .await;
        }
        let Some(settled) = out.settled else { return };
        self.env.pool.set_busy(&self.id, false);
        let cursor = st.mapper.cursor().map(str::to_owned);
        let mut cmd = json!({"type": "get_entries"});
        if let Some(c) = &cursor {
            cmd["since"] = json!(c);
        }
        if let Ok(data) = process.rpc.request(cmd, Duration::from_secs(5)).await
            && let Some(leaf) = data["leafId"].as_str()
        {
            let events = st.mapper.mark_cursor(leaf);
            self.append(events).await;
        }
        self.flush().await;
        self.set_status("idle", None);
        if let Settled::Error(e) = &settled {
            tracing::debug!(session = %self.id, "pi turn failed: {e}");
        }
        if st.stopping {
            return;
        }
        let due = match st.compaction {
            TurnEndCompaction::Off => false,
            TurnEndCompaction::EveryTurn => true,
            TurnEndCompaction::Threshold(t) => st.context_tokens >= t,
        };
        if due && settled != Settled::Aborted {
            st.compacting = true;
            st.mapper.quiet_next_compaction();
            drop(st);
            let me = self.clone();
            tokio::spawn(async move {
                let _ = process
                    .rpc
                    .request(json!({"type": "compact"}), COMPACT_TIMEOUT)
                    .await;
                me.state.lock().await.compacting = false;
                me.drive().await;
            });
            return;
        }
        drop(st);
        tokio::spawn(self.drive());
    }

    async fn on_exit(&self, generation: u64, code: Option<i32>, stderr: String) {
        let mut st = self.state.lock().await;
        if st.process.as_ref().map(|r| r.generation) != Some(generation) {
            return;
        }
        if let Some(r) = st.process.take() {
            r.bridge.stop();
        }
        st.applied = None;
        self.env.pool.release(&self.id);
        if st.mapper.running().is_none() {
            return;
        }
        if st.stopping {
            let events = st.mapper.close_turn(TurnEnd::Aborted);
            self.append(events).await;
            self.flush().await;
            self.set_status("idle", None);
            return;
        }
        let message = if stderr.trim().is_empty() {
            format!(
                "the Pi engine exited{}",
                code.map(|c| format!(" with code {c}")).unwrap_or_default()
            )
        } else {
            format!(
                "the Pi engine exited{}: {}",
                code.map(|c| format!(" with code {c}")).unwrap_or_default(),
                stderr.trim()
            )
        };
        let events = st.mapper.close_turn(TurnEnd::Error {
            code: "ENGINE_EXITED".into(),
            message: message.clone(),
        });
        self.append(events).await;
        self.flush().await;
        self.set_status("error", Some(message));
    }

    async fn cancel(self: Arc<Self>) -> anyhow::Result<()> {
        let process = {
            let mut st = self.state.lock().await;
            st.stopping = true;
            if st.mapper.running().is_none() {
                return Ok(());
            }
            match st.process.as_ref() {
                Some(r) => r.process.clone(),
                None => {
                    let events = st.mapper.close_turn(TurnEnd::Aborted);
                    self.append(events).await;
                    self.flush().await;
                    self.set_status("idle", None);
                    return Ok(());
                }
            }
        };
        // 1. Pi's queued steers come back to the front of Aether's queue.
        if let Ok(data) = process
            .rpc
            .request(json!({"type": "clear_queue"}), Duration::from_secs(5))
            .await
        {
            let texts: Vec<String> =
                serde_json::from_value(data["steering"].clone()).unwrap_or_default();
            let mut st = self.state.lock().await;
            let events = st.mapper.restore_steers(&texts);
            self.append(events).await;
            self.flush().await;
        }
        // 2. Abort and wait until Pi is idle; a hung Pi is killed.
        if process
            .rpc
            .request(json!({"type": "abort"}), ABORT_TIMEOUT)
            .await
            .is_err()
        {
            process.kill(Duration::from_secs(2)).await;
        }
        Ok(())
    }

    async fn compact(self: Arc<Self>, model: ModelChoice) -> anyhow::Result<Value> {
        let need_slot = {
            let st = self.state.lock().await;
            anyhow::ensure!(
                st.mapper.running().is_none() && !st.compacting,
                "a turn is running; compact after it ends"
            );
            st.process.is_none()
        };
        if need_slot {
            self.env.pool.acquire(&self.id, self.stopper()).await;
        }
        let process = {
            let mut st = self.state.lock().await;
            st.want.get_or_insert(model);
            let ready = match self.ensure_process(&mut st).await {
                Ok(()) => self.ensure_model(&mut st).await,
                Err(e) => Err(e),
            };
            if let Err(e) = ready {
                if st.process.is_none() {
                    self.env.pool.release(&self.id);
                }
                return Err(e);
            }
            st.compacting = true;
            st.process.as_ref().map(|r| r.process.clone())
        };
        let Some(process) = process else {
            anyhow::bail!("the Pi engine is not running")
        };
        self.env.pool.set_busy(&self.id, true);
        let result = process
            .rpc
            .request(json!({"type": "compact"}), COMPACT_TIMEOUT)
            .await;
        self.env.pool.set_busy(&self.id, false);
        self.state.lock().await.compacting = false;
        self.flush().await;
        tokio::spawn(self.clone().drive());
        match result {
            Ok(data) => Ok(json!({
                "compacted": true,
                "tokens": data["tokensBefore"],
            })),
            Err(e) if format!("{e}").contains("Nothing to compact") => {
                Ok(json!({ "compacted": false }))
            }
            Err(e) => Err(e),
        }
    }

    async fn change_queued(self: Arc<Self>, item: &str, change: QueueChange) -> anyhow::Result<()> {
        let mut st = self.state.lock().await;
        let (events, steer) = st.mapper.change_queued(item, change)?;
        self.append(events).await;
        self.flush().await;
        if let (Some(message), Some(r)) = (steer, st.process.as_ref()) {
            let mut cmd = json!({"type": "prompt", "message": prompt_text(&message), "streamingBehavior": "steer"});
            let images = self.images(&message).await;
            if !images.is_empty() {
                cmd["images"] = images.into();
            }
            if let Err(e) = r.process.rpc.request(cmd, COMMAND_TIMEOUT).await {
                tracing::warn!(session = %self.id, "pi steer: {e:#}");
            }
        }
        Ok(())
    }

    async fn close(self: Arc<Self>) {
        let mut st = self.state.lock().await;
        st.stopping = true;
        if st.mapper.running().is_some() {
            let events = st.mapper.close_turn(TurnEnd::Aborted);
            self.append(events).await;
        }
        if let Some(r) = st.process.take() {
            r.bridge.stop();
            r.process.shutdown().await;
        }
        self.env.pool.release(&self.id);
        self.flush().await;
    }

    async fn stop_if_idle(self: Arc<Self>) -> bool {
        let Ok(mut st) = self.state.try_lock() else {
            return false;
        };
        if st.mapper.running().is_some() || st.compacting {
            return false;
        }
        let running = st.process.take();
        st.applied = None;
        drop(st);
        self.env.pool.release(&self.id);
        if let Some(r) = running {
            r.bridge.stop();
            r.process.shutdown().await;
        }
        true
    }
}

/// The bridge's view of its chat.
struct ChatBridge {
    inner: Weak<Inner>,
}

/// `path` with its existing ancestor resolved (symlinks) and the rest kept.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = sandbox::canonical_path(&existing);
    for part in rest.into_iter().rev() {
        out.push(part);
    }
    out
}

/// Aether's write rule for Pi's `write`/`edit`: `read-only` refuses,
/// `workspace-write` needs the target below a writable root.
pub fn gate_write(mode: SandboxMode, cwd: &Path, path: &str) -> Result<(), String> {
    match mode {
        SandboxMode::DangerFullAccess => Ok(()),
        SandboxMode::ReadOnly => Err(sandbox::denial_marker(mode)),
        SandboxMode::WorkspaceWrite => {
            let target = resolved(&crate::meta::absolute(path, cwd));
            let roots = sandbox::writable_roots(mode, &sandbox::canonical_path(cwd));
            if roots.iter().any(|r| target.starts_with(r)) {
                Ok(())
            } else {
                Err(sandbox::denial_marker(mode))
            }
        }
    }
}

#[async_trait]
impl BridgeHost for ChatBridge {
    fn config(&self) -> Value {
        let Some(inner) = self.inner.upgrade() else {
            return json!({});
        };
        let tools: Vec<Value> = inner
            .host
            .tools()
            .iter()
            .map(|t| {
                let s = t.schema();
                json!({
                    "name": s.name,
                    "description": s.description,
                    "parameters": s.parameters,
                })
            })
            .collect();
        json!({
            "session": inner.id,
            "cwd": inner.cwd,
            "sandbox": sandbox::mode_name(*inner.sandbox.lock().unwrap()),
            "tools": tools,
            "providers": inner.host.providers(),
        })
    }

    fn gate(&self, tool: &str, input: &Value) -> Result<(), String> {
        let Some(inner) = self.inner.upgrade() else {
            return Err("the chat is closed".into());
        };
        if tool != "write" && tool != "edit" {
            return Ok(());
        }
        let mode = *inner.sandbox.lock().unwrap();
        let path = input["path"]
            .as_str()
            .or_else(|| input["file_path"].as_str())
            .unwrap_or_default();
        gate_write(mode, &inner.cwd, path)
    }

    async fn call(
        &self,
        call_id: &str,
        tool: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Value {
        let Some(inner) = self.inner.upgrade() else {
            return json!({"content": [{"type": "text", "text": "the chat is closed"}], "isError": true});
        };
        let Some(t) = inner
            .host
            .tools()
            .into_iter()
            .find(|t| t.schema().name == tool)
        else {
            return json!({"content": [{"type": "text", "text": format!("unknown Aether tool {tool}")}], "isError": true});
        };
        let ctx = ToolContext {
            session_id: inner.id.clone(),
            call_id: call_id.to_owned(),
            cwd: inner.cwd.clone(),
            sandbox: *inner.sandbox.lock().unwrap(),
            cancel,
            attachments: inner.host.attachments(),
            model_accepts_images: true,
            approver: inner.host.approver(),
            state: Arc::new(()),
            caller: None,
        };
        match t.execute(args, ctx).await {
            Ok(out) => {
                let store = inner.host.attachments();
                let mut content = Vec::new();
                for part in out.content {
                    match part {
                        ToolResultPart::Text { text } => {
                            content.push(json!({"type": "text", "text": text}))
                        }
                        ToolResultPart::Image { attachment } => {
                            if let Ok((bytes, media)) = store.load(&attachment.attachment_id).await
                            {
                                content.push(json!({
                                    "type": "image",
                                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                                    "mimeType": media,
                                }));
                            }
                        }
                    }
                }
                json!({
                    "content": content,
                    "isError": out.is_error,
                    "details": {"aether": {"value": out.value, "meta": out.meta}},
                })
            }
            Err(e) => {
                json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true, "details": {}})
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efforts_map_onto_available_levels() {
        let levels: Vec<String> = ["off", "low", "high", "max"].map(String::from).to_vec();
        assert_eq!(thinking_level("high", &levels), "high");
        assert_eq!(thinking_level("medium", &levels), "low");
        assert_eq!(thinking_level("xhigh", &levels), "high");
        assert_eq!(thinking_level("none", &levels), "off");
        assert_eq!(thinking_level("high", &["off".to_owned()]), "off");
    }

    #[test]
    fn the_write_gate_follows_the_access_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        assert!(gate_write(SandboxMode::WorkspaceWrite, cwd, "new/file.txt").is_ok());
        let denied = gate_write(SandboxMode::WorkspaceWrite, cwd, "/etc/hosts").unwrap_err();
        assert_eq!(
            denied,
            "[sandbox: file access denied under workspace-write mode]"
        );
        assert!(gate_write(SandboxMode::ReadOnly, cwd, "a.txt").is_err());
        assert!(gate_write(SandboxMode::DangerFullAccess, cwd, "/etc/hosts").is_ok());
    }
}
