//! Background-job integration with `dshx-tools` (dsh `jobs` + `tool-jobs`):
//! `job/registered`/`job/settled` events in the session log and completion
//! notices for the owning agent (injected while it runs, a wake-up turn
//! while it is idle).
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

use dshx_core::{ContentBlock, Message, MessageSource, Role};
use dshx_tools::{JobEvent, ToolState};
use serde_json::json;

use crate::agent::{Agent, Inner, Target};

/// Job events forwarded from the registry, waiting for the loop to log them.
#[derive(Default)]
pub(crate) struct JobQueue {
    events: Mutex<VecDeque<JobEvent>>,
}

/// Starts the forwarder for this agent's session (no-op outside a runtime).
pub(crate) fn start(inner: &Arc<Inner>, state: &Arc<ToolState>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut rx = state.jobs().subscribe();
    let weak: Weak<Inner> = Arc::downgrade(inner);
    let lifetime = inner.lifetime.clone();
    handle.spawn(async move {
        loop {
            let event = tokio::select! {
                e = rx.recv() => e,
                _ = lifetime.cancelled() => None,
            };
            let Some(event) = event else { return };
            let Some(inner) = weak.upgrade() else { return };
            inner.jobs.events.lock().unwrap().push_back(event);
            let idle = inner.state.lock().unwrap().running.is_none();
            if idle {
                // Nobody drains while idle: log now and wake for notices.
                let messages = drain(&inner).await;
                wake(&inner, messages);
            }
        }
    });
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
        let owner = match &event {
            JobEvent::Registered { job }
            | JobEvent::Settled { job, .. }
            | JobEvent::Removed { job } => job.owner.clone(),
        };
        if owner.as_deref() != Some(session.as_str()) {
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
