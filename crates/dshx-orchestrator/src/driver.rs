//! The orchestration round driver: when the lead is idle and its run is
//! active and armed, it records settled workers, dispatches ready tasks of
//! the current plan (one-shot subagent children, dependency order, bounded
//! by `maxParallel` and `maxWorkers`), and queues one reserved
//! `<orchestration_round>` prompt when no work can run. A pre-step check
//! admits only the exact reserved round. A user abort of a round pauses the
//! run, `max-tokens` and agent errors disarm it, the round cap blocks it.
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use dshx_agent::{AgentObserver, AgentStatus, CancelCause, PreStep, PreStepHook};
use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent, kinds};
use serde_json::json;
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::fold::Phase;
use crate::fold::TaskStatus;
use crate::service::{Activation, Entry, Initiator, OrchestratorService, RunView};

/// One settled worker waiting to be recorded.
#[derive(Debug)]
pub(crate) struct Settlement {
    pub run_id: String,
    pub task_id: String,
    pub update: crate::fold::TaskUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase2 {
    Queued,
    Claimed,
    Admitted,
}

/// The one reserved round.
#[derive(Debug, Clone)]
struct Attempt {
    run_id: String,
    revision: u64,
    round: u64,
    message_id: String,
    content: Vec<ContentBlock>,
    phase: Phase2,
    cancelled: bool,
    stale: bool,
}

/// The driver's per-agent state (shared with the pre-step check).
#[derive(Debug, Default)]
pub(crate) struct DriverState {
    attempt: Option<Attempt>,
    competing_queued: bool,
    needs_checkpoint: bool,
    requested: bool,
    stopping: bool,
    /// Running workers of this process: task id → cancel token.
    pub(crate) workers: HashMap<String, CancellationToken>,
    /// Settled workers that were recorded while a turn ran.
    pending: Vec<Settlement>,
}

impl DriverState {
    pub(crate) fn worker_ids(&self) -> Vec<String> {
        self.workers.keys().cloned().collect()
    }

    /// Cancels every running worker (their settlements still arrive).
    pub(crate) fn cancel_workers(&self) {
        for token in self.workers.values() {
            token.cancel();
        }
    }
}

pub(crate) struct Handle {
    stop: CancellationToken,
    wake: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

fn source_round(source: &MessageSource) -> Option<(String, u64, u64)> {
    let MessageSource::Other(v) = source else {
        return None;
    };
    crate::fold::round_source(v)
        .ok()
        .flatten()
        .filter(|(_, _, r)| *r > 0)
}

/// A bounded one-line task digest for the round prompt's board.
fn task_line(t: &crate::fold::TaskRecord) -> String {
    let status = match t.status {
        TaskStatus::Pending => {
            if t.depends_on.is_empty() {
                "pending".to_owned()
            } else {
                format!("pending — waits for [{}]", t.depends_on.join(", "))
            }
        }
        TaskStatus::Running => "running".to_owned(),
        TaskStatus::Completed => format!(
            "completed — {}",
            short(t.summary.as_deref().unwrap_or("no summary"))
        ),
        TaskStatus::Failed => format!(
            "failed ({}) — {}",
            t.stop_reason.as_deref().unwrap_or("unknown"),
            short(t.summary.as_deref().unwrap_or("no summary"))
        ),
    };
    format!("[{}] {status}", t.id)
}

fn short(s: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    if units.len() <= 160 {
        s.to_owned()
    } else {
        format!("{}…", String::from_utf16_lossy(&units[..159]))
    }
}

/// The reserved planner-round prompt with the current board.
pub fn round_prompt(view: &RunView) -> String {
    let run = &view.state.run;
    let criteria = run
        .criteria
        .iter()
        .map(|c| format!("- [{}] {}", c.id, c.criterion))
        .collect::<Vec<_>>()
        .join("\n");
    let board = if view.state.plan_version == 0 {
        "No plan yet: submit version 1 with update_orchestration (action plan).".to_owned()
    } else {
        view.state
            .ordered_tasks()
            .iter()
            .map(|t| task_line(t))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "<orchestration_round>\nObjective: {}\nRound: {}/{}\n\nAcceptance criteria:\n{criteria}\n\nBoard (plan v{}, workers used {}/{}):\n{board}\n\nDecide the next move for this run:\n- Work remaining: submit the next plan version (action plan). Ready tasks are dispatched to workers automatically after your turn; keep plans small and dependency-ordered, and prefer one task when one task suffices.\n- Every criterion met: complete the run (action complete), citing concrete evidence for each criterion.\n- Progress impossible: block the run (action blocked) with a lower-kebab-case code — needs-input when only the user can unblock.\n</orchestration_round>",
        serde_json::to_string(&run.objective).unwrap_or_default(),
        view.state.rounds_started + 1,
        run.limits.max_rounds,
        view.state.plan_version,
        view.state.workers_dispatched,
        run.limits.max_workers,
    )
}

fn round_message(view: &RunView, round: u64) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(round_prompt(view))],
        source: MessageSource::Other(json!({
            "kind": "orchestration",
            "runId": view.state.run.id,
            "revision": view.state.run.revision,
            "round": round,
        })),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// dsh `sameQueued`: the queued record is exactly the reservation.
fn same_queued(message: &Message, attempt: &Attempt) -> bool {
    source_round(&message.source).is_some_and(|(id, rev, round)| {
        id == attempt.run_id && rev == attempt.revision && round == attempt.round
    }) && message.content == attempt.content
}

/// Live status edges of the agent, for the driver task.
struct StatusRelay(mpsc::UnboundedSender<AgentStatus>);

impl AgentObserver for StatusRelay {
    fn on_status(&self, status: &AgentStatus) {
        let _ = self.0.send(status.clone());
    }
}

pub(crate) fn start(
    service: Weak<OrchestratorService>,
    entry: Arc<Entry>,
    settlements: mpsc::UnboundedReceiver<Settlement>,
) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    entry.agent.add_observer(Arc::new(StatusRelay(tx)));
    let log = entry.agent.subscribe();
    let stop = CancellationToken::new();
    let wake = Arc::new(Notify::new());
    let task = tokio::spawn(run(
        service,
        entry,
        log,
        rx,
        settlements,
        stop.clone(),
        wake.clone(),
    ));
    Handle { stop, wake, task }
}

/// Teardown: disarm, cancel workers, stop a running round (cause `parent`).
pub(crate) async fn stop(entry: &Arc<Entry>) {
    let handle = entry.driver.lock().unwrap().take();
    {
        let mut st = entry.state.lock().unwrap();
        st.driver.stopping = true;
        st.activation = Activation::Disarmed;
        st.driver.cancel_workers();
        if let Some(a) = st.driver.attempt.as_mut() {
            a.stale = true;
        }
    }
    if entry.agent.status() == AgentStatus::Running {
        entry.agent.cancel_with(CancelCause::Parent, false).await;
    }
    if let Some(h) = handle {
        h.stop.cancel();
        let _ = h.task.await;
    }
}

/// [`stop`] for a replaced entry, without waiting.
pub(crate) fn stop_detached(entry: &Arc<Entry>) {
    let entry = entry.clone();
    tokio::spawn(async move { stop(&entry).await });
}

fn request(entry: &Entry) {
    entry.state.lock().unwrap().driver.requested = true;
    if let Some(h) = entry.driver.lock().unwrap().as_ref() {
        h.wake.notify_one();
    }
}

/// A run changed: checkpoint before the next round; a pause or cancel also
/// stops a running turn and the in-flight workers.
pub(crate) fn run_changed(entry: &Arc<Entry>, operation: &str, initiator: Initiator) {
    {
        let mut st = entry.state.lock().unwrap();
        st.driver.needs_checkpoint = true;
        if matches!(
            operation,
            "pause" | "cancel" | "clear" | "complete" | "block"
        ) {
            st.driver.cancel_workers();
        }
        if matches!(operation, "create" | "clear") {
            for (_, token) in st.driver.workers.drain() {
                token.cancel();
            }
            st.driver.pending.clear();
        }
    }
    if matches!(operation, "pause" | "cancel" | "clear")
        && initiator == Initiator::Host
        && entry.agent.status() == AgentStatus::Running
    {
        let agent = entry.agent.clone();
        tokio::spawn(async move { agent.cancel_with(CancelCause::User, true).await });
    }
    request(entry);
}

/// Waits (briefly) until the agent's mirror holds event `seq`.
async fn settle(entry: &Entry, seq: u64) {
    for _ in 0..1000 {
        if entry.agent.log_len() > seq {
            return;
        }
        tokio::task::yield_now().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    service: Weak<OrchestratorService>,
    entry: Arc<Entry>,
    mut log: broadcast::Receiver<SessionEvent>,
    mut status: mpsc::UnboundedReceiver<AgentStatus>,
    mut settlements: mpsc::UnboundedReceiver<Settlement>,
    stop: CancellationToken,
    wake: Arc<Notify>,
) {
    // Live edges of a prior attachment do not carry over.
    request(&entry);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            s = settlements.recv() => {
                let Some(s) = s else { return };
                entry.state.lock().unwrap().driver.pending.push(s);
                request(&entry);
            }
            event = log.recv() => match event {
                Ok(event) => {
                    settle(&entry, event.seq).await;
                    on_event(&entry, &service, &event).await;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            s = status.recv() => {
                let Some(s) = s else { return };
                // Events appended before the status edge come first.
                while let Ok(event) = log.try_recv() {
                    settle(&entry, event.seq).await;
                    on_event(&entry, &service, &event).await;
                }
                on_status(&entry, &service, &s).await;
            }
            _ = wake.notified() => {}
        }
        loop {
            let requested = {
                let mut st = entry.state.lock().unwrap();
                std::mem::take(&mut st.driver.requested) && !st.driver.stopping
            };
            if !requested || stop.is_cancelled() {
                break;
            }
            let Some(service) = service.upgrade() else {
                return;
            };
            if let Err(e) = drive(&service, &entry).await {
                tracing::warn!(
                    "orchestration driver failed for agent \"{}\": {e}",
                    entry.session_id()
                );
                service.set_activation(&entry, Activation::Disarmed);
            }
        }
    }
}

fn disarm(service: &Weak<OrchestratorService>, entry: &Entry) {
    if let Some(service) = service.upgrade() {
        service.set_activation(entry, Activation::Disarmed);
    }
}

async fn on_event(entry: &Arc<Entry>, service: &Weak<OrchestratorService>, event: &SessionEvent) {
    match event.kind.as_str() {
        kinds::INBOX_SPLICED => {
            let next_turn = event.data["target"] == "next-turn";
            let mut st = entry.state.lock().unwrap();
            let d = &mut st.driver;
            if next_turn {
                for m in event.data["inserted"].as_array().into_iter().flatten() {
                    let Ok(message) = serde_json::from_value::<Message>(m.clone()) else {
                        continue;
                    };
                    if d.attempt.as_ref().is_some_and(|a| same_queued(&message, a)) {
                        continue;
                    }
                    d.competing_queued = true;
                    if let Some(a) = d.attempt.as_mut().filter(|a| a.phase == Phase2::Queued) {
                        a.stale = true;
                    }
                }
            }
            // A discarded inbox item (`outcome: canceled`) that was ours.
            if event.data["outcome"] == "canceled"
                && let Some(a) = d.attempt.as_mut()
                && a.phase == Phase2::Queued
                && !entry.agent.inbox_contains(&a.message_id)
            {
                a.cancelled = true;
            }
        }
        kinds::USER_MESSAGE => {
            let mut st = entry.state.lock().unwrap();
            if let Some(a) = st.driver.attempt.as_mut()
                && event.data["id"].as_str() == Some(a.message_id.as_str())
            {
                a.phase = Phase2::Admitted;
            }
        }
        kinds::TURN_END => {
            let reason = event.data["reason"]["kind"].as_str().unwrap_or_default();
            if reason == "max-tokens" {
                disarm(service, entry);
                return;
            }
            if reason != "aborted" {
                return;
            }
            let claimed = {
                let mut st = entry.state.lock().unwrap();
                match st.driver.attempt.as_mut() {
                    Some(a) if matches!(a.phase, Phase2::Claimed | Phase2::Admitted) => {
                        a.cancelled = true;
                        true
                    }
                    _ => false,
                }
            };
            if !claimed {
                disarm(service, entry);
            }
        }
        _ => {}
    }
}

async fn on_status(entry: &Arc<Entry>, service: &Weak<OrchestratorService>, status: &AgentStatus) {
    match status {
        AgentStatus::Error { .. } => disarm(service, entry),
        AgentStatus::Idle => {
            let attempt = {
                let mut st = entry.state.lock().unwrap();
                st.driver.competing_queued = false;
                st.driver.attempt.clone()
            };
            if let Some(a) = attempt
                && (matches!(a.phase, Phase2::Queued | Phase2::Claimed) || a.cancelled)
                && let Some(service) = service.upgrade()
            {
                let run = entry.view().ok().flatten();
                if let Some(v) = run.filter(|v| {
                    v.state.run.phase == Phase::Active
                        && v.activation == Activation::Armed
                        && v.state.run.id == a.run_id
                        && v.state.run.revision == a.revision
                }) {
                    entry.state.lock().unwrap().driver.attempt = None;
                    let session = entry.session_id();
                    if let Err(e) = service
                        .pause(&session, &v.reference(), Initiator::Host)
                        .await
                    {
                        tracing::warn!(
                            "orchestration driver: could not pause cancelled run for agent \"{session}\": {e}"
                        );
                        service.set_activation(entry, Activation::Disarmed);
                    }
                }
            }
            request(entry);
        }
        _ => {}
    }
}

/// Quiescent, no competing prompt.
fn ready(entry: &Entry) -> bool {
    let st = entry.state.lock().unwrap();
    !st.driver.stopping
        && !st.driver.competing_queued
        && matches!(entry.agent.status(), AgentStatus::Idle)
        && entry.agent.inbox_is_empty()
}

/// One drive pass: flush settlements, then dispatch, then at most one round.
async fn drive(service: &Arc<OrchestratorService>, entry: &Arc<Entry>) -> Result<(), String> {
    if !ready(entry) {
        return Ok(());
    }
    let checkpoint = std::mem::take(&mut entry.state.lock().unwrap().driver.needs_checkpoint);
    if checkpoint {
        if let Err(e) = entry.agent.flush().await {
            tracing::warn!(
                "orchestration driver: durability checkpoint failed for agent \"{}\": {e:#}",
                entry.session_id()
            );
            service.set_activation(entry, Activation::Disarmed);
            return Ok(());
        }
        if !ready(entry) || entry.state.lock().unwrap().driver.needs_checkpoint {
            return Ok(());
        }
    }
    {
        let mut st = entry.state.lock().unwrap();
        if st.driver.attempt.is_some() {
            st.driver.attempt = None;
            st.driver.needs_checkpoint = true;
            st.driver.requested = true;
            return Ok(());
        }
    }
    flush_settlements(service, entry).await;
    let Some(view) = entry.view().map_err(|e| e.message)? else {
        return Ok(());
    };
    if view.state.run.phase != Phase::Active || view.activation != Activation::Armed {
        return Ok(());
    }
    dispatch(service, entry, &view).await;
    // Dispatch may have recorded events; re-read before deciding on a round.
    let Some(view) = entry.view().map_err(|e| e.message)? else {
        return Ok(());
    };
    if view.state.run.phase != Phase::Active || view.activation != Activation::Armed {
        return Ok(());
    }
    let in_flight = entry.state.lock().unwrap().driver.workers.len();
    if in_flight > 0 {
        return Ok(()); // wait for settlements
    }
    let session = entry.session_id();
    if view.state.rounds_started >= view.state.run.limits.max_rounds {
        let message = format!(
            "Run reached its configured limit of {} rounds.",
            view.state.run.limits.max_rounds
        );
        service
            .block(
                &session,
                &view.reference(),
                "round-limit",
                &message,
                Initiator::Host,
            )
            .await
            .map_err(|e| e.message)?;
        return Ok(());
    }
    queue_round(service, entry, &view).await;
    Ok(())
}

/// Records every buffered settlement (idle only, so revisions stay stable
/// while the planner works).
async fn flush_settlements(service: &Arc<OrchestratorService>, entry: &Arc<Entry>) {
    let pending: Vec<Settlement> = std::mem::take(&mut entry.state.lock().unwrap().driver.pending);
    for s in pending {
        if !entry
            .view()
            .ok()
            .flatten()
            .is_some_and(|v| v.state.run.id == s.run_id)
        {
            continue; // a prior run's worker cannot settle into a replacement run
        }
        entry
            .state
            .lock()
            .unwrap()
            .driver
            .workers
            .remove(&s.task_id);
        if let Err(e) = service.record_task(entry, s.update, Initiator::Host).await {
            tracing::warn!(
                "orchestration driver: recording a settlement of \"{}\" failed: {e}",
                entry.session_id()
            );
        }
    }
}

/// Dispatches ready tasks up to the parallel and worker budgets.
async fn dispatch(service: &Arc<OrchestratorService>, entry: &Arc<Entry>, view: &RunView) {
    let ready = view.state.ready_tasks();
    if ready.is_empty() {
        return;
    }
    let limits = view.state.run.limits;
    let in_flight = entry.state.lock().unwrap().driver.workers.len() as u64;
    let remaining = limits
        .max_workers
        .saturating_sub(view.state.workers_dispatched);
    let slots = limits.max_parallel.saturating_sub(in_flight).min(remaining) as usize;
    for task_id in ready.into_iter().take(slots) {
        if let Err(e) = dispatch_one(service, entry, view, &task_id).await {
            tracing::warn!(
                "orchestration driver: dispatching task \"{task_id}\" of \"{}\" failed: {e}",
                entry.session_id()
            );
        }
    }
}

async fn dispatch_one(
    service: &Arc<OrchestratorService>,
    entry: &Arc<Entry>,
    view: &RunView,
    task_id: &str,
) -> Result<(), String> {
    // A host pause/clear must serialize with both spawning and recording a
    // worker, so cancellation always sees every child it has to stop.
    let _ops = entry.ops.lock().await;
    let Some(current) = entry.view().map_err(|e| e.message)? else {
        return Ok(());
    };
    if current.state.run.id != view.state.run.id
        || current.state.plan_version != view.state.plan_version
        || current.state.run.phase != Phase::Active
        || current.activation != Activation::Armed
    {
        return Ok(());
    }
    let token = CancellationToken::new();
    if !view.state.tasks.contains_key(task_id) {
        return Err(format!("task \"{task_id}\" vanished from the plan"));
    }
    match service
        .spawn_worker(entry, &view.state, task_id, token.clone())
        .await
    {
        Ok(one_shot) => {
            let update = crate::fold::TaskUpdate {
                id: task_id.to_owned(),
                status: TaskStatus::Running,
                child_id: Some(one_shot.child_id.clone()),
                summary: None,
                stop_reason: None,
            };
            if let Err(e) = service
                .record_task_locked(entry, update, Initiator::Host)
                .await
            {
                token.cancel(); // the child runs unmanaged; stop it
                return Err(format!(
                    "recording the dispatch of \"{task_id}\" failed: {e}"
                ));
            }
            entry
                .state
                .lock()
                .unwrap()
                .driver
                .workers
                .insert(task_id.to_owned(), token);
            let tx = entry.settlements_tx.clone();
            let task_id = task_id.to_owned();
            let run_id = view.state.run.id.clone();
            let child_id = one_shot.child_id.clone();
            let result = one_shot.result;
            tokio::spawn(async move {
                let result = result.await;
                let update = crate::service::settlement_update(&task_id, &child_id, &result);
                let _ = tx.send(Settlement {
                    run_id,
                    task_id,
                    update,
                });
            });
            Ok(())
        }
        Err(e) => {
            // A refused spawn never dispatched; record the failure from
            // `pending` so the planner can replan.
            let update = crate::fold::TaskUpdate {
                id: task_id.to_owned(),
                status: TaskStatus::Failed,
                child_id: None,
                summary: Some(format!("worker was not started: {}", e.message)),
                stop_reason: Some("refused".into()),
            };
            service
                .record_task_locked(entry, update, Initiator::Host)
                .await
                .map(|_| ())
                .map_err(|e| e.message)
        }
    }
}

async fn queue_round(service: &Arc<OrchestratorService>, entry: &Arc<Entry>, view: &RunView) {
    let round = view.state.rounds_started + 1;
    let message = round_message(view, round);
    entry.state.lock().unwrap().driver.attempt = Some(Attempt {
        run_id: view.state.run.id.clone(),
        revision: view.state.run.revision,
        round,
        message_id: message.id.clone(),
        content: message.content.clone(),
        phase: Phase2::Queued,
        cancelled: false,
        stale: false,
    });
    if let Err(e) = entry.agent.followup(message).await {
        entry.state.lock().unwrap().driver.attempt = None;
        tracing::warn!(
            "orchestration driver: could not queue round {round} for agent \"{}\": {e:#}",
            entry.session_id()
        );
        let latest = entry.view().ok().flatten();
        if let Some(l) = latest.filter(|l| {
            l.state.run.id == view.state.run.id
                && l.state.run.revision == view.state.run.revision
                && l.state.run.phase == Phase::Active
                && l.activation == Activation::Armed
        }) {
            let message = format!("Could not queue orchestration round {round}: {e:#}");
            let _ = service
                .block(
                    &entry.session_id(),
                    &l.reference(),
                    "queue-failed",
                    &message,
                    Initiator::Host,
                )
                .await;
        }
    }
}

/// The pre-step check of a submitted round: only the exact reserved round of
/// the live armed revision enters, and its step starts a request series.
pub(crate) struct RoundHook {
    pub entry: Weak<Entry>,
    pub service: Weak<OrchestratorService>,
}

impl RoundHook {
    fn valid(&self, entry: &Entry, message: &Message, round: (String, u64, u64)) -> bool {
        let run = entry.view().ok().flatten();
        let st = entry.state.lock().unwrap();
        let Some(a) = st.driver.attempt.as_ref() else {
            return false;
        };
        !st.driver.stopping
            && a.phase == Phase2::Claimed
            && !a.stale
            && same_queued(message, a)
            && run.is_some_and(|v| {
                v.state.run.id == round.0
                    && v.state.run.revision == round.1
                    && v.state.run.phase == Phase::Active
                    && v.activation == Activation::Armed
                    && round.2 == v.state.rounds_started + 1
            })
    }

    /// Canonical run state after compaction is model context, never a new
    /// admitted round. The durable fold remains the budget authority.
    fn restore_context(&self, step: &PreStep<'_>, entering: &mut Vec<Message>) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let Ok(mut view) = entry.view() else {
            return;
        };
        if let Some(view) = &mut view
            && let Some((id, revision, round)) =
                step.claimed.iter().find_map(|m| source_round(&m.source))
            && view.state.run.id == id
            && view.state.run.revision == revision
        {
            // Admission was validated above; the fold commits this round
            // and this snapshot in the same batch before the model request.
            view.state.rounds_started = round;
        }
        let snapshot = json!({"orchestration": view.map(|v| v.to_json())});
        let needed = step.read_history(|events, visible| {
            if !events.iter().any(|e| {
                e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "compact-checkpoint"
            }) {
                return false;
            }
            let previous = events.iter().rev().find(|e| {
                e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "orchestration-state"
            });
            if snapshot["orchestration"].is_null() && previous.is_none() {
                return false;
            }
            !previous.is_some_and(|e| visible(e.seq) && e.data["source"]["snapshot"] == snapshot)
        });
        if needed {
            let mut message = Message::user_text(format!(
                "<orchestration_state>\nCurrent orchestration run from the session journal (authoritative over earlier checkpoints):\n{snapshot}\n</orchestration_state>"
            ));
            message.source =
                MessageSource::Other(json!({"kind": "orchestration-state", "snapshot": snapshot}));
            entering.push(message);
        }
    }
}

#[async_trait]
impl PreStepHook for RoundHook {
    fn compact_short_turns(&self) -> bool {
        self.entry
            .upgrade()
            .and_then(|entry| entry.view().ok().flatten())
            .is_some_and(|v| v.state.run.phase == Phase::Active)
    }

    fn cancel_between_turns(&self) {
        if let (Some(entry), Some(service)) = (self.entry.upgrade(), self.service.upgrade()) {
            service.set_activation(&entry, Activation::Disarmed);
        }
    }

    async fn pre_step(
        &self,
        step: &PreStep<'_>,
        entering: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        let Some((submitted, round)) = step
            .claimed
            .iter()
            .find_map(|m| source_round(&m.source).map(|r| (m, r)))
        else {
            self.restore_context(step, entering);
            return Ok(());
        };
        let Some(entry) = self.entry.upgrade() else {
            step.reject(restore_other(step.claimed, &submitted.id));
            return Ok(());
        };
        {
            // `agent/inbox/claimed` precedes the step's listeners.
            let mut st = entry.state.lock().unwrap();
            if let Some(a) = st.driver.attempt.as_mut()
                && same_queued(submitted, a)
            {
                a.phase = Phase2::Claimed;
            }
        }
        if self.valid(&entry, submitted, round.clone()) {
            step.start_request_series();
            self.restore_context(step, entering);
            return Ok(());
        }
        {
            let mut st = entry.state.lock().unwrap();
            let d = &mut st.driver;
            if d.attempt
                .as_ref()
                .is_some_and(|a| a.run_id == round.0 && a.revision == round.1 && a.round == round.2)
            {
                d.attempt = None;
            }
        }
        step.reject(restore_other(step.claimed, &submitted.id));
        request(&entry);
        Ok(())
    }
}

/// Claimed input other than the dropped round goes back to the next-step
/// inbox.
fn restore_other(claimed: &[Message], dropped: &str) -> Vec<Message> {
    claimed
        .iter()
        .filter(|m| m.id != dropped)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fold::{Criterion, Limits, Run, RunState, safe_integer};
    use crate::service::RunView;
    use serde_json::json;

    fn view(rounds: u64) -> RunView {
        RunView {
            state: RunState {
                run: Run {
                    id: "orch-1".into(),
                    revision: 3,
                    objective: "Report".into(),
                    criteria: vec![Criterion {
                        id: "c1".into(),
                        criterion: "exists".into(),
                    }],
                    limits: Limits {
                        max_rounds: 4,
                        max_parallel: 3,
                        max_workers: 12,
                    },
                    phase: Phase::Active,
                    blocked_reason: None,
                },
                plan_version: 0,
                tasks: Default::default(),
                seen_tasks: Default::default(),
                plan_order: vec![],
                task_seq: vec![],
                workers_dispatched: 2,
                checks: None,
                rounds_started: rounds,
                created_at: 1,
                updated_at: 2,
            },
            activation: Activation::Armed,
        }
    }

    #[test]
    fn round_prompt_carries_board_and_budgets() {
        let prompt = round_prompt(&view(1));
        assert!(prompt.contains("Round: 2/4"));
        assert!(prompt.contains("workers used 2/12"));
        assert!(prompt.contains("No plan yet"));
    }

    #[test]
    fn round_message_source_is_reserved() {
        let m = round_message(&view(0), 1);
        let MessageSource::Other(v) = &m.source else {
            panic!("not an other source");
        };
        assert_eq!(
            v["kind"],
            json!("orchestration"),
            "round sources are reserved messages"
        );
        assert!(safe_integer(&v["round"]) == Some(1));
    }
}
