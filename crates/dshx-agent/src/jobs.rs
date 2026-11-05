//! Background-job integration with `dshx-tools` (dsh `jobs` + `tool-jobs`):
//! `job/registered`/`job/settled` events in the session log and completion
//! notices for the owning agent (injected while it runs, a wake-up turn
//! while it is idle).
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

use dshx_core::{ContentBlock, Message, MessageSource, Role};
use dshx_tools::{JobEvent, ToolState};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, Inner, Target};

type Feed = Arc<tokio::sync::Mutex<Option<UnboundedReceiver<JobEvent>>>>;

/// Job events forwarded from the registry, waiting for the loop to log them.
#[derive(Default)]
pub(crate) struct JobQueue {
    events: Mutex<VecDeque<JobEvent>>,
    /// The registry's feed. The forwarder holds it while it runs; [`finish`]
    /// takes what is left once the forwarder stopped.
    feed: Feed,
    /// Stops the forwarder; only [`finish`] cancels it, after the session's
    /// jobs were torn down, so their settlements still reach the log.
    stop: CancellationToken,
}

/// Starts the forwarder for this agent's session (no-op outside a runtime).
pub(crate) fn start(inner: &Arc<Inner>, state: &Arc<ToolState>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Ok(mut slot) = inner.jobs.feed.try_lock() else {
        return;
    };
    *slot = Some(state.jobs().subscribe());
    drop(slot);
    let feed = inner.jobs.feed.clone();
    let stop = inner.jobs.stop.clone();
    let weak: Weak<Inner> = Arc::downgrade(inner);
    handle.spawn(async move {
        let mut feed = feed.lock().await;
        loop {
            let Some(rx) = feed.as_mut() else { return };
            let event = tokio::select! {
                biased;
                _ = stop.cancelled() => None,
                e = rx.recv() => e,
            };
            let Some(event) = event else { return };
            let Some(inner) = weak.upgrade() else { return };
            inner.jobs.events.lock().unwrap().push_back(event);
            let idle = inner.state.lock().unwrap().running.is_none();
            if idle {
                // Nobody drains while idle: log now and wake for notices,
                // unless the agent is being disposed.
                let messages = drain(&inner).await;
                if !inner.lifetime.is_cancelled() {
                    wake(&inner, messages);
                }
            }
        }
    });
}

/// Disposal, after the session's jobs were torn down: stops the forwarder
/// and logs every event still queued or in the feed (the teardown
/// settlements among them). Delivers no notices: the session is closing.
pub(crate) async fn finish(inner: &Arc<Inner>) {
    inner.jobs.stop.cancel();
    // Waits for the forwarder to finish the event it is logging.
    if let Some(mut rx) = inner.jobs.feed.lock().await.take() {
        let mut queue = inner.jobs.events.lock().unwrap();
        while let Ok(event) = rx.try_recv() {
            queue.push_back(event);
        }
    }
    let session = inner.journal.id();
    let events: Vec<JobEvent> = inner.jobs.events.lock().unwrap().drain(..).collect();
    for event in events.iter().filter(|e| owned_by(e, &session)) {
        if let Some(e) = event.session_event()
            && let Err(err) = inner.append(e).await
        {
            tracing::warn!("job event not logged: {err:#}");
        }
    }
}

fn owned_by(event: &JobEvent, session: &str) -> bool {
    let owner = match event {
        JobEvent::Registered { job }
        | JobEvent::Settled { job, .. }
        | JobEvent::Removed { job } => job.owner.as_deref(),
    };
    owner == Some(session)
}

/// Opens one turn per notice (dsh `followup` on an idle owner).
pub(crate) fn wake(inner: &Arc<Inner>, messages: Vec<Message>) {
    for message in messages {
        let agent = Agent {
            inner: inner.clone(),
        };
        let turn: futures::future::BoxFuture<'static, ()> = Box::pin(async move {
            if let Err(e) = agent.run_message(message).await {
                tracing::warn!("job completion turn failed: {e:#}");
            }
        });
        tokio::spawn(turn);
    }
}

fn notice_message(text: String, summary: String) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(text)],
        source: MessageSource::Other(
            json!({"kind": "tool-jobs", "form": "notice", "summary": summary}),
        ),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// Logs queued job events of this session. Completion notices are injected
/// into the next step while a turn runs; otherwise they are returned so the
/// caller can open a wake-up turn.
pub(crate) async fn drain(inner: &Arc<Inner>) -> Vec<Message> {
    tokio::task::yield_now().await;
    let session = inner.journal.id();
    let events: Vec<JobEvent> = inner.jobs.events.lock().unwrap().drain(..).collect();
    let mut wake = Vec::new();
    for event in events {
        if !owned_by(&event, &session) {
            continue;
        }
        if let Some(e) = event.session_event()
            && let Err(err) = inner.append(e).await
        {
            tracing::warn!("job event not logged: {err:#}");
        }
        if let Some(notice) = event.completion_notice() {
            let message = notice_message(notice.text, notice.summary);
            if inner.state.lock().unwrap().running.is_some() {
                let len = inner.state.lock().unwrap().next_step.len();
                if let Err(err) = inner
                    .splice(Target::NextStep, len, 0, vec![message], false)
                    .await
                {
                    tracing::warn!("job notice not queued: {err:#}");
                }
            } else {
                wake.push(message);
            }
        }
    }
    wake
}
