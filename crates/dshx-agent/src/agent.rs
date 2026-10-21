//! The agent driver: inbox, turn/step machine, request admission, model
//! streaming, retries and cancellation (dsh `core/agent-loop/src/agent.ts`).
use std::any::Any;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use dshx_core::{
    Approver, AttachmentStore, ContentBlock, FinishReason, GenerateRequest, LlmClient, LlmFailure,
    Message, MessageSource, NewEvent, Role, SandboxMode, SessionEvent, SessionHeader, StreamChunk,
    ToolRegistry, ToolSchema, kinds, now_ms,
};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::{AgentConfig, ApprovalPolicy, sandbox_str};
use crate::events::{
    Aborted, CancelCause, LlmError, TurnEndReason, TurnOutcome, error_chain, event, message_json,
    surface_append, surface_replace,
};
use crate::journal::Journal;
use crate::observer::{AgentObserver, AgentStatus, DeltaKind, StreamDelta};
use crate::prompt;
use crate::recovery::ToolCallRecovery;
use crate::stream::{BlockAssembler, StreamAccumulator};

/// Collaborators of an [`Agent`].
pub struct AgentDeps {
    pub llm: Arc<dyn LlmClient>,
    pub tools: ToolRegistry,
    pub log: Arc<dyn dshx_core::SessionLog>,
    pub attachments: Arc<dyn AttachmentStore>,
    pub approver: Arc<dyn Approver>,
    pub observer: Option<Arc<dyn AgentObserver>>,
    /// Per-session state shared by tools (`ToolContext::state`); `None` = a
    /// fresh `dshx_tools::ToolState` (jobs, spill files, read-before-write).
    pub tool_state: Option<Arc<dyn Any + Send + Sync>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    NextTurn,
    NextStep,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Target::NextTurn => "next-turn",
            Target::NextStep => "next-step",
        }
    }
}

pub(crate) struct Running {
    pub cancel: CancellationToken,
    pub cause: Option<CancelCause>,
}

/// Mutable loop state; never held across an await.
pub(crate) struct LoopState {
    pub status: AgentStatus,
    pub next_turn: Vec<Message>,
    pub next_step: Vec<Message>,
    pub running: Option<Running>,
    pub last_turn: u64,
    /// Whether this instance already logged its first `request/header`.
    pub request_header_logged: bool,
    /// Surface content generation at attachment or the previous request.
    pub request_surface_generation: u64,
    /// Files touched by successful read/write/edit calls of the current step.
    pub touches: Vec<String>,
    /// Repeat-tool-reminder chain: canonical call key and run length.
    pub repeat_chain: Option<(String, u32)>,
    pub overflow_retries: u32,
    pub pending_approvals: u32,
    /// Workspace-instruction baseline identity and the scopes its budget omitted.
    pub instructions_prepared: Option<(String, HashSet<String>)>,
    /// The queued prompt the running driver owns (claimed instead of the head).
    pub own_prompt: Option<String>,
    /// The running turn and the `seq` of its `turn/start`.
    pub open_turn: Option<(u64, u64)>,
    /// Whether this session already has a title (skips the per-request scan).
    pub titled: bool,
}

impl LoopState {
    fn list(&mut self, target: Target) -> &mut Vec<Message> {
        match target {
            Target::NextTurn => &mut self.next_turn,
            Target::NextStep => &mut self.next_step,
        }
    }
}

pub(crate) struct Inner {
    pub config: Mutex<AgentConfig>,
    pub llm: Arc<dyn LlmClient>,
    pub tools: Mutex<ToolRegistry>,
    pub journal: Journal,
    pub attachments: Arc<dyn AttachmentStore>,
    pub approver: Arc<dyn Approver>,
    pub observer: Observers,
    pub tool_state: Arc<dyn Any + Send + Sync>,
    pub state: Mutex<LoopState>,
    driver: tokio::sync::Mutex<()>,
    inbox_lock: tokio::sync::Mutex<()>,
    init: tokio::sync::Mutex<bool>,
    /// Cancelled when the agent is disposed (stops title work).
    pub lifetime: CancellationToken,
    /// `dshx-tools` session state when the tools use it (jobs, spill, model).
    pub tools_state: Option<Arc<dshx_tools::ToolState>>,
    pub jobs: crate::jobs::JobQueue,
    /// Product hooks over each step's entering batch, in registration order.
    pub pre_step_hooks: Mutex<Vec<Arc<dyn crate::hooks::PreStepHook>>>,
}

/// The agent's live observers: the one from [`AgentDeps`] plus every
/// [`Agent::add_observer`], notified in that order.
#[derive(Default)]
pub(crate) struct Observers(Mutex<Vec<Arc<dyn AgentObserver>>>);

impl Observers {
    fn all(&self) -> Vec<Arc<dyn AgentObserver>> {
        self.0.lock().unwrap().clone()
    }
}

impl AgentObserver for Observers {
    fn on_delta(&self, delta: &StreamDelta) {
        for o in self.all() {
            o.on_delta(delta);
        }
    }
    fn on_status(&self, status: &AgentStatus) {
        for o in self.all() {
            o.on_status(status);
        }
    }
    fn on_attempt(&self, turn: u64, step: u64, attempt: u32) {
        for o in self.all() {
            o.on_attempt(turn, step, attempt);
        }
    }
}

/// One coding agent over one session log. Cheap to clone; all clones drive the
/// same session. Turns are serialized: concurrent [`Agent::run_turn`] calls
/// queue in the inbox and run in order.
#[derive(Clone)]
pub struct Agent {
    pub(crate) inner: Arc<Inner>,
}

/// What one step decided to enter (dsh `PreparedStep`).
pub(crate) struct Entering {
    pub messages: Vec<Message>,
    /// The step's first request starts a request series (dsh `startsRequestSeries`).
    pub starts_series: bool,
    /// A hook rejected the step: the turn ends `blocked`, these messages
    /// return to the front of the next-step inbox.
    pub rejected: Option<Vec<Message>>,
}

type StepEnd = Option<TurnEndReason>;

fn is_aborted(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Aborted>().is_some()
}

pub(crate) fn check(cancel: &CancellationToken) -> anyhow::Result<()> {
    if cancel.is_cancelled() {
        Err(Aborted.into())
    } else {
        Ok(())
    }
}

fn user_message(content: Vec<ContentBlock>) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content,
        source: MessageSource::User,
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

fn preset_for(sandbox: SandboxMode, approval: ApprovalPolicy) -> Option<&'static str> {
    match (sandbox, approval) {
        (SandboxMode::WorkspaceWrite, ApprovalPolicy::Ask) => Some("workspace-write"),
        (SandboxMode::DangerFullAccess, ApprovalPolicy::Never) => Some("danger-full-access"),
        _ => None,
    }
}

impl Agent {
    pub fn new(config: AgentConfig, deps: AgentDeps) -> Agent {
        let journal = Journal::new(deps.log);
        let generation = journal.surface().content_generation;
        let tool_state: Arc<dyn Any + Send + Sync> = deps
            .tool_state
            .unwrap_or_else(|| Arc::new(dshx_tools::ToolState::new()));
        let tools_state = tool_state.clone().downcast::<dshx_tools::ToolState>().ok();
        if let Some(s) = &tools_state {
            s.set_model(config.model.clone());
        }
        let inner = Inner {
            config: Mutex::new(config),
            llm: deps.llm,
            tools: Mutex::new(deps.tools),
            journal,
            attachments: deps.attachments,
            approver: deps.approver,
            observer: Observers(Mutex::new(deps.observer.into_iter().collect())),
            tool_state,
            state: Mutex::new(LoopState {
                status: AgentStatus::Idle,
                next_turn: Vec::new(),
                next_step: Vec::new(),
                running: None,
                last_turn: 0,
                request_header_logged: false,
                request_surface_generation: generation,
                touches: Vec::new(),
                repeat_chain: None,
                overflow_retries: 0,
                pending_approvals: 0,
                instructions_prepared: None,
                own_prompt: None,
                open_turn: None,
                titled: false,
            }),
            driver: tokio::sync::Mutex::new(()),
            inbox_lock: tokio::sync::Mutex::new(()),
            init: tokio::sync::Mutex::new(false),
            lifetime: CancellationToken::new(),
            tools_state,
            jobs: Default::default(),
            pre_step_hooks: Mutex::new(Vec::new()),
        };
        let inner = Arc::new(inner);
        if let Some(state) = &inner.tools_state {
            crate::jobs::start(&inner, state);
        }
        Agent { inner }
    }

    pub fn session_id(&self) -> String {
        self.inner.journal.id()
    }

    pub fn status(&self) -> AgentStatus {
        self.inner.state.lock().unwrap().status.clone()
    }

    pub fn config(&self) -> AgentConfig {
        self.inner.config.lock().unwrap().clone()
    }

    /// Changes model/effort/limits for later requests (a changed header is logged then).
    pub fn update_config(&self, f: impl FnOnce(&mut AgentConfig)) {
        f(&mut self.inner.config.lock().unwrap());
    }

    /// Replaces the tool set; additions/removals reach the model at the next request.
    pub fn set_tools(&self, tools: ToolRegistry) {
        *self.inner.tools.lock().unwrap() = tools;
    }

    /// Adds a live observer after the existing ones (status, deltas, attempts).
    pub fn add_observer(&self, observer: Arc<dyn AgentObserver>) {
        self.inner.observer.0.lock().unwrap().push(observer);
    }

    /// Adds a hook that runs at every step after the runtime-context
    /// snapshot (see [`crate::PreStepHook`]).
    pub fn add_pre_step_hook(&self, hook: Arc<dyn crate::hooks::PreStepHook>) {
        self.inner.pre_step_hooks.lock().unwrap().push(hook);
    }

    /// Switches sandbox mode and approval policy, logging dsh's permission events.
    pub async fn set_permissions(
        &self,
        sandbox: SandboxMode,
        approval: ApprovalPolicy,
    ) -> anyhow::Result<()> {
        {
            let mut c = self.inner.config.lock().unwrap();
            c.sandbox = sandbox;
            c.approval = approval;
        }
        if *self.inner.init.lock().await {
            self.inner.apply_permissions().await?;
        }
        Ok(())
    }

    /// Sets a user-chosen session title (`session/title` with source `user`).
    pub async fn rename(&self, title: &str) -> anyhow::Result<()> {
        crate::title::rename(&self.inner, title).await
    }

    /// Runs one user prompt to the end of its turn. Queues behind a running turn.
    pub async fn run_turn(&self, input: Vec<ContentBlock>) -> anyhow::Result<TurnOutcome> {
        self.run_message(user_message(input)).await
    }

    /// Runs any user-role message (e.g. a job notice or a subagent report) as
    /// the prompt of its own turn, like dsh `followup`.
    pub async fn run_message(&self, message: Message) -> anyhow::Result<TurnOutcome> {
        let id = self.accept(Target::NextTurn, message).await?;
        self.drive_queued(&id).await
    }

    /// Queues a prebuilt message as the prompt of its own turn and returns
    /// once the inbox accepted it (dsh `followup`); the turn runs in the
    /// background behind earlier input.
    pub async fn followup(&self, message: Message) -> anyhow::Result<()> {
        let id = self.accept(Target::NextTurn, message).await?;
        let agent = self.clone();
        tokio::spawn(async move {
            if let Err(e) = agent.drive_queued(&id).await {
                tracing::warn!("follow-up turn failed: {e:#}");
            }
        });
        Ok(())
    }

    /// Delivers a prebuilt message at the nearest step and returns once the
    /// inbox accepted it (dsh `steer`); an idle agent opens a turn for it in
    /// the background.
    pub async fn steer_detached(&self, message: Message) -> anyhow::Result<()> {
        let id = self.accept(Target::NextStep, message).await?;
        let agent = self.clone();
        tokio::spawn(async move {
            if let Err(e) = agent.drive_steered(&id).await {
                tracing::warn!("steered turn failed: {e:#}");
            }
        });
        Ok(())
    }

    /// Appends `message` to an inbox list (one durable splice); returns its id.
    async fn accept(&self, target: Target, message: Message) -> anyhow::Result<String> {
        let inner = &self.inner;
        inner.ensure_initialized().await?;
        let id = message.id.clone();
        let len = inner.state.lock().unwrap().list(target).len();
        inner.splice(target, len, 0, vec![message], false).await?;
        Ok(id)
    }

    /// Drives turns while the queued prompt `id` is pending.
    async fn drive_queued(&self, id: &str) -> anyhow::Result<TurnOutcome> {
        let inner = &self.inner;
        let _driver = inner.driver.lock().await;
        let pending = {
            let mut st = inner.state.lock().unwrap();
            let pending = st.next_turn.iter().any(|m| m.id == id);
            st.own_prompt = pending.then(|| id.to_owned());
            pending
        };
        if !pending {
            // Cancelled while queued: no turn opens for it.
            return Ok(TurnOutcome {
                turn: None,
                reason: TurnEndReason::Aborted {
                    reason: CancelCause::User,
                },
            });
        }
        inner.drive(Target::NextTurn).await
    }

    /// Delivers a prebuilt message at the nearest step (dsh `steer`): a
    /// running turn claims it at its next step; an idle agent opens a turn
    /// that claims it from the next-step inbox. Resolves when the turn that
    /// took it ends; `None` when an already running turn took it.
    pub async fn steer_message(&self, message: Message) -> anyhow::Result<Option<TurnOutcome>> {
        let id = self.accept(Target::NextStep, message).await?;
        self.drive_steered(&id).await
    }

    /// Drives a turn for the steered message `id` unless a running turn took it.
    async fn drive_steered(&self, id: &str) -> anyhow::Result<Option<TurnOutcome>> {
        let inner = &self.inner;
        let _driver = inner.driver.lock().await;
        let pending = inner
            .state
            .lock()
            .unwrap()
            .next_step
            .iter()
            .any(|m| m.id == id);
        if !pending {
            return Ok(None);
        }
        inner.drive(Target::NextStep).await.map(Some)
    }

    /// No turn runs, no input is queued and no caller is about to drive one
    /// (dsh `status === "idle"` with an empty inbox).
    pub fn is_idle(&self) -> bool {
        let st = self.inner.state.lock().unwrap();
        st.running.is_none()
            && st.next_turn.is_empty()
            && st.next_step.is_empty()
            && self.inner.driver.try_lock().is_ok()
    }

    /// The tools offered to the model (a subagent starts from its parent's set).
    pub fn tools(&self) -> ToolRegistry {
        self.inner.tools.lock().unwrap().clone()
    }

    /// The session header.
    pub fn header(&self) -> SessionHeader {
        self.inner.journal.header()
    }

    /// Snapshot of the session's events (without the header).
    pub fn events(&self) -> Vec<SessionEvent> {
        self.inner.journal.read(|events, _| events.to_vec())
    }

    /// Number of events in the session log (the next `seq`).
    pub fn log_len(&self) -> u64 {
        self.inner.journal.len()
    }

    /// Events with `seq >= from` (cheaper than [`Agent::events`] for a tail).
    pub fn events_from(&self, from: u64) -> Vec<SessionEvent> {
        dshx_core::CallerSession::events_from(&self.inner.journal, from)
    }

    /// Live feed of the events appended to this agent's session log.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.inner.journal.subscribe()
    }

    /// Whether neither inbox list holds input.
    pub fn inbox_is_empty(&self) -> bool {
        let st = self.inner.state.lock().unwrap();
        st.next_turn.is_empty() && st.next_step.is_empty()
    }

    /// Whether the inbox still holds the message `id`.
    pub fn inbox_contains(&self, id: &str) -> bool {
        let st = self.inner.state.lock().unwrap();
        st.next_turn.iter().chain(&st.next_step).any(|m| m.id == id)
    }

    /// Appends one log-only event (e.g. a parent's `subagent/catalog` fact).
    pub async fn append_event(&self, event: NewEvent) -> anyhow::Result<SessionEvent> {
        self.inner.append(event).await
    }

    /// Durability checkpoint of the session log.
    pub async fn flush(&self) -> anyhow::Result<()> {
        self.inner.journal.flush().await
    }

    /// Adds input for the next step of the running turn (dsh `steer`); when idle
    /// it starts a turn like [`Agent::run_turn`].
    pub async fn steer(&self, input: Vec<ContentBlock>) -> anyhow::Result<Option<TurnOutcome>> {
        let running = self.inner.state.lock().unwrap().running.is_some();
        if running {
            self.inner.ensure_initialized().await?;
            let len = self.inner.state.lock().unwrap().next_step.len();
            self.inner
                .splice(Target::NextStep, len, 0, vec![user_message(input)], false)
                .await?;
            return Ok(None);
        }
        self.run_turn(input).await.map(Some)
    }

    /// Cancels the running turn (cause `user`) and drops queued input.
    pub async fn cancel(&self) {
        self.cancel_with(CancelCause::User, false).await;
    }

    /// Cancels with an explicit cause; `keep_inbox` keeps queued prompts.
    pub async fn cancel_with(&self, cause: CancelCause, keep_inbox: bool) {
        let inner = &self.inner;
        if !keep_inbox {
            let (steps, turns) = {
                let st = inner.state.lock().unwrap();
                (st.next_step.len(), st.next_turn.len())
            };
            if steps > 0 {
                let _ = inner
                    .splice(Target::NextStep, 0, steps, Vec::new(), true)
                    .await;
            }
            if turns > 0 {
                let _ = inner
                    .splice(Target::NextTurn, 0, turns, Vec::new(), true)
                    .await;
            }
        }
        let mut st = inner.state.lock().unwrap();
        if let Some(r) = st.running.as_mut() {
            if r.cause.is_none() {
                r.cause = Some(cause);
            }
            r.cancel.cancel();
        }
    }

    /// Stops background work (title generation, the session's jobs) and
    /// cancels a running turn.
    pub async fn dispose(&self) {
        self.cancel_with(CancelCause::Disposed, false).await;
        self.inner.lifetime.cancel();
        if let Some(state) = &self.inner.tools_state {
            state.jobs().shutdown(Some(&self.inner.journal.id()));
        }
    }
}

impl Inner {
    pub(crate) fn set_status(&self, status: AgentStatus) {
        let changed = {
            let mut st = self.state.lock().unwrap();
            if st.status == status {
                false
            } else {
                st.status = status.clone();
                true
            }
        };
        if changed {
            self.observer.on_status(&status);
        }
    }

    pub(crate) async fn append(&self, e: NewEvent) -> anyhow::Result<dshx_core::SessionEvent> {
        self.journal.append(e).await
    }

    /// One normalized durable inbox mutation (`agent/inbox/spliced`).
    pub(crate) async fn splice(
        &self,
        target: Target,
        start: usize,
        delete: usize,
        inserted: Vec<Message>,
        discard: bool,
    ) -> anyhow::Result<Vec<Message>> {
        let _guard = self.inbox_lock.lock().await;
        let (start, delete, removed) = {
            let mut st = self.state.lock().unwrap();
            let list = st.list(target);
            let start = start.min(list.len());
            let delete = delete.min(list.len() - start);
            (start, delete, list[start..start + delete].to_vec())
        };
        if delete == 0 && inserted.is_empty() {
            return Ok(Vec::new());
        }
        let mut data = Map::new();
        data.insert("target".into(), target.as_str().into());
        data.insert("start".into(), start.into());
        if delete > 0 {
            data.insert("removedCount".into(), delete.into());
        }
        data.insert(
            "inserted".into(),
            Value::Array(inserted.iter().map(message_json).collect()),
        );
        if discard && delete > 0 {
            data.insert("outcome".into(), "canceled".into());
        }
        self.append(event(kinds::INBOX_SPLICED, Value::Object(data)))
            .await?;
        self.state
            .lock()
            .unwrap()
            .list(target)
            .splice(start..start + delete, inserted);
        Ok(removed)
    }

    /// Removes and returns the batch one step consumes (dsh `inbox.claim`).
    async fn claim(&self, target: Target) -> anyhow::Result<Vec<Message>> {
        let steps = self.state.lock().unwrap().next_step.len();
        let mut claimed = self
            .splice(Target::NextStep, 0, steps, Vec::new(), false)
            .await?;
        if target == Target::NextTurn {
            claimed.extend(
                self.splice(Target::NextTurn, 0, 1, Vec::new(), false)
                    .await?,
            );
        }
        Ok(claimed)
    }

    /// Folds durable state of an existing log, repairs a crashed turn and pins permissions.
    pub(crate) async fn ensure_initialized(&self) -> anyhow::Result<()> {
        let mut done = self.init.lock().await;
        if *done {
            return Ok(());
        }
        let (next_turn, next_step, last_turn, open, recovery) = self.journal.read(|events, _| {
            let mut next_turn: Vec<Message> = Vec::new();
            let mut next_step: Vec<Message> = Vec::new();
            let mut last_turn = 0;
            let mut open: Option<(u64, Option<u64>)> = None;
            let mut recovery = ToolCallRecovery::default();
            for e in events {
                recovery.observe(e);
                match e.kind.as_str() {
                    kinds::INBOX_SPLICED => {
                        let list = if e.data["target"] == "next-step" {
                            &mut next_step
                        } else {
                            &mut next_turn
                        };
                        let start =
                            (e.data["start"].as_u64().unwrap_or(0) as usize).min(list.len());
                        let removed = (e.data["removedCount"].as_u64().unwrap_or(0) as usize)
                            .min(list.len() - start);
                        let inserted: Vec<Message> = e.data["inserted"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|m| serde_json::from_value(m.clone()).ok())
                                    .collect()
                            })
                            .unwrap_or_default();
                        list.splice(start..start + removed, inserted);
                    }
                    kinds::TURN_START => {
                        let t = e.data["turn"].as_u64().unwrap_or(0);
                        last_turn = last_turn.max(t);
                        open = Some((t, None));
                    }
                    kinds::TURN_END => open = None,
                    kinds::STEP_START => {
                        if let Some(o) = open.as_mut() {
                            o.1 = e.data["step"].as_u64();
                        }
                    }
                    kinds::STEP_END => {
                        if let Some(o) = open.as_mut() {
                            o.1 = None;
                        }
                    }
                    _ => {}
                }
            }
            (next_turn, next_step, last_turn, open, recovery)
        });
        {
            let mut st = self.state.lock().unwrap();
            st.next_turn = next_turn;
            st.next_step = next_step;
            st.last_turn = last_turn;
        }
        if let Some((turn, step)) = open {
            // A crashed process left this turn open: close it like dsh resume.
            self.append_recovery(&recovery).await?;
            if let Some(step) = step {
                self.append(event(kinds::STEP_END, json!({"turn": turn, "step": step})))
                    .await?;
            }
            self.append(event(
                kinds::TURN_END,
                json!({"turn": turn, "reason": {"kind": "interrupted"}}),
            ))
            .await?;
        }
        self.apply_permissions().await?;
        let stale = self.state.lock().unwrap().next_turn.len();
        if stale > 0 {
            // Prompts queued by an earlier process have no waiting caller.
            self.splice(Target::NextTurn, 0, stale, Vec::new(), true)
                .await?;
        }
        *done = true;
        Ok(())
    }

    async fn append_recovery(&self, recovery: &ToolCallRecovery) -> anyhow::Result<()> {
        if recovery.is_empty() {
            return Ok(());
        }
        let base = self.journal.len();
        for (data, sources) in recovery.results(|i| base + i as u64) {
            self.append(surface_append(kinds::TOOL_RESULT, data, sources))
                .await?;
        }
        Ok(())
    }

    /// Pins initial permission facts or records a changed preset/sandbox/approval.
    pub(crate) async fn apply_permissions(&self) -> anyhow::Result<()> {
        let (sandbox, approval) = {
            let c = self.config.lock().unwrap();
            (c.sandbox, c.approval)
        };
        let (preset, cur_sandbox, cur_approval) = self.journal.read(|events, _| {
            let last = |k: &str, f: &str| {
                events
                    .iter()
                    .rev()
                    .find(|e| e.kind == k)
                    .and_then(|e| e.data.get(f).and_then(Value::as_str).map(str::to_owned))
            };
            (
                last(kinds::PERMISSION_PRESET, "preset"),
                last(kinds::SANDBOX_MODE, "mode"),
                last(kinds::APPROVAL_POLICY, "policy"),
            )
        });
        let mut out = Vec::new();
        let name = preset_for(sandbox, approval);
        if let Some(n) = name
            && preset.as_deref() != Some(n)
        {
            out.push(event(kinds::PERMISSION_PRESET, json!({"preset": n})));
        }
        if cur_sandbox.as_deref() != Some(sandbox_str(sandbox)) {
            out.push(event(
                kinds::SANDBOX_MODE,
                json!({"mode": sandbox_str(sandbox)}),
            ));
        }
        if cur_approval.as_deref() != Some(approval.as_str()) {
            out.push(event(
                kinds::APPROVAL_POLICY,
                json!({"policy": approval.as_str()}),
            ));
        }
        for e in out {
            self.append(e).await?;
        }
        Ok(())
    }

    /// Runs turns while this caller's prompt (and late next-step input) is pending.
    async fn drive(self: &Arc<Self>, first: Target) -> anyhow::Result<TurnOutcome> {
        self.set_status(AgentStatus::Running);
        let mut outcome;
        let mut target = first;
        loop {
            outcome = self.turn(target).await;
            target = Target::NextStep;
            let more = outcome.is_ok() && !self.state.lock().unwrap().next_step.is_empty();
            if !more {
                break;
            }
        }
        {
            let mut st = self.state.lock().unwrap();
            st.overflow_retries = 0;
        }
        match &outcome {
            Ok(TurnOutcome {
                reason: TurnEndReason::Error { error },
                ..
            }) => self.set_status(AgentStatus::Error {
                error: error.message.clone(),
            }),
            Err(e) => self.set_status(AgentStatus::Error {
                error: e.to_string(),
            }),
            _ => self.set_status(AgentStatus::Idle),
        }
        outcome
    }

    async fn turn(self: &Arc<Self>, target: Target) -> anyhow::Result<TurnOutcome> {
        let cancel = CancellationToken::new();
        let turn = {
            let mut st = self.state.lock().unwrap();
            st.running = Some(Running {
                cancel: cancel.clone(),
                cause: None,
            });
            st.last_turn + 1
        };
        let start = match self
            .append(event(kinds::TURN_START, json!({"turn": turn})))
            .await
        {
            Ok(start) => start,
            Err(e) => {
                self.state.lock().unwrap().running = None;
                return Err(e);
            }
        };
        {
            let mut st = self.state.lock().unwrap();
            st.last_turn = turn;
            st.open_turn = Some((turn, start.seq));
        }
        let result = self.turn_body(turn, target, &cancel).await;
        let cause = self
            .state
            .lock()
            .unwrap()
            .running
            .as_ref()
            .and_then(|r| r.cause.clone());
        let reason = match result {
            Ok(reason) => reason,
            Err(_) if cancel.is_cancelled() => TurnEndReason::Aborted {
                reason: cause.unwrap_or(CancelCause::User),
            },
            Err(e) => {
                let failure = match e.downcast_ref::<LlmError>() {
                    Some(LlmError(f)) => f.clone(),
                    None => LlmFailure::new("UNKNOWN", error_chain(&e)),
                };
                tracing::warn!(turn, "turn failed: {}", failure.message);
                TurnEndReason::Error { error: failure }
            }
        };
        let end = self
            .append(event(
                kinds::TURN_END,
                json!({"turn": turn, "reason": reason}),
            ))
            .await;
        {
            let mut st = self.state.lock().unwrap();
            st.running = None;
            st.open_turn = None;
        }
        end?;
        // Completions that arrived after the last step wake the agent.
        let wake = crate::jobs::drain(self).await;
        crate::jobs::wake(self, wake);
        Ok(TurnOutcome {
            turn: Some(turn),
            reason,
        })
    }

    async fn turn_body(
        self: &Arc<Self>,
        turn: u64,
        first_target: Target,
        cancel: &CancellationToken,
    ) -> anyhow::Result<TurnEndReason> {
        let mut turn_ends: Option<TurnEndReason> = None;
        let mut target = first_target;
        let mut current_step = 0u64;
        loop {
            check(cancel)?;
            let step = current_step + 1;
            let mut entering = self.pre_step(target, turn, step, cancel).await?;
            if let Some(restore) = entering.rejected.take() {
                if !restore.is_empty() {
                    self.splice(Target::NextStep, 0, 0, restore, false).await?;
                }
                return Ok(TurnEndReason::Blocked);
            }
            if turn_ends.is_some() && entering.messages.is_empty() {
                break;
            }
            if current_step == 0 && entering.messages.is_empty() {
                return Ok(TurnEndReason::Completed);
            }
            check(cancel)?;
            let step_start = self
                .append(event(
                    kinds::STEP_START,
                    json!({"turn": turn, "step": step}),
                ))
                .await?;
            current_step = step;
            let result = self.step(turn, step, entering, cancel).await;
            match result {
                Ok(end) => {
                    if !matches!(turn_ends, Some(TurnEndReason::MaxTokens)) {
                        turn_ends = end;
                    }
                }
                Err(e) => {
                    let recovery = self.journal.read(|events, _| {
                        let mut r = ToolCallRecovery::default();
                        for ev in &events[step_start.seq as usize..] {
                            r.observe(ev);
                        }
                        r
                    });
                    if let Err(re) = self.append_recovery(&recovery).await {
                        tracing::warn!("could not record pending tool results: {re:#}");
                    }
                    self.append(event(kinds::STEP_END, json!({"turn": turn, "step": step})))
                        .await?;
                    return Err(e);
                }
            }
            self.append(event(kinds::STEP_END, json!({"turn": turn, "step": step})))
                .await?;
            check(cancel)?;
            if turn_ends.is_some() && self.state.lock().unwrap().next_step.is_empty() {
                break;
            }
            target = Target::NextStep;
        }
        Ok(turn_ends.unwrap_or(TurnEndReason::Completed))
    }

    /// Claims input, runs pressure compaction and folds workspace instructions
    /// and the runtime-context snapshot into the entering batch.
    async fn pre_step(
        self: &Arc<Self>,
        target: Target,
        turn: u64,
        step: u64,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Entering> {
        // Job events logged so far; notices land in this step's batch.
        crate::jobs::drain(self).await;
        let claimed = self.claim(target).await?;
        // dsh 0.2.0 projects the runtime context before the pre-step listeners run.
        let context = self.runtime_context();
        let (compaction_auto, reminder) = {
            let c = self.config.lock().unwrap();
            (c.compaction.auto, c.repeat_reminder.enabled)
        };
        if compaction_auto && !cancel.is_cancelled() {
            match crate::compaction::compact_if_needed(
                self,
                crate::compaction::Trigger::Pressure,
                turn,
                cancel,
            )
            .await
            {
                Ok(_) => {}
                Err(e) if is_aborted(&e) || cancel.is_cancelled() => return Err(Aborted.into()),
                Err(e) => tracing::warn!("step compaction failed: {e:#}; continuing the turn"),
            }
        }
        check(cancel)?;
        if reminder && claimed.iter().any(|m| m.source.is_user()) {
            self.state.lock().unwrap().repeat_chain = None;
        }
        let mut messages = claimed.clone();
        let instructions = crate::instructions::pre_step(self, &claimed, cancel).await;
        check(cancel)?;
        match instructions {
            Ok(Some(desired)) if !(step == 1 && messages.is_empty() && context.is_none()) => {
                messages.push(desired)
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("workspace instructions failed: {e:#}"),
        }
        if let Some(ctx) = context {
            messages.push(ctx);
        }
        // dsh skips the step's listeners when nothing at all enters step 1.
        let mut decision = crate::hooks::Decision::default();
        if !(step == 1 && messages.is_empty()) {
            decision = crate::hooks::run(self, &claimed, &mut messages, turn, step, cancel).await;
            check(cancel)?;
        }
        Ok(Entering {
            messages,
            starts_series: decision.starts_series,
            rejected: decision.reject,
        })
    }

    /// The runtime-context snapshot when it differs from the retained one.
    fn runtime_context(&self) -> Option<Message> {
        let (sandbox, approval, cwd, include, extra) = {
            let c = self.config.lock().unwrap();
            (
                c.sandbox,
                c.approval,
                c.cwd.to_string_lossy().into_owned(),
                c.prompt.include_runtime_context,
                c.prompt.extra_context.clone(),
            )
        };
        let sections = if include {
            prompt::context_sections(sandbox, approval, &cwd, &extra)
        } else {
            Vec::new()
        };
        let current = prompt::join_context(&sections);
        // `None` = never existed, `Some(None)` = shadowed, `Some(Some(t))` = retained text.
        let retained: Option<Option<String>> = self.journal.read(|events, surface| {
            let latest = events.iter().rev().find(|e| {
                e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "runtime-context"
            })?;
            if surface.nodes.contains(&latest.seq) {
                Some(Some(
                    prompt::text_of(&latest.data["content"]).unwrap_or_default(),
                ))
            } else {
                Some(None)
            }
        });
        if retained.is_none() && current.is_empty() {
            return None;
        }
        let snapshot = if current.is_empty() {
            prompt::CONTEXT_CLEARED.to_owned()
        } else {
            current
        };
        if let Some(Some(text)) = &retained
            && *text == snapshot
        {
            return None;
        }
        Some(prompt::context_message(
            &snapshot,
            if snapshot == prompt::CONTEXT_CLEARED {
                &[]
            } else {
                &sections
            },
        ))
    }

    pub(crate) fn tool_schemas(&self) -> Vec<ToolSchema> {
        prompt::ordered_tools(self.tools.lock().unwrap().schemas())
    }

    fn rendered_prompt(&self, tools: &[ToolSchema]) -> anyhow::Result<String> {
        let c = self.config.lock().unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let cwd = c.cwd.to_string_lossy().into_owned();
        prompt::render_system_prompt(&c.prompt, &names, self.llm.provider(), &c.model, &cwd)
    }

    /// dsh `SystemPromptProjection.project`: per-node system prompt commits.
    fn system_prompt_commits(
        &self,
        rendered: &str,
        in_history: bool,
        starts_series: bool,
        turn: u64,
        step: u64,
    ) -> Vec<NewEvent> {
        let nodes: Vec<(u64, Option<String>)> = self.journal.read(|events, surface| {
            surface
                .nodes
                .iter()
                .filter_map(|seq| {
                    let e = events.get(*seq as usize)?;
                    if e.kind != kinds::SYSTEM_MESSAGE {
                        return None;
                    }
                    let content = &e.data["message"]["content"];
                    let text = if content.as_array().is_some_and(|a| a.is_empty()) {
                        Some(String::new())
                    } else {
                        prompt::text_of(content)
                    };
                    Some((*seq, text))
                })
                .collect()
        });
        let data = |text: &str| json!({"turn": turn, "step": step, "message": message_json(&prompt::system_message(text))});
        let Some(head) = nodes.first() else {
            return vec![surface_append(kinds::SYSTEM_MESSAGE, data(rendered), None)];
        };
        let replace = |seq: u64, text: &str| {
            surface_replace(kinds::SYSTEM_MESSAGE, data(text), seq, seq, vec![seq])
        };
        if !in_history || starts_series || rendered.is_empty() {
            let mut out: Vec<NewEvent> = nodes[1..]
                .iter()
                .filter(|(_, t)| t.as_deref() != Some(""))
                .map(|(seq, _)| replace(*seq, ""))
                .collect();
            if head.1.as_deref() != Some(rendered) {
                out.push(replace(head.0, rendered));
            }
            return out;
        }
        let latest = nodes
            .iter()
            .rev()
            .find(|(_, t)| t.as_deref() != Some(""))
            .unwrap_or(head);
        if latest.1.as_deref() == Some(rendered) {
            return Vec::new();
        }
        vec![surface_append(kinds::SYSTEM_MESSAGE, data(rendered), None)]
    }

    /// The canonical request header and resolved effort/max tokens.
    fn header(&self, tools: &[ToolSchema]) -> (Value, String, u32) {
        let c = self.config.lock().unwrap();
        let route = c.route();
        let effort = c
            .effort
            .clone()
            .unwrap_or_else(|| route.default_effort.clone());
        let max_tokens = c.max_tokens.unwrap_or(route.default_max_tokens);
        // dsh key order: an explicit effort precedes `maxTokens`, an adapter
        // default one is appended after it.
        let mut config = Map::new();
        config.insert("provider".into(), self.llm.provider().into());
        config.insert("model".into(), c.model.clone().into());
        if c.effort.is_some() {
            config.insert("reasoningEffort".into(), effort.clone().into());
        }
        config.insert("maxTokens".into(), max_tokens.into());
        if c.effort.is_none() {
            config.insert("reasoningEffort".into(), effort.clone().into());
        }
        let mut header = Map::new();
        header.insert("config".into(), Value::Object(config));
        let mut defaults = Map::new();
        if c.effort.is_none() {
            defaults.insert("reasoningEffort".into(), true.into());
        }
        if c.max_tokens.is_none() {
            defaults.insert("maxTokens".into(), true.into());
        }
        if !defaults.is_empty() {
            header.insert("adapterDefaults".into(), Value::Object(defaults));
        }
        if !tools.is_empty() {
            header.insert(
                "tools".into(),
                serde_json::to_value(tools).unwrap_or_default(),
            );
        }
        (Value::Object(header), effort, max_tokens)
    }

    fn tools_changed(&self, header: &Value) -> bool {
        match self.journal.request_header() {
            None => false,
            Some(base) => base.get("tools") != header.get("tools"),
        }
    }

    pub(crate) fn context_window(&self) -> Option<u64> {
        let c = self.config.lock().unwrap();
        c.route().context_window.or_else(|| {
            self.llm
                .models()
                .into_iter()
                .find(|m| m.id == c.model)
                .map(|m| m.context_window)
        })
    }

    pub(crate) fn model_accepts_images(&self) -> bool {
        let model = self.config.lock().unwrap().model.clone();
        self.llm
            .models()
            .into_iter()
            .find(|m| m.id == model)
            .is_some_and(|m| m.accepts_images())
    }

    /// Logs the request envelope as needed and derives the request (dsh `buildRequest`).
    #[allow(clippy::too_many_arguments)]
    async fn build_request(
        self: &Arc<Self>,
        header: Value,
        effort: String,
        max_tokens: u32,
        tools: &[ToolSchema],
        turn: u64,
        step: u64,
        starts_request_series: bool,
    ) -> anyhow::Result<GenerateRequest> {
        let baseline = self.journal.request_header();
        let generation = self.journal.surface().content_generation;
        let (logged, prev_generation) = {
            let st = self.state.lock().unwrap();
            (st.request_header_logged, st.request_surface_generation)
        };
        let starts_series = starts_request_series || prev_generation != generation;
        let mut header_seq = None;
        let with_series = |mut d: Map<String, Value>| {
            if starts_series {
                d.insert("startsSeries".into(), true.into());
            }
            Value::Object(d)
        };
        if !logged {
            let mut d = Map::new();
            d.insert("header".into(), header.clone());
            d.insert(
                "reason".into(),
                if baseline.is_none() {
                    "initial"
                } else {
                    "resume"
                }
                .into(),
            );
            header_seq = Some(
                self.append(event(kinds::REQUEST_HEADER, with_series(d)))
                    .await?
                    .seq,
            );
            self.state.lock().unwrap().request_header_logged = true;
        } else if baseline.as_ref() != Some(&header) {
            let mut d = Map::new();
            d.insert("header".into(), header.clone());
            d.insert("reason".into(), "change".into());
            header_seq = Some(
                self.append(event(kinds::REQUEST_HEADER, with_series(d)))
                    .await?
                    .seq,
            );
        } else if starts_series {
            self.append(event(
                kinds::REQUEST_HEADER,
                json!({"header": header, "reason": "series"}),
            ))
            .await?;
        }
        if let (Some(base), Some(seq)) = (&baseline, header_seq) {
            let names = |h: &Value| -> Vec<String> {
                h.get("tools")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t["name"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let previous = names(base);
            let current: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            let additions: Vec<Value> = current
                .iter()
                .filter(|n| !previous.contains(n))
                .map(|n| json!({"type": "tool-addition", "toolName": n}))
                .collect();
            let removals: Vec<Value> = previous
                .iter()
                .filter(|n| !current.contains(n))
                .map(|n| json!({"type": "tool-removal", "toolName": n}))
                .collect();
            if !additions.is_empty() || !removals.is_empty() {
                let has_additions = !additions.is_empty();
                let content: Vec<Value> = additions.into_iter().chain(removals).collect();
                let message = json!({
                    "id": dshx_core::new_id(),
                    "role": "developer",
                    "content": content,
                    "source": {"kind": "tool-registry"},
                });
                let mut d = Map::new();
                d.insert("turn".into(), turn.into());
                d.insert("step".into(), step.into());
                d.insert("message".into(), message);
                if has_additions {
                    d.insert("headerSeq".into(), seq.into());
                }
                self.append(surface_append(
                    kinds::DEVELOPER_MESSAGE,
                    Value::Object(d),
                    None,
                ))
                .await?;
            }
        }
        self.state.lock().unwrap().request_surface_generation =
            self.journal.surface().content_generation;

        let (model, route) = {
            let c = self.config.lock().unwrap();
            (c.model.clone(), c.route())
        };
        let mut context = Map::new();
        context.insert("provider".into(), self.llm.provider().into());
        context.insert("model".into(), model.clone().into());
        if let Some(w) = self.context_window() {
            context.insert("contextWindow".into(), w.into());
        }
        if route.system_prompt_in_history {
            context.insert("systemPromptUpdate".into(), "in-history".into());
        }
        let context = Value::Object(context);
        if self.journal.request_context().as_ref() != Some(&context) {
            self.append(event(kinds::REQUEST_CONTEXT, context)).await?;
        }
        crate::title::after_request(self, &model).await;

        Ok(GenerateRequest {
            model,
            reasoning_effort: Some(effort),
            messages: self.journal.derive_messages(),
            tools: tools.to_vec(),
            max_tokens: Some(max_tokens),
            temperature: None,
            session_id: Some(self.journal.id()),
        })
    }

    /// One model call plus the tool calls it requested (dsh `step`).
    async fn step(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        entering: Entering,
        cancel: &CancellationToken,
    ) -> anyhow::Result<StepEnd> {
        check(cancel)?;
        if let Some(state) = &self.tools_state {
            state.set_model(self.config.lock().unwrap().model.clone());
        }
        let tools = self.tool_schemas();
        let rendered = self.rendered_prompt(&tools)?;
        let mut first_attempt = true;
        let mut attempt = 0u32;
        let mut retries = 0u32;
        let mut retry_id: Option<String> = None;
        loop {
            let (header, effort, max_tokens) = self.header(&tools);
            let route = self.config.lock().unwrap().route();
            check(cancel)?;
            let generation = self.journal.surface().content_generation;
            let forced = first_attempt && entering.starts_series;
            let starts_series = forced
                || self.state.lock().unwrap().request_surface_generation != generation
                || (route.tool_update.is_none() && self.tools_changed(&header));
            for commit in self.system_prompt_commits(
                &rendered,
                route.system_prompt_in_history,
                starts_series,
                turn,
                step,
            ) {
                self.append(commit).await?;
            }
            if first_attempt {
                for m in &entering.messages {
                    self.append(surface_append(kinds::USER_MESSAGE, message_json(m), None))
                        .await?;
                }
            }
            first_attempt = false;
            let request = self
                .build_request(header, effort, max_tokens, &tools, turn, step, forced)
                .await?;
            self.journal.flush().await?;
            check(cancel)?;
            attempt += 1;
            self.observer.on_attempt(turn, step, attempt);
            let provider = self.llm.provider().to_owned();
            let model = request.model.clone();

            let mut acc = StreamAccumulator::default();
            let mut asm = BlockAssembler::default();
            // Cancellation before the stream starts settles no attempt (like dsh).
            let mut stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Aborted.into()),
                s = self.llm.stream(request, cancel.child_token()) => s,
            };
            let mut interrupted = false;
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { interrupted = true; break; }
                    chunk = stream.next() => {
                        let Some(chunk) = chunk else { break };
                        if cancel.is_cancelled() { interrupted = true; break; }
                        acc.push(now_ms(), &chunk);
                        asm.push(&chunk);
                        self.emit_delta(turn, step, &chunk);
                    }
                }
            }
            drop(stream);
            if interrupted || cancel.is_cancelled() {
                let content = asm.interrupted_blocks();
                if content.is_empty() {
                    self.append(event(
                        crate::ASSISTANT_ATTEMPT,
                        json!({"turn": turn, "step": step, "stream": acc.snapshot()}),
                    ))
                    .await?;
                } else {
                    let message = assistant_message(content, &provider, &model, asm.replay_state());
                    let mut d = Map::new();
                    d.insert("turn".into(), turn.into());
                    d.insert("step".into(), step.into());
                    d.insert("message".into(), message_json(&message));
                    d.insert("interrupted".into(), true.into());
                    if let Some(u) = asm.usage() {
                        d.insert("usage".into(), serde_json::to_value(u)?);
                    }
                    d.insert("stream".into(), acc.snapshot());
                    self.append(surface_append(
                        kinds::ASSISTANT_MESSAGE,
                        Value::Object(d),
                        None,
                    ))
                    .await?;
                }
                return Err(Aborted.into());
            }

            let finish = asm.finish();
            if let FinishReason::Error { failure } | FinishReason::Aborted { failure } = &finish {
                self.append(event(
                    crate::ASSISTANT_ATTEMPT,
                    json!({"turn": turn, "step": step, "stream": acc.snapshot()}),
                ))
                .await?;
                let retry = self
                    .request_error(
                        turn,
                        step,
                        &provider,
                        failure,
                        &mut retries,
                        &mut retry_id,
                        cancel,
                    )
                    .await?;
                if retry {
                    continue;
                }
                return Err(LlmError(failure.clone()).into());
            }

            let message = assistant_message(asm.blocks(), &provider, &model, asm.replay_state());
            let mut d = Map::new();
            d.insert("turn".into(), turn.into());
            d.insert("step".into(), step.into());
            d.insert("message".into(), message_json(&message));
            if let Some(u) = asm.usage() {
                d.insert("usage".into(), serde_json::to_value(u)?);
            }
            d.insert("stream".into(), acc.snapshot());
            self.append(surface_append(
                kinds::ASSISTANT_MESSAGE,
                Value::Object(d),
                None,
            ))
            .await?;
            self.state.lock().unwrap().overflow_retries = 0;
            if matches!(finish, FinishReason::MaxTokens) {
                return Ok(Some(TurnEndReason::MaxTokens));
            }
            let calls: Vec<(String, String, String)> = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some((id.clone(), name.clone(), arguments.clone())),
                    _ => None,
                })
                .collect();
            if calls.is_empty() {
                return Ok(Some(TurnEndReason::Completed));
            }
            let concluded = self.execute_tool_calls(turn, step, calls, cancel).await?;
            return Ok(if concluded {
                Some(TurnEndReason::Completed)
            } else {
                None
            });
        }
    }

    fn emit_delta(&self, turn: u64, step: u64, chunk: &StreamChunk) {
        let (index, kind, text) = match chunk {
            StreamChunk::TextDelta { index, text } => (*index, DeltaKind::Text, text),
            StreamChunk::ReasoningDelta { index, text } => (*index, DeltaKind::Reasoning, text),
            _ => return,
        };
        self.observer.on_delta(&StreamDelta {
            turn,
            step,
            index,
            block_type: kind,
            text: text.clone(),
        });
    }

    /// dsh `agent/request-error` listeners: llm-retry, then overflow compaction.
    #[allow(clippy::too_many_arguments)]
    async fn request_error(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        provider: &str,
        failure: &LlmFailure,
        retries: &mut u32,
        retry_id: &mut Option<String>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<bool> {
        check(cancel)?;
        let policy = self.config.lock().unwrap().retry.clone();
        if let Some(delay) = policy.decide(failure, *retries) {
            *retries += 1;
            let id = retry_id.get_or_insert_with(dshx_core::new_id).clone();
            let data = policy.event_data(&id, turn, step, provider, *retries, delay, failure);
            self.append(event(kinds::LLM_RETRY, data)).await?;
            tokio::select! {
                _ = cancel.cancelled() => return Err(Aborted.into()),
                _ = tokio::time::sleep(std::time::Duration::from_micros((delay * 1000.0) as u64)) => {}
            }
            self.append(event(
                kinds::LLM_RETRY_STARTED,
                json!({"retryId": id, "turn": turn, "step": step, "retry": *retries}),
            ))
            .await?;
            return Ok(true);
        }
        if failure.code == "CONTEXT_WINDOW_EXCEEDED" {
            let (auto, max) = {
                let c = self.config.lock().unwrap();
                (c.compaction.auto, c.compaction.max_overflow_retries)
            };
            let used = self.state.lock().unwrap().overflow_retries;
            if !auto || used >= max || self.journal.request_header().is_none() {
                return Ok(false);
            }
            let generation = self.journal.surface().replace_generation;
            let result = crate::compaction::compact_if_needed(
                self,
                crate::compaction::Trigger::ContextOverflow,
                turn,
                cancel,
            )
            .await;
            if cancel.is_cancelled() {
                return Err(Aborted.into());
            }
            if let Err(e) = &result {
                tracing::warn!("context-overflow compaction failed: {e:#}");
            }
            if self.journal.surface().replace_generation > generation {
                self.state.lock().unwrap().overflow_retries = used + 1;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub(crate) fn assistant_message(
    content: Vec<ContentBlock>,
    provider: &str,
    model: &str,
    replay: Option<Value>,
) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::Assistant,
        content,
        source: MessageSource::Model {
            provider: provider.to_owned(),
            model: model.to_owned(),
            replay_state: replay,
        },
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}
