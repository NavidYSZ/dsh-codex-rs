//! The goal service (dsh `GoalService`, `ctx.goals`): durable state is the
//! owning session's `goal/change` events, activation (`armed`/`disarmed`)
//! is process-local and starts disarmed whenever an agent attaches.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use dshx_agent::Agent;
use dshx_core::{NewEvent, now_ms};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::driver::{self, DriverState};
use crate::fold::{
    BlockReason, GOAL_CHANGE, Goal, GoalChange, GoalFold, GoalState, Phase, safe_integer,
};

/// Deployment settings (dsh `goal.defaultMaxGoalRounds`, `tool-goal.blockedAfterConsecutiveRounds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalSettings {
    pub default_max_goal_rounds: u64,
    pub blocked_after_consecutive_rounds: u64,
}

impl Default for GoalSettings {
    fn default() -> Self {
        GoalSettings {
            default_max_goal_rounds: 256,
            blocked_after_consecutive_rounds: 3,
        }
    }
}

impl GoalSettings {
    pub fn from_config(c: &dshx_core::config::GoalConfig) -> Self {
        GoalSettings {
            default_max_goal_rounds: u64::from(c.default_max_goal_rounds.max(1)),
            blocked_after_consecutive_rounds: u64::from(c.blocked_after_consecutive_rounds.max(1)),
        }
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

/// The live view of the current goal (dsh `GoalView`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalView {
    pub goal: Goal,
    pub rounds_started: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub activation: Activation,
}

impl GoalView {
    fn new(state: &GoalState, activation: Activation) -> Self {
        GoalView {
            goal: state.goal.clone(),
            rounds_started: state.rounds_started,
            created_at: state.created_at,
            updated_at: state.updated_at,
            activation,
        }
    }

    /// `{...goal, roundsStarted, createdAt, updatedAt, activation}` (the UI and REST shape).
    pub fn to_json(&self) -> Value {
        let mut m = match self.goal.to_json() {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("roundsStarted".into(), self.rounds_started.into());
        m.insert("createdAt".into(), self.created_at.into());
        m.insert("updatedAt".into(), self.updated_at.into());
        m.insert("activation".into(), self.activation.as_str().into());
        Value::Object(m)
    }

    pub fn reference(&self) -> GoalRef {
        GoalRef {
            id: self.goal.id.clone(),
            revision: self.goal.revision,
        }
    }
}

/// The exact compare-and-set identity of one goal revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRef {
    pub id: String,
    pub revision: u64,
}

/// A refused goal operation. `name` is the dsh error class (`GoalError`,
/// `HarnessError`); `code` is `None` for an unclassified `Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalError {
    pub name: &'static str,
    pub code: Option<&'static str>,
    pub message: String,
}

impl GoalError {
    pub(crate) fn goal(code: &'static str, message: impl Into<String>) -> Self {
        GoalError {
            name: "GoalError",
            code: Some(code),
            message: message.into(),
        }
    }

    pub(crate) fn plain(message: impl Into<String>) -> Self {
        GoalError {
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

impl std::fmt::Display for GoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GoalError {}

/// A goal or activation change of one session, for live UIs.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalNotice {
    pub session_id: String,
    /// [`GoalView::to_json`], `None` when no goal is current.
    pub goal: Option<Value>,
}

/// One attached session.
pub(crate) struct Entry {
    pub agent: Agent,
    pub state: Mutex<EntryState>,
    /// Serializes mutations (read-check-append).
    pub ops: tokio::sync::Mutex<()>,
    pub driver: Mutex<Option<driver::Handle>>,
}

pub(crate) struct EntryState {
    pub fold: GoalFold,
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

    pub(crate) fn view(&self) -> Result<Option<GoalView>, GoalError> {
        self.refresh();
        let st = self.state.lock().unwrap();
        let current = st.fold.state().map_err(GoalError::plain)?;
        Ok(current.map(|c| GoalView::new(c, st.activation)))
    }
}

/// The per-process goal service. [`GoalService::attach`] every agent whose
/// tools may use goals; [`crate::goal_tools`] are its model-facing tools.
pub struct GoalService {
    settings: Mutex<GoalSettings>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<Entry>>>,
    notices: broadcast::Sender<GoalNotice>,
    pub(crate) me: Weak<GoalService>,
}

/// dsh `resolveMaxGoalRounds`.
fn max_rounds(v: &Value) -> Result<u64, GoalError> {
    match safe_integer(v) {
        Some(n) if n >= 1 => Ok(n as u64),
        _ => Err(GoalError::goal(
            "GOAL_INVALID_MAX_ROUNDS",
            "maxGoalRounds must be a positive safe integer",
        )),
    }
}

/// dsh `resolveObjective`.
fn objective(v: &str) -> Result<String, GoalError> {
    let t = v.trim();
    if t.is_empty() {
        return Err(GoalError::goal(
            "GOAL_INVALID_OBJECTIVE",
            "goal objective must be a non-empty string",
        ));
    }
    Ok(t.to_owned())
}

fn transition_error(current: &Goal, operation: &str, allowed: &[&str]) -> GoalError {
    GoalError::goal(
        "GOAL_INVALID_TRANSITION",
        format!(
            "cannot {operation} goal \"{}\" from phase \"{}\"; expected {}",
            current.id,
            current.phase.as_str(),
            allowed.join(" or ")
        ),
    )
}

fn expect_current<'a>(
    state: Option<&'a GoalState>,
    r: &GoalRef,
) -> Result<&'a GoalState, GoalError> {
    let state = state.ok_or_else(|| GoalError::goal("GOAL_NOT_FOUND", "no current goal"))?;
    let cur = &state.goal;
    if r.id != cur.id || r.revision != cur.revision {
        return Err(GoalError::goal(
            "GOAL_STALE_REVISION",
            format!(
                "stale goal ref \"{}\" revision {}; current is \"{}\" revision {}",
                r.id, r.revision, cur.id, cur.revision
            ),
        ));
    }
    Ok(state)
}

fn with_phase(current: &Goal, phase: Phase) -> Goal {
    Goal {
        id: current.id.clone(),
        revision: current.revision + 1,
        objective: current.objective.clone(),
        phase,
        max_goal_rounds: current.max_goal_rounds,
        blocked_reason: None,
    }
}

/// Who asked for a mutation: the agent's own tool call or someone else
/// (REST, the round driver). A pause from someone else stops a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    Agent,
    Host,
}

impl GoalService {
    pub fn new(settings: GoalSettings) -> Arc<GoalService> {
        Arc::new_cyclic(|me| GoalService {
            settings: Mutex::new(settings),
            sessions: Mutex::new(HashMap::new()),
            notices: broadcast::channel(256).0,
            me: me.clone(),
        })
    }

    pub fn settings(&self) -> GoalSettings {
        *self.settings.lock().unwrap()
    }

    pub fn set_settings(&self, settings: GoalSettings) {
        *self.settings.lock().unwrap() = settings;
    }

    /// Goal and activation changes of every attached session.
    pub fn subscribe(&self) -> broadcast::Receiver<GoalNotice> {
        self.notices.subscribe()
    }

    /// Registers a live agent (activation starts `disarmed`, like dsh's
    /// `agent/created`) and installs the goal-round pre-step check. With
    /// `drive`, the goal-round driver continues an armed goal whenever the
    /// agent is idle.
    pub fn attach(&self, agent: &Agent, drive: bool) {
        let id = agent.session_id();
        let entry = Arc::new(Entry {
            agent: agent.clone(),
            state: Mutex::new(EntryState {
                fold: GoalFold::default(),
                activation: Activation::Disarmed,
                driver: DriverState::default(),
            }),
            ops: tokio::sync::Mutex::new(()),
            driver: Mutex::new(None),
        });
        entry.refresh();
        let previous = self.sessions.lock().unwrap().insert(id, entry.clone());
        if let Some(previous) = previous {
            driver::stop_detached(&previous);
        }
        agent.add_pre_step_hook(Arc::new(driver::RoundHook {
            entry: Arc::downgrade(&entry),
        }));
        if drive {
            let handle = driver::start(self.me.clone(), entry.clone());
            *entry.driver.lock().unwrap() = Some(handle);
        }
    }

    /// Stops the driver of `session_id` (disarming its goal) and forgets the agent.
    pub async fn detach(&self, session_id: &str) {
        let entry = self.sessions.lock().unwrap().remove(session_id);
        if let Some(entry) = entry {
            driver::stop(&entry).await;
            self.set_activation(&entry, Activation::Disarmed);
        }
    }

    pub(crate) fn entry(&self, session_id: &str) -> Result<Arc<Entry>, GoalError> {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                GoalError::goal(
                    "GOAL_AGENT_NOT_LIVE",
                    format!("agent \"{session_id}\" is not live in this registry"),
                )
            })
    }

    /// Whether `session_id` has an attached agent.
    pub fn is_attached(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(session_id)
    }

    /// The current goal of a live session (dsh `goals.get`).
    pub fn get(&self, session_id: &str) -> Result<Option<GoalView>, GoalError> {
        self.entry(session_id)?.view()
    }

    /// Removes automatic continuation without changing the durable goal (dsh `disarm`).
    pub fn disarm(&self, session_id: &str) -> Result<Option<GoalView>, GoalError> {
        let entry = self.entry(session_id)?;
        self.set_activation(&entry, Activation::Disarmed);
        entry.view()
    }

    pub(crate) fn set_activation(&self, entry: &Entry, activation: Activation) {
        let changed = {
            let mut st = entry.state.lock().unwrap();
            let changed = st.activation != activation;
            st.activation = activation;
            changed
        };
        if changed {
            self.publish(entry);
        }
    }

    fn publish(&self, entry: &Entry) {
        let goal = entry.view().ok().flatten().map(|v| v.to_json());
        let _ = self.notices.send(GoalNotice {
            session_id: entry.agent.session_id(),
            goal,
        });
    }

    /// Creates and arms a goal; a completed goal may be replaced (dsh `create`).
    pub async fn create(
        &self,
        session_id: &str,
        goal_objective: &str,
        max_goal_rounds: Option<&Value>,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        let objective = objective(goal_objective)?;
        let max = match max_goal_rounds {
            Some(v) => max_rounds(v)?,
            None => self.settings().default_max_goal_rounds,
        };
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let current = entry.view()?;
        if let Some(c) = &current
            && c.goal.phase != Phase::Complete
        {
            return Err(GoalError::goal(
                "GOAL_ALREADY_EXISTS",
                format!(
                    "goal \"{}\" already exists with phase \"{}\"",
                    c.goal.id,
                    c.goal.phase.as_str()
                ),
            ));
        }
        let now = now_ms();
        let goal = Goal {
            id: format!("goal-{}", uuid::Uuid::new_v4()),
            revision: 1,
            objective,
            phase: Phase::Active,
            max_goal_rounds: max,
            blocked_reason: None,
        };
        let change = GoalChange::Snapshot {
            operation: "create".into(),
            goal,
            rounds_started: 0,
            created_at: now,
            updated_at: now,
        };
        self.commit(&entry, change, Activation::Armed, initiator)
            .await
    }

    /// Replaces the objective and/or round cap, keeping the phase (dsh `edit`).
    pub async fn edit(
        &self,
        session_id: &str,
        r: &GoalRef,
        new_objective: Option<&str>,
        max_goal_rounds: Option<&Value>,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (state, activation) = self.current(&entry)?;
        let state = expect_current(state.as_ref(), r)?;
        if new_objective.is_none() && max_goal_rounds.is_none() {
            return Err(GoalError::goal(
                "GOAL_INVALID_EDIT",
                "goal edit requires objective and/or maxGoalRounds",
            ));
        }
        let mut goal = state.goal.clone();
        goal.revision += 1;
        if let Some(o) = new_objective {
            goal.objective = objective(o)?;
        }
        if let Some(m) = max_goal_rounds {
            goal.max_goal_rounds = max_rounds(m)?;
        }
        let change = self.snapshot(state, "edit", goal);
        self.commit(&entry, change, activation, initiator).await
    }

    /// Pauses an active goal and disarms it.
    pub async fn pause(
        &self,
        session_id: &str,
        r: &GoalRef,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        self.transition(
            session_id,
            r,
            "pause",
            &["active"],
            Phase::Paused,
            Activation::Disarmed,
            initiator,
        )
        .await
    }

    /// Resumes and arms a stopped goal, or rearms a disarmed active one (dsh `resume`).
    pub async fn resume(
        &self,
        session_id: &str,
        r: &GoalRef,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (state, activation) = self.current(&entry)?;
        let state = expect_current(state.as_ref(), r)?;
        let cur = &state.goal;
        let resumable = ["active", "paused", "blocked"];
        if cur.phase == Phase::Complete {
            return Err(transition_error(cur, "resume", &resumable));
        }
        if cur.phase == Phase::Active && activation == Activation::Armed {
            return Err(GoalError::goal(
                "GOAL_INVALID_TRANSITION",
                format!("goal \"{}\" is already active and armed", cur.id),
            ));
        }
        if state.rounds_started >= cur.max_goal_rounds {
            return Err(GoalError::goal(
                "GOAL_INVALID_TRANSITION",
                format!(
                    "goal \"{}\" exhausted {} goal rounds; increase maxGoalRounds before resuming",
                    cur.id, cur.max_goal_rounds
                ),
            ));
        }
        let change = self.snapshot(state, "resume", with_phase(cur, Phase::Active));
        self.commit(&entry, change, Activation::Armed, initiator)
            .await
    }

    /// Marks a non-complete goal complete and disarms it.
    pub async fn complete(
        &self,
        session_id: &str,
        r: &GoalRef,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        self.transition(
            session_id,
            r,
            "complete",
            &["active", "paused", "blocked"],
            Phase::Complete,
            Activation::Disarmed,
            initiator,
        )
        .await
    }

    /// Marks an active goal blocked with a policy-owned reason and disarms it.
    pub async fn block(
        &self,
        session_id: &str,
        r: &GoalRef,
        code: &str,
        message: &str,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (state, _) = self.current(&entry)?;
        let state = expect_current(state.as_ref(), r)?;
        if state.goal.phase != Phase::Active {
            return Err(transition_error(&state.goal, "block", &["active"]));
        }
        if !crate::fold::kebab(code) || message.trim().is_empty() {
            return Err(GoalError::goal(
                "GOAL_INVALID_BLOCK_REASON",
                "goal block reason requires a lower-kebab-case code and a non-empty message",
            ));
        }
        let mut goal = with_phase(&state.goal, Phase::Blocked);
        goal.blocked_reason = Some(BlockReason {
            code: code.to_owned(),
            message: message.trim().to_owned(),
        });
        let change = self.snapshot(state, "block", goal);
        self.commit(&entry, change, Activation::Disarmed, initiator)
            .await
    }

    /// Clears the current goal with a durable tombstone (dsh `clear`).
    pub async fn clear(
        &self,
        session_id: &str,
        r: &GoalRef,
        initiator: Initiator,
    ) -> Result<GoalRef, GoalError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (state, _) = self.current(&entry)?;
        let state = expect_current(state.as_ref(), r)?;
        let tombstone = GoalRef {
            id: state.goal.id.clone(),
            revision: state.goal.revision + 1,
        };
        let change = GoalChange::Clear {
            id: tombstone.id.clone(),
            revision: tombstone.revision,
            cleared_at: now_ms().max(state.updated_at),
        };
        self.append(&entry, &change, Activation::Disarmed, initiator)
            .await?;
        Ok(tombstone)
    }

    fn current(&self, entry: &Entry) -> Result<(Option<GoalState>, Activation), GoalError> {
        entry.refresh();
        let st = entry.state.lock().unwrap();
        let current = st.fold.state().map_err(GoalError::plain)?.cloned();
        Ok((current, st.activation))
    }

    fn snapshot(&self, state: &GoalState, operation: &str, goal: Goal) -> GoalChange {
        GoalChange::Snapshot {
            operation: operation.into(),
            goal,
            rounds_started: state.rounds_started,
            created_at: state.created_at,
            updated_at: now_ms().max(state.updated_at),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn transition(
        &self,
        session_id: &str,
        r: &GoalRef,
        operation: &str,
        allowed: &[&str],
        phase: Phase,
        activation: Activation,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        let entry = self.entry(session_id)?;
        let _ops = entry.ops.lock().await;
        let (state, _) = self.current(&entry)?;
        let state = expect_current(state.as_ref(), r)?;
        if !allowed.contains(&state.goal.phase.as_str()) {
            return Err(transition_error(&state.goal, operation, allowed));
        }
        let change = self.snapshot(state, operation, with_phase(&state.goal, phase));
        self.commit(&entry, change, activation, initiator).await
    }

    async fn commit(
        &self,
        entry: &Arc<Entry>,
        change: GoalChange,
        activation: Activation,
        initiator: Initiator,
    ) -> Result<GoalView, GoalError> {
        self.append(entry, &change, activation, initiator).await?;
        entry
            .view()?
            .ok_or_else(|| GoalError::plain("goal disappeared after its change"))
    }

    /// Appends one change, applies its activation edge, then notifies.
    async fn append(
        &self,
        entry: &Arc<Entry>,
        change: &GoalChange,
        activation: Activation,
        initiator: Initiator,
    ) -> Result<(), GoalError> {
        entry
            .agent
            .append_event(NewEvent::new(GOAL_CHANGE, change.to_json()))
            .await
            .map_err(|e| GoalError::plain(format!("{e:#}")))?;
        entry.refresh();
        entry.state.lock().unwrap().activation = activation;
        self.publish(entry);
        driver::goal_changed(entry, change.operation(), initiator);
        Ok(())
    }
}

/// What a `/goal`-style request asks for (dsh `dsh-command-goal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCommand {
    Create(String),
    Edit(String),
    Pause,
    Resume,
    Clear,
}

impl GoalService {
    /// Runs one human goal command like dsh's `/goal`: create (a completed
    /// goal is replaced), edit (a completed goal is replaced by a new one),
    /// pause, resume, clear. `max_goal_rounds` caps a created goal (dsh's
    /// `/goal` always uses the default). Returns the goal afterwards.
    pub async fn command(
        &self,
        session_id: &str,
        command: GoalCommand,
        max_goal_rounds: Option<&Value>,
    ) -> Result<Option<GoalView>, GoalError> {
        let current = self.get(session_id)?;
        let missing = |action: &str| {
            GoalError::goal(
                "GOAL_NOT_FOUND",
                format!("No goal is currently set; /goal {action} requires one."),
            )
        };
        let host = Initiator::Host;
        Ok(match command {
            GoalCommand::Create(o) => {
                if let Some(c) = &current
                    && c.goal.phase != Phase::Complete
                {
                    return Err(GoalError::goal(
                        "GOAL_ALREADY_EXISTS",
                        format!(
                            "A goal is already {}. Use /goal edit <objective> to change it or /goal clear before replacing it.",
                            c.goal.phase.as_str()
                        ),
                    ));
                }
                Some(self.create(session_id, &o, max_goal_rounds, host).await?)
            }
            GoalCommand::Edit(o) => {
                let c = current.ok_or_else(|| missing("edit"))?;
                if c.goal.phase == Phase::Complete {
                    Some(self.create(session_id, &o, max_goal_rounds, host).await?)
                } else {
                    Some(
                        self.edit(session_id, &c.reference(), Some(&o), max_goal_rounds, host)
                            .await?,
                    )
                }
            }
            GoalCommand::Pause => {
                let c = current.ok_or_else(|| missing("pause"))?;
                Some(self.pause(session_id, &c.reference(), host).await?)
            }
            GoalCommand::Resume => {
                let c = current.ok_or_else(|| missing("resume"))?;
                Some(self.resume(session_id, &c.reference(), host).await?)
            }
            GoalCommand::Clear => {
                if let Some(c) = current {
                    self.clear(session_id, &c.reference(), host).await?;
                }
                None
            }
        })
    }
}

/// The durable goal of a stored session that has no live agent
/// (activation is always `disarmed` then).
pub fn stored_goal(events: &[dshx_core::SessionEvent]) -> Result<Option<GoalView>, GoalError> {
    let fold = GoalFold::from_events(events);
    let current = fold.state().map_err(GoalError::plain)?;
    Ok(current.map(|c| GoalView::new(c, Activation::Disarmed)))
}
