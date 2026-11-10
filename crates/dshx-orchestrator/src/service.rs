//! The orchestrator service: durable state is the owning session's
//! `orchestration/change` events (validated by replay through the fold, so
//! every mutation is checked by applying it to a clone of the fold before
//! it is appended); activation (`armed`/`disarmed`) is process-local and
//! starts disarmed whenever an agent attaches. Workers are one-shot
//! subagent children of the lead session.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use dshx_agent::Agent;
use dshx_core::{NewEvent, now_ms};
use dshx_subagent::{OneShot, RunResult, SubagentRuntime};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::driver::{self, DriverState, Settlement};
use crate::fold::{
    Check, Criterion, Limits, ORCHESTRATION_CHANGE, OrchestratorFold, Phase, Run, RunChange,
    RunState, TaskRecord, TaskSpec, TaskStatus, TaskUpdate,
};

/// Deployment settings (`[codex.orchestration]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestratorSettings {
    pub default_max_rounds: u64,
    pub default_max_parallel: u64,
    pub default_max_workers: u64,
    pub blocked_after_consecutive_rounds: u64,
}

impl Default for OrchestratorSettings {
    fn default() -> Self {
        OrchestratorSettings {
            default_max_rounds: 8,
            default_max_parallel: 3,
            default_max_workers: 12,
            blocked_after_consecutive_rounds: 3,
        }
    }
}

impl OrchestratorSettings {
    pub fn from_config(c: &dshx_core::config::OrchestrationConfig) -> Self {
        OrchestratorSettings {
            default_max_rounds: u64::from(c.default_max_rounds.clamp(1, 10_000)),
            default_max_parallel: u64::from(c.default_max_parallel.clamp(1, 8)),
            default_max_workers: u64::from(c.default_max_workers.clamp(1, 64)),
            blocked_after_consecutive_rounds: u64::from(c.blocked_after_consecutive_rounds.max(1)),
        }
    }

    /// Partial limits from REST or a tool; omitted fields use deployment defaults.
    pub fn parse_limits(&self, value: &Value) -> Result<Option<Limits>, OrchestratorError> {
        if value.is_null() {
            return Ok(None);
        }
        let invalid = |message: String| OrchestratorError::run("ORCH_INVALID_INPUT", message);
        let object = value
            .as_object()
            .ok_or_else(|| invalid("limits must be an object".into()))?;
        for key in object.keys() {
            if !matches!(key.as_str(), "maxRounds" | "maxParallel" | "maxWorkers") {
                return Err(invalid(format!("unknown limit {key}")));
            }
        }
        let limit = |key: &str, max: u64, default: u64| match object.get(key) {
            None => Ok(default),
            Some(v) => v.as_u64().filter(|n| (1..=max).contains(n)).ok_or_else(|| {
                invalid(format!(
                    "limits.{key} must be an integer between 1 and {max}"
                ))
            }),
        };
        Ok(Some(Limits {
            max_rounds: limit("maxRounds", 10_000, self.default_max_rounds)?,
            max_parallel: limit("maxParallel", 8, self.default_max_parallel)?,
            max_workers: limit("maxWorkers", 64, self.default_max_workers)?,
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Activation {
    Armed,
    Disarmed,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Armed => "armed",
            Activation::Disarmed => "disarmed",
        }
    }
}

/// The live view of the current run (the UI and REST shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunView {
    pub state: RunState,
    pub activation: Activation,
}

impl RunView {
    pub fn to_json(&self) -> Value {
        let run = &self.state.run;
        let mut m = match run.to_json() {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("roundsStarted".into(), self.state.rounds_started.into());
        m.insert("createdAt".into(), self.state.created_at.into());
        m.insert("updatedAt".into(), self.state.updated_at.into());
        m.insert("activation".into(), self.activation.as_str().into());
        m.insert("planVersion".into(), self.state.plan_version.into());
        m.insert(
            "workersDispatched".into(),
            self.state.workers_dispatched.into(),
        );
        if let Some(checks) = &self.state.checks {
            m.insert(
                "checks".into(),
                Value::Array(
                    checks
                        .iter()
                        .map(|c| json!({"criterionId": c.criterion_id, "evidence": c.evidence}))
                        .collect(),
                ),
            );
        }
        m.insert(
            "tasks".into(),
            Value::Array(
                self.state
                    .ordered_tasks()
                    .iter()
                    .map(|t| task_json(t))
                    .collect(),
            ),
        );
        Value::Object(m)
    }

    pub fn reference(&self) -> RunRef {
        RunRef {
            id: self.state.run.id.clone(),
            revision: self.state.run.revision,
        }
    }
}

fn task_json(t: &TaskRecord) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), t.id.clone().into());
    m.insert("description".into(), t.description.clone().into());
    m.insert(
        "dependsOn".into(),
        Value::Array(t.depends_on.iter().cloned().map(Value::String).collect()),
    );
    m.insert("status".into(), t.status.as_str().into());
    m.insert("planVersion".into(), t.plan_version.into());
    if let Some(c) = &t.child_id {
        m.insert("childId".into(), c.clone().into());
    }
    if let Some(s) = &t.summary {
        m.insert("summary".into(), s.clone().into());
    }
    if let Some(r) = &t.stop_reason {
        m.insert("stopReason".into(), r.clone().into());
    }
    Value::Object(m)
}

/// The exact compare-and-set identity of one run revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    pub id: String,
    pub revision: u64,
}

/// A refused orchestration operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorError {
    pub name: &'static str,
    pub code: Option<&'static str>,
    pub message: String,
}

impl OrchestratorError {
    pub(crate) fn run(code: &'static str, message: impl Into<String>) -> Self {
        OrchestratorError {
            name: "OrchestrationError",
            code: Some(code),
            message: message.into(),
        }
    }

    pub(crate) fn plain(message: impl Into<String>) -> Self {
        OrchestratorError {
            name: "Error",
            code: None,
            message: message.into(),
        }
    }

    pub fn into_tool_error(self) -> dshx_core::ToolError {
        match self.code {
            Some(code) => dshx_core::ToolError::coded(self.name, code, self.message),
            None => dshx_core::ToolError::Failed(self.message),
        }
    }
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OrchestratorError {}

/// A run or activation change of one session, for live UIs.
#[derive(Debug, Clone, PartialEq)]
pub struct OrchestratorNotice {
    pub session_id: String,
    /// [`RunView::to_json`], `None` when no run is current.
    pub orchestration: Option<Value>,
}

/// One attached session.
pub(crate) struct Entry {
    pub agent: Agent,
    pub state: Mutex<EntryState>,
    /// Serializes mutations (validate-append).
    pub ops: tokio::sync::Mutex<()>,
    pub driver: Mutex<Option<driver::Handle>>,
    /// Worker settlements reach the driver task through this channel.
    pub settlements_tx: mpsc::UnboundedSender<Settlement>,
}

pub(crate) struct EntryState {
    pub fold: OrchestratorFold,
    pub activation: Activation,
    pub driver: DriverState,
}

impl Entry {
    /// Folds the events appended since the last read.
    pub(crate) fn refresh(&self) {
        let from = self.state.lock().unwrap().fold.folded;
        let events = self.agent.events_from(from);
        self.state.lock().unwrap().fold.extend(&events);
    }

    pub(crate) fn view(&self) -> Result<Option<RunView>, OrchestratorError> {
        self.refresh();
        let st = self.state.lock().unwrap();
        let current = st.fold.state().map_err(OrchestratorError::plain)?;
        Ok(current.map(|c| RunView {
            state: c.clone(),
            activation: st.activation,
        }))
    }

    pub(crate) fn session_id(&self) -> String {
        self.agent.session_id()
    }
}

/// Who asked for a mutation: the lead's own tool call or someone else
/// (REST, the driver). A pause or cancel from someone else also stops the
/// lead's running turn and the in-flight workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    Agent,
    Host,
}

/// The per-process orchestrator service. [`OrchestratorService::attach`]
/// every top-level agent; [`crate::orchestrator_tools`] are its model-facing
/// tools.
pub struct OrchestratorService {
    settings: Mutex<OrchestratorSettings>,
    pub(crate) subagents: Arc<SubagentRuntime>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<Entry>>>,
    notices: broadcast::Sender<OrchestratorNotice>,
    pub(crate) me: Weak<OrchestratorService>,
}

fn transition_error(current: &Run, operation: &str, allowed: &[&str]) -> OrchestratorError {
    OrchestratorError::run(
        "ORCH_INVALID_TRANSITION",
        format!(
            "cannot {operation} run \"{}\" from phase \"{}\"; expected {}",
            current.id,
            current.phase.as_str(),
            allowed.join(" or ")
        ),
    )
}

fn expect_current<'a>(
    state: Option<&'a RunState>,
    r: &RunRef,
) -> Result<&'a RunState, OrchestratorError> {
    let state = state
        .ok_or_else(|| OrchestratorError::run("ORCH_NOT_FOUND", "no current orchestration run"))?;
    let cur = &state.run;
    if r.id != cur.id || r.revision != cur.revision {
        return Err(OrchestratorError::run(
            "ORCH_STALE_REVISION",
            format!(
                "stale run ref \"{}\" revision {}; current is \"{}\" revision {}",
                r.id, r.revision, cur.id, cur.revision
            ),
        ));
    }
    Ok(state)
}

/// A criterion or limit input as the tools and REST hand it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractInput {
    pub objective: String,
    pub criteria: Vec<Criterion>,
    pub limits: Option<Limits>,
}

impl OrchestratorService {
    pub fn new(
        settings: OrchestratorSettings,
        subagents: Arc<SubagentRuntime>,
    ) -> Arc<OrchestratorService> {
        Arc::new_cyclic(|me| OrchestratorService {
            settings: Mutex::new(settings),
            subagents,
            sessions: Mutex::new(HashMap::new()),
            notices: broadcast::channel(256).0,
            me: me.clone(),
        })
    }

    pub fn settings(&self) -> OrchestratorSettings {
        *self.settings.lock().unwrap()
    }

    pub fn set_settings(&self, settings: OrchestratorSettings) {
        *self.settings.lock().unwrap() = settings;
    }

    /// Run and activation changes of every attached session.
    pub fn subscribe(&self) -> broadcast::Receiver<OrchestratorNotice> {
        self.notices.subscribe()
    }

    /// Registers a live lead agent (activation starts `disarmed`), repairs
    /// tasks whose workers this process never had (a restart interrupted
    /// them) and installs the round pre-step check. With `drive`, the driver
    /// continues an armed run whenever the agent is idle.
    pub fn attach(&self, agent: &Agent, drive: bool) {
        let id = agent.session_id();
        let (tx, rx) = mpsc::unbounded_channel();
        let entry = Arc::new(Entry {
            agent: agent.clone(),
            state: Mutex::new(EntryState {
                fold: OrchestratorFold::default(),
                activation: Activation::Disarmed,
                driver: DriverState::default(),
            }),
            ops: tokio::sync::Mutex::new(()),
            driver: Mutex::new(None),
            settlements_tx: tx,
        });
        entry.refresh();
        let previous = self.sessions.lock().unwrap().insert(id, entry.clone());
        if let Some(previous) = previous {
            driver::stop_detached(&previous);
        }
        agent.add_pre_step_hook(Arc::new(driver::RoundHook {
            entry: Arc::downgrade(&entry),
            service: self.me.clone(),
        }));
        if drive && let Some(me) = self.me.upgrade() {
            me.recover_interrupted(&entry);
            let handle = driver::start(self.me.clone(), entry.clone(), rx);
            *entry.driver.lock().unwrap() = Some(handle);
        }
    }

    /// Marks tasks `running` in the fold but unknown to this process (their
    /// workers died with a restart) as failed, so the planner can replan.
    fn recover_interrupted(&self, entry: &Arc<Entry>) {
        let interrupted: Vec<String> = {
            entry.refresh();
            let Ok(Some(view)) = entry.view() else {
                return;
            };
            let in_flight = entry.state.lock().unwrap().driver.worker_ids();
            view.state
                .ordered_tasks()
                .iter()
                .filter(|t| t.status == TaskStatus::Running && !in_flight.contains(&t.id))
                .map(|t| t.id.clone())
                .collect()
        };
        if interrupted.is_empty() {
            return;
        }
        let me = self.me.clone();
        let entry = entry.clone();
        tokio::spawn(async move {
            for id in interrupted {
                let update = TaskUpdate {
                    id,
                    status: TaskStatus::Failed,
                    child_id: None,
                    summary: Some("interrupted: the worker did not survive a restart".into()),
                    stop_reason: Some("interrupted".into()),
                };
                if let Some(service) = me.upgrade()
                    && let Err(e) = service.record_task(&entry, update, Initiator::Host).await
                {
                    tracing::warn!(
                        "orchestrator: recovery of agent \"{}\" failed: {e}",
                        entry.session_id()
                    );
                }
            }
        });
    }

    /// Stops the driver of `session_id` (cancelling its workers) and forgets
    /// the agent.
    pub async fn detach(&self, session_id: &str) {
        let entry = self.sessions.lock().unwrap().remove(session_id);
        if let Some(entry) = entry {
            driver::stop(&entry).await;
        }
    }

    /// Stops all planners and workers before the host tears down subagents.
    pub async fn shutdown(&self) {
        let entries: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, e)| e)
            .collect();
        for entry in entries {
            driver::stop(&entry).await;
        }
    }

    pub(crate) fn entry(&self, session_id: &str) -> Result<Arc<Entry>, OrchestratorError> {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                OrchestratorError::run(
                    "ORCH_AGENT_NOT_LIVE",
                    format!("agent \"{session_id}\" is not live in this registry"),
                )
            })
    }

    /// Whether `session_id` has an attached agent.
    pub fn is_attached(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(session_id)
    }

    /// The current run of a live session.
    pub fn get(&self, session_id: &str) -> Result<Option<RunView>, OrchestratorError> {
        self.entry(session_id)?.view()
    }

    pub(crate) fn set_activation(&self, entry: &Entry, activation: Activation) {
        let changed = {
            let mut st = entry.state.lock().unwrap();
            if activation == Activation::Disarmed {
                st.driver.cancel_workers();
            }
            let changed = st.activation != activation;
            st.activation = activation;
            changed
        };
        if changed {
            self.publish(entry);
        }
    }

    fn publish(&self, entry: &Entry) {
        let orchestration = entry.view().ok().flatten().map(|v| v.to_json());
        let _ = self.notices.send(OrchestratorNotice {
            session_id: entry.session_id(),
            orchestration,
        });
    }

    /// Creates and arms a run; a completed or cancelled run may be replaced.
    pub async fn create(
        &self,
        session_id: &str,
        contract: ContractInput,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let settings = self.settings();
        let objective = contract.objective.trim().to_owned();
        if objective.is_empty() {
            return Err(OrchestratorError::run(
                "ORCH_INVALID_OBJECTIVE",
                "run objective must be a non-empty string",
            ));
        }
        if contract.criteria.is_empty() {
            return Err(OrchestratorError::run(
                "ORCH_INVALID_CRITERIA",
                "a run needs at least one acceptance criterion",
            ));
        }
        let limits = contract.limits.unwrap_or(Limits {
            max_rounds: settings.default_max_rounds,
            max_parallel: settings.default_max_parallel,
            max_workers: settings.default_max_workers,
        });
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let current = entry.view()?;
        if let Some(c) = &current
            && c.state.run.phase != Phase::Complete
            && c.state.run.phase != Phase::Cancelled
        {
            return Err(OrchestratorError::run(
                "ORCH_ALREADY_EXISTS",
                format!(
                    "run \"{}\" already exists with phase \"{}\"",
                    c.state.run.id,
                    c.state.run.phase.as_str()
                ),
            ));
        }
        let now = now_ms();
        let run = Run {
            id: format!("orch-{}", uuid::Uuid::new_v4()),
            revision: 1,
            objective,
            criteria: contract.criteria,
            limits,
            phase: Phase::Active,
            blocked_reason: None,
        };
        let change = RunChange::Create {
            run,
            rounds_started: 0,
            created_at: now,
            updated_at: now,
        };
        self.commit(&entry, change, Activation::Armed, initiator)
            .await
    }

    /// Submits the next plan version (the model-facing planning move).
    pub async fn plan(
        &self,
        session_id: &str,
        r: &RunRef,
        tasks: Vec<TaskSpec>,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            if state.run.phase != Phase::Active {
                return Err(transition_error(&state.run, "plan", &["active"]));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            (
                RunChange::Plan {
                    run,
                    plan_version: state.plan_version + 1,
                    tasks,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, activation, initiator).await
    }

    /// Completes the run; every criterion must be covered by one check with
    /// cited evidence.
    pub async fn complete(
        &self,
        session_id: &str,
        r: &RunRef,
        checks: Vec<Check>,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, _activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            if state.run.phase != Phase::Active {
                return Err(transition_error(&state.run, "complete", &["active"]));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            run.phase = Phase::Complete;
            (
                RunChange::Complete {
                    run,
                    checks,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, Activation::Disarmed, initiator)
            .await
    }

    /// Marks an active run blocked with a policy-owned reason and disarms it.
    pub async fn block(
        &self,
        session_id: &str,
        r: &RunRef,
        code: &str,
        message: &str,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, _activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            if state.run.phase != Phase::Active {
                return Err(transition_error(&state.run, "block", &["active"]));
            }
            if !crate::fold::kebab(code) || message.trim().is_empty() {
                return Err(OrchestratorError::run(
                    "ORCH_INVALID_BLOCK_REASON",
                    "run block reason requires a lower-kebab-case code and a non-empty message",
                ));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            run.phase = Phase::Blocked;
            run.blocked_reason = Some(crate::fold::BlockReason {
                code: code.to_owned(),
                message: message.trim().to_owned(),
            });
            (
                RunChange::Block {
                    run,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, Activation::Disarmed, initiator)
            .await
    }

    /// Pauses an active run and disarms it (host only).
    pub async fn pause(
        &self,
        session_id: &str,
        r: &RunRef,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, _activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            if state.run.phase != Phase::Active {
                return Err(transition_error(&state.run, "pause", &["active"]));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            run.phase = Phase::Paused;
            (
                RunChange::Pause {
                    run,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, Activation::Disarmed, initiator)
            .await
    }

    /// Resumes a stopped run or rearms a disarmed active run (host only).
    pub async fn resume(
        &self,
        session_id: &str,
        r: &RunRef,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, _activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            let resumable = ["active", "paused", "blocked"];
            if !resumable.contains(&state.run.phase.as_str())
                || (state.run.phase == Phase::Active && st.activation == Activation::Armed)
            {
                return Err(transition_error(&state.run, "resume", &resumable));
            }
            if state.rounds_started >= state.run.limits.max_rounds {
                return Err(OrchestratorError::run(
                    "ORCH_INVALID_TRANSITION",
                    format!(
                        "run \"{}\" exhausted {} rounds; the limit cannot be raised (start a new run instead)",
                        state.run.id, state.run.limits.max_rounds
                    ),
                ));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            run.phase = Phase::Active;
            run.blocked_reason = None;
            (
                RunChange::Resume {
                    run,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, Activation::Armed, initiator)
            .await
    }

    /// Cancels a run (host only).
    pub async fn cancel(
        &self,
        session_id: &str,
        r: &RunRef,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (change, _activation) = {
            entry.refresh();
            let st = entry.state.lock().unwrap();
            let state = st.fold.state().map_err(OrchestratorError::plain)?.cloned();
            let state = expect_current(state.as_ref(), r)?;
            let cancellable = ["active", "paused", "blocked"];
            if !cancellable.contains(&state.run.phase.as_str()) {
                return Err(transition_error(&state.run, "cancel", &cancellable));
            }
            let mut run = state.run.clone();
            run.revision += 1;
            run.phase = Phase::Cancelled;
            (
                RunChange::Cancel {
                    run,
                    rounds_started: state.rounds_started,
                    created_at: state.created_at,
                    updated_at: now_ms().max(state.updated_at),
                },
                st.activation,
            )
        };
        self.commit(&entry, change, Activation::Disarmed, initiator)
            .await
    }

    /// Clears the current run with a durable tombstone (host only).
    pub async fn clear(
        &self,
        session_id: &str,
        r: &RunRef,
        initiator: Initiator,
    ) -> Result<RunRef, OrchestratorError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        entry.refresh();
        let state = {
            let st = entry.state.lock().unwrap();
            st.fold.state().map_err(OrchestratorError::plain)?.cloned()
        };
        let state = expect_current(state.as_ref(), r)?;
        let tombstone = RunRef {
            id: state.run.id.clone(),
            revision: state.run.revision + 1,
        };
        let change = RunChange::Clear {
            id: tombstone.id.clone(),
            revision: tombstone.revision,
            cleared_at: now_ms().max(state.updated_at),
        };
        self.append(&entry, &change, Activation::Disarmed, initiator)
            .await?;
        Ok(tombstone)
    }

    /// Records one worker edge (dispatch or settlement); driver-owned.
    pub(crate) async fn record_task(
        &self,
        entry: &Arc<Entry>,
        update: TaskUpdate,
        initiator: Initiator,
    ) -> Result<(), OrchestratorError> {
        let _ops = entry.ops.lock().await;
        self.record_task_locked(entry, update, initiator).await
    }

    /// The caller holds `entry.ops` across spawning and recording the dispatch.
    pub(crate) async fn record_task_locked(
        &self,
        entry: &Arc<Entry>,
        update: TaskUpdate,
        initiator: Initiator,
    ) -> Result<(), OrchestratorError> {
        entry.refresh();
        let activation = entry.state.lock().unwrap().activation;
        let change = {
            let st = entry.state.lock().unwrap();
            let state = st
                .fold
                .state()
                .map_err(OrchestratorError::plain)?
                .cloned()
                .ok_or_else(|| {
                    OrchestratorError::run("ORCH_NOT_FOUND", "no current orchestration run")
                })?;
            let mut run = state.run.clone();
            run.revision += 1;
            RunChange::Task {
                run,
                task: update,
                rounds_started: state.rounds_started,
                created_at: state.created_at,
                updated_at: now_ms().max(state.updated_at),
            }
        };
        self.append(entry, &change, activation, initiator).await
    }

    /// Spawns one worker child for `task_id` of the current plan.
    pub(crate) async fn spawn_worker(
        &self,
        entry: &Arc<Entry>,
        state: &RunState,
        task_id: &str,
        cancel: CancellationToken,
    ) -> Result<OneShot, OrchestratorError> {
        let task = state.tasks.get(task_id).ok_or_else(|| {
            OrchestratorError::run(
                "ORCH_TASK_NOT_FOUND",
                format!("task \"{task_id}\" is not planned"),
            )
        })?;
        let prompt = worker_prompt(&state.run, task, &state.tasks);
        let label = format!("orchestration {}", task.id);
        self.subagents
            .start_one_shot_child(
                &entry.session_id(),
                "subagent",
                &label,
                &prompt,
                None,
                cancel,
            )
            .await
            .map_err(|e| {
                OrchestratorError::run(
                    "ORCH_WORKER_REFUSED",
                    format!(
                        "worker spawn for task \"{task_id}\" was refused: {}",
                        e.message
                    ),
                )
            })
    }

    async fn commit(
        &self,
        entry: &Arc<Entry>,
        change: RunChange,
        activation: Activation,
        initiator: Initiator,
    ) -> Result<RunView, OrchestratorError> {
        self.append(entry, &change, activation, initiator).await?;
        entry
            .view()?
            .ok_or_else(|| OrchestratorError::plain("run disappeared after its change"))
    }

    /// Validates the change against a clone of the fold, appends it, then
    /// notifies the driver and the UI.
    async fn append(
        &self,
        entry: &Arc<Entry>,
        change: &RunChange,
        activation: Activation,
        initiator: Initiator,
    ) -> Result<(), OrchestratorError> {
        // Typed callers must pass the same shape and evidence validation as
        // replay. Reject before appending so invalid input cannot poison a log.
        let validated = crate::fold::decode_change(&change.to_json())
            .map_err(OrchestratorError::plain)?
            .ok_or_else(|| OrchestratorError::plain("missing orchestration change"))?;
        {
            // Validate before appending: the fold is the single authority.
            let mut candidate = entry.state.lock().unwrap().fold.clone();
            candidate
                .apply_change(&validated)
                .map_err(OrchestratorError::plain)?;
        }
        entry
            .agent
            .append_event(NewEvent::new(ORCHESTRATION_CHANGE, change.to_json()))
            .await
            .map_err(|e| OrchestratorError::plain(format!("{e:#}")))?;
        entry.refresh();
        entry.state.lock().unwrap().activation = activation;
        self.publish(entry);
        driver::run_changed(entry, change.operation(), initiator);
        Ok(())
    }
}

/// The task prompt of one worker child.
pub(crate) fn worker_prompt(
    run: &Run,
    task: &TaskRecord,
    tasks: &HashMap<String, TaskRecord>,
) -> String {
    let mut deps = String::new();
    for d in &task.depends_on {
        match tasks.get(d) {
            Some(dep) if dep.status == TaskStatus::Completed => {
                deps.push_str(&format!(
                    "- [{}] completed: {}\n  worker transcript: session {}\n",
                    dep.id,
                    dep.summary.as_deref().unwrap_or("no summary"),
                    dep.child_id.as_deref().unwrap_or("unknown")
                ));
            }
            _ => deps.push_str(&format!("- [{d}]: no recorded result\n")),
        }
    }
    let criteria = run
        .criteria
        .iter()
        .map(|c| format!("- [{}] {}", c.id, c.criterion))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<orchestration_task>\nRun objective: {}\n\nThe run's acceptance criteria (context only; you own exactly one task):\n{criteria}\n\nYour task [{}]:\n{}\n\nResults this task depends on:\n{deps}\nComplete exactly this task in the current workspace. Finish with a concise summary of the outcome and name the concrete artifacts (file paths, URLs, commands) that support it.\n</orchestration_task>",
        serde_json::to_string(&run.objective).unwrap_or_default(),
        task.id,
        task.description,
    )
}

/// A settled worker's facts, as the driver records them.
pub(crate) fn settlement_update(
    task_id: &str,
    child_id: &str,
    result: &Result<RunResult, tokio::sync::oneshot::error::RecvError>,
) -> TaskUpdate {
    match result {
        Ok(r) if r.stop_reason == "completed" => TaskUpdate {
            id: task_id.to_owned(),
            status: TaskStatus::Completed,
            child_id: None,
            summary: Some(worker_summary(r)),
            stop_reason: None,
        },
        Ok(r) => TaskUpdate {
            id: task_id.to_owned(),
            status: TaskStatus::Failed,
            child_id: None,
            summary: Some(worker_summary(r)),
            stop_reason: Some(r.stop_reason.to_owned()),
        },
        Err(_) => TaskUpdate {
            id: task_id.to_owned(),
            status: TaskStatus::Failed,
            child_id: None,
            summary: Some(format!(
                "worker session {child_id} was dropped before reporting"
            )),
            stop_reason: Some("dropped".into()),
        },
    }
}

/// The worker's final text, bounded to the fold's summary limit.
fn worker_summary(result: &RunResult) -> String {
    let mut text = result
        .output
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(failure) = &result.failure {
        text = format!("{text}\ninfrastructure failure: {failure}");
    }
    let text = text.trim();
    if text.is_empty() {
        return "no final report".into();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= 2000 {
        text.to_owned()
    } else {
        format!("{}…", String::from_utf16_lossy(&units[..1999]))
    }
}

/// What a `/orchestration`-style REST request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestrationCommand {
    Create(ContractInput),
    Pause,
    Resume,
    Cancel,
    Clear,
}

impl OrchestratorService {
    /// Runs one human command: create (a finished run is replaced), pause,
    /// resume, cancel or clear. Returns the run afterwards.
    pub async fn command(
        &self,
        session_id: &str,
        command: OrchestrationCommand,
    ) -> Result<Option<RunView>, OrchestratorError> {
        let current = self.get(session_id)?;
        let missing = |action: &str| {
            OrchestratorError::run(
                "ORCH_NOT_FOUND",
                format!("No orchestration run is current; {action} requires one."),
            )
        };
        let host = Initiator::Host;
        Ok(match command {
            OrchestrationCommand::Create(contract) => {
                if let Some(c) = &current
                    && c.state.run.phase != Phase::Complete
                    && c.state.run.phase != Phase::Cancelled
                {
                    return Err(OrchestratorError::run(
                        "ORCH_ALREADY_EXISTS",
                        format!(
                            "A run is already {}. Clear it before starting another.",
                            c.state.run.phase.as_str()
                        ),
                    ));
                }
                Some(self.create(session_id, contract, host).await?)
            }
            OrchestrationCommand::Pause => {
                let c = current.ok_or_else(|| missing("pause"))?;
                Some(self.pause(session_id, &c.reference(), host).await?)
            }
            OrchestrationCommand::Resume => {
                let c = current.ok_or_else(|| missing("resume"))?;
                Some(self.resume(session_id, &c.reference(), host).await?)
            }
            OrchestrationCommand::Cancel => {
                let c = current.ok_or_else(|| missing("cancel"))?;
                Some(self.cancel(session_id, &c.reference(), host).await?)
            }
            OrchestrationCommand::Clear => {
                if let Some(c) = current {
                    self.clear(session_id, &c.reference(), host).await?;
                }
                None
            }
        })
    }
}

/// The durable run of a stored session that has no live agent
/// (activation is always `disarmed` then).
pub fn stored_orchestration(
    events: &[dshx_core::SessionEvent],
) -> Result<Option<RunView>, OrchestratorError> {
    let fold = OrchestratorFold::from_events(events);
    let current = fold.state().map_err(OrchestratorError::plain)?;
    Ok(current.map(|c| RunView {
        state: c.clone(),
        activation: Activation::Disarmed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_prompt_names_task_and_deps() {
        let run = Run {
            id: "orch-1".into(),
            revision: 3,
            objective: "Report".into(),
            criteria: vec![Criterion {
                id: "c1".into(),
                criterion: "report exists".into(),
            }],
            limits: Limits {
                max_rounds: 4,
                max_parallel: 2,
                max_workers: 6,
            },
            phase: Phase::Active,
            blocked_reason: None,
        };
        let mut tasks = HashMap::new();
        tasks.insert(
            "t-a".to_owned(),
            TaskRecord {
                id: "t-a".into(),
                description: "Gather".into(),
                depends_on: vec![],
                status: TaskStatus::Completed,
                plan_version: 1,
                child_id: Some("sess-a".into()),
                summary: Some("3 sources".into()),
                stop_reason: None,
            },
        );
        let task = TaskRecord {
            id: "t-b".into(),
            description: "Write".into(),
            depends_on: vec!["t-a".into()],
            status: TaskStatus::Pending,
            plan_version: 1,
            child_id: None,
            summary: None,
            stop_reason: None,
        };
        let prompt = worker_prompt(&run, &task, &tasks);
        assert!(prompt.contains("Your task [t-b]:"));
        assert!(prompt.contains("[t-a] completed: 3 sources"));
        assert!(prompt.contains("session sess-a"));
        assert!(prompt.contains("[c1] report exists"));
    }

    #[tokio::test]
    async fn settlement_update_maps_stop_reasons() {
        let result = RunResult {
            output: vec![json!({"type": "text", "text": "did it"})],
            stop_reason: "completed",
            failure: None,
        };
        let update = settlement_update("t-1", "s1", &Ok(result));
        assert_eq!(update.status, TaskStatus::Completed);
        assert_eq!(update.summary.as_deref(), Some("did it"));
        let result = RunResult {
            output: vec![],
            stop_reason: "aborted",
            failure: Some("boom".into()),
        };
        let update = settlement_update("t-1", "s1", &Ok(result));
        assert_eq!(update.status, TaskStatus::Failed);
        assert_eq!(update.stop_reason.as_deref(), Some("aborted"));
        assert!(update.summary.as_deref().unwrap().contains("boom"));
        let (tx, dropped) = tokio::sync::oneshot::channel::<RunResult>();
        drop(tx);
        let result = dropped.await;
        let update = settlement_update("t-1", "s1", &result);
        assert_eq!(update.status, TaskStatus::Failed);
        assert_eq!(update.stop_reason.as_deref(), Some("dropped"));
    }
}
