//! Automatic same-session continuation (dsh `dsh-goal-round-driver`): when
//! the agent is idle and its goal is active and armed, one `<goal_round>`
//! prompt is queued; a pre-step check admits only the exact reserved round
//! and starts a request series for it. A user abort of a round pauses the
//! goal, `max-tokens` and agent errors disarm it, the round cap blocks it.
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use dshx_agent::{AgentObserver, AgentStatus, CancelCause, PreStep, PreStepHook};
use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent, kinds};
use serde_json::json;
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::fold::Phase;
use crate::service::{Activation, Entry, GoalService, GoalView, Initiator};

/// dsh `renderGoalRoundPrompt`.
pub fn round_prompt(objective: &str, round: u64, max_goal_rounds: u64) -> String {
    format!(
        "<goal_round>\nObjective: {}\nRound: {round}/{max_goal_rounds}\n\nContinue working toward the objective in this same session. Treat the current workspace, tool results, and durable session state as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before claiming completion, gather evidence that the whole objective is achieved, read the current goal, and mark it complete. If work remains, leave the goal active for the next round. Follow the configured goal-tool policy before reporting a blocker.\n</goal_round>",
        serde_json::to_string(objective).unwrap_or_default()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase2 {
    Queued,
    Claimed,
    Admitted,
}

/// The one reserved round (dsh `attempt`).
#[derive(Debug, Clone)]
struct Attempt {
    goal_id: String,
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

fn round_message(goal: &GoalView, round: u64) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(round_prompt(
            &goal.goal.objective,
            round,
            goal.goal.max_goal_rounds,
        ))],
        source: MessageSource::Other(json!({
            "kind": "goal",
            "goalId": goal.goal.id,
            "revision": goal.goal.revision,
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
        id == attempt.goal_id && rev == attempt.revision && round == attempt.round
    }) && message.content == attempt.content
}

/// Live status edges of the agent, for the driver task.
struct StatusRelay(mpsc::UnboundedSender<AgentStatus>);

impl AgentObserver for StatusRelay {
    fn on_status(&self, status: &AgentStatus) {
        let _ = self.0.send(status.clone());
    }
}

pub(crate) fn start(service: Weak<GoalService>, entry: Arc<Entry>) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    entry.agent.add_observer(Arc::new(StatusRelay(tx)));
    let log = entry.agent.subscribe();
    let stop = CancellationToken::new();
    let wake = Arc::new(Notify::new());
    let task = tokio::spawn(run(service, entry, log, rx, stop.clone(), wake.clone()));
    Handle { stop, wake, task }
}

/// Teardown: disarm, stop a running round (cause `parent`), end the task.
pub(crate) async fn stop(entry: &Arc<Entry>) {
    let handle = entry.driver.lock().unwrap().take();
    let running = {
        let mut st = entry.state.lock().unwrap();
        st.driver.stopping = true;
        st.activation = Activation::Disarmed;
        match st.driver.attempt.as_mut() {
            Some(a) => {
                a.stale = true;
                true
            }
            None => false,
        }
    };
    if running && entry.agent.status() == AgentStatus::Running {
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

/// dsh `goal/changed`: checkpoint before the next round; a pause from
/// outside the agent's own tool call stops its running turn.
pub(crate) fn goal_changed(entry: &Arc<Entry>, operation: &str, initiator: Initiator) {
    entry.state.lock().unwrap().driver.needs_checkpoint = true;
    if operation == "pause"
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

async fn run(
    service: Weak<GoalService>,
    entry: Arc<Entry>,
    mut log: broadcast::Receiver<SessionEvent>,
    mut status: mpsc::UnboundedReceiver<AgentStatus>,
    stop: CancellationToken,
    wake: Arc<Notify>,
) {
    // Live edges of a prior attachment do not carry over (dsh `agent/created`).
    request(&entry);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return,
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
                    "goal-round-driver: driver failed for agent \"{}\": {e}",
                    entry.agent.session_id()
                );
                service.set_activation(&entry, Activation::Disarmed);
            }
        }
    }
}

fn disarm(service: &Weak<GoalService>, entry: &Entry) {
    if let Some(service) = service.upgrade() {
        service.set_activation(entry, Activation::Disarmed);
    }
}

async fn on_event(entry: &Arc<Entry>, service: &Weak<GoalService>, event: &SessionEvent) {
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

async fn on_status(entry: &Arc<Entry>, service: &Weak<GoalService>, status: &AgentStatus) {
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
                let goal = entry.view().ok().flatten();
                if let Some(g) = goal.filter(|g| {
                    g.goal.phase == Phase::Active
                        && g.activation == Activation::Armed
                        && g.goal.id == a.goal_id
                        && g.goal.revision == a.revision
                }) {
                    entry.state.lock().unwrap().driver.attempt = None;
                    let session = entry.agent.session_id();
                    if let Err(e) = service
                        .pause(&session, &g.reference(), Initiator::Host)
                        .await
                    {
                        tracing::warn!(
                            "goal-round-driver: could not pause cancelled goal for agent \"{session}\": {e}"
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

/// dsh `readyToDrive`: quiescent, no competing prompt.
fn ready(entry: &Entry) -> bool {
    let st = entry.state.lock().unwrap();
    !st.driver.stopping
        && !st.driver.competing_queued
        && matches!(entry.agent.status(), AgentStatus::Idle)
        && entry.agent.inbox_is_empty()
}

/// dsh `drive`: admitted work first, then at most one next round.
async fn drive(service: &Arc<GoalService>, entry: &Arc<Entry>) -> Result<(), String> {
    if !ready(entry) {
        return Ok(());
    }
    let checkpoint = std::mem::take(&mut entry.state.lock().unwrap().driver.needs_checkpoint);
    if checkpoint {
        if let Err(e) = entry.agent.flush().await {
            tracing::warn!(
                "goal-round-driver: durability checkpoint failed for agent \"{}\": {e:#}",
                entry.agent.session_id()
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
    let Some(goal) = entry.view().map_err(|e| e.message)? else {
        return Ok(());
    };
    if goal.goal.phase != Phase::Active || goal.activation != Activation::Armed {
        return Ok(());
    }
    let session = entry.agent.session_id();
    if goal.rounds_started >= goal.goal.max_goal_rounds {
        let message = format!(
            "Goal reached its configured limit of {} rounds.",
            goal.goal.max_goal_rounds
        );
        service
            .block(
                &session,
                &goal.reference(),
                "round-limit",
                &message,
                Initiator::Host,
            )
            .await
            .map_err(|e| e.message)?;
        return Ok(());
    }
    let round = goal.rounds_started + 1;
    let message = round_message(&goal, round);
    entry.state.lock().unwrap().driver.attempt = Some(Attempt {
        goal_id: goal.goal.id.clone(),
        revision: goal.goal.revision,
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
            "goal-round-driver: could not queue round {round} for agent \"{session}\": {e:#}"
        );
        let latest = entry.view().ok().flatten();
        if let Some(l) = latest.filter(|l| {
            l.goal.id == goal.goal.id
                && l.goal.revision == goal.goal.revision
                && l.goal.phase == Phase::Active
                && l.activation == Activation::Armed
        }) {
            let message = format!("Could not queue goal round {round}: {e:#}");
            let _ = service
                .block(
                    &session,
                    &l.reference(),
                    "queue-failed",
                    &message,
                    Initiator::Host,
                )
                .await;
        }
    }
    Ok(())
}

/// The pre-step check of a submitted goal round (dsh `agent/pre-step`
/// listener): only the exact reserved round of the live armed revision
/// enters, and its step starts a request series.
pub(crate) struct RoundHook {
    pub entry: Weak<Entry>,
    pub service: Weak<GoalService>,
}

impl RoundHook {
    fn valid(&self, entry: &Entry, message: &Message, round: (String, u64, u64)) -> bool {
        let goal = entry.view().ok().flatten();
        let st = entry.state.lock().unwrap();
        let Some(a) = st.driver.attempt.as_ref() else {
            return false;
        };
        !st.driver.stopping
            && a.phase == Phase2::Claimed
            && !a.stale
            && same_queued(message, a)
            && goal.is_some_and(|g| {
                g.goal.id == round.0
                    && g.goal.revision == round.1
                    && g.goal.phase == Phase::Active
                    && g.activation == Activation::Armed
                    && round.2 == g.rounds_started + 1
            })
    }

    /// Canonical goal state after compaction is model context, never a new
    /// admitted goal round. The durable fold remains the budget authority.
    fn restore_context(&self, step: &PreStep<'_>, entering: &mut Vec<Message>) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let Ok(mut goal) = entry.view() else {
            return;
        };
        if let Some(goal) = &mut goal
            && let Some((id, revision, round)) =
                step.claimed.iter().find_map(|m| source_round(&m.source))
            && goal.goal.id == id
            && goal.goal.revision == revision
        {
            // Admission was validated above; settle_claim commits this round
            // and this snapshot in the same batch before the model request.
            goal.rounds_started = round;
        }
        let snapshot = json!({"goal": goal.map(|goal| goal.to_json())});
        let needed = step.read_history(|events, visible| {
            if !events.iter().any(|e| {
                e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "compact-checkpoint"
            }) {
                return false;
            }
            let previous = events.iter().rev().find(|e| {
                e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "goal-state"
            });
            if snapshot["goal"].is_null() && previous.is_none() {
                return false;
            }
            !previous.is_some_and(|e| visible(e.seq) && e.data["source"]["snapshot"] == snapshot)
        });
        if needed {
            let mut message = Message::user_text(format!(
                "<goal_state>\nCurrent goal state from the session journal (authoritative over earlier checkpoints):\n{snapshot}\n</goal_state>"
            ));
            message.source =
                MessageSource::Other(json!({"kind": "goal-state", "snapshot": snapshot}));
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
            .is_some_and(|goal| goal.goal.phase == Phase::Active)
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
            // dsh `agent/inbox/claimed` precedes the step's listeners.
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
            if d.attempt.as_ref().is_some_and(|a| {
                a.goal_id == round.0 && a.revision == round.1 && a.round == round.2
            }) {
                d.attempt = None;
            }
        }
        step.reject(restore_other(step.claimed, &submitted.id));
        request(&entry);
        Ok(())
    }
}

/// dsh `restoreOtherClaimed`: claimed input other than the dropped round
/// (and round-zero goal records) goes back to the next-step inbox.
fn restore_other(claimed: &[Message], dropped: &str) -> Vec<Message> {
    claimed
        .iter()
        .filter(|m| m.id != dropped)
        .filter(|m| {
            !matches!(&m.source, MessageSource::Other(v) if v["kind"] == "goal" && v["round"] == json!(0))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_prompt_matches_dsh() {
        assert_eq!(
            round_prompt("Fix \"all\" tests", 2, 5),
            "<goal_round>\nObjective: \"Fix \\\"all\\\" tests\"\nRound: 2/5\n\nContinue working toward the objective in this same session. Treat the current workspace, tool results, and durable session state as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before claiming completion, gather evidence that the whole objective is achieved, read the current goal, and mark it complete. If work remains, leave the goal active for the next round. Follow the configured goal-tool policy before reporting a blocker.\n</goal_round>"
        );
    }
}
