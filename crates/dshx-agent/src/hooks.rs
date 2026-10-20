//! Pre-step hooks: products extend each step's entering batch the way dsh
//! plugins do with `agent/pre-step` listeners registered after workspace
//! instructions and the runtime context (e.g. `tool-skill`'s session skill
//! catalog and `/name` invocations). Hooks run in registration order, after
//! the runtime-context snapshot was appended; each sees the batch the
//! previous one left.
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dshx_core::{Message, SessionEvent};
use tokio_util::sync::CancellationToken;

use crate::agent::Inner;
use crate::journal::Journal;

/// What a [`PreStepHook`] sees of the step being prepared.
pub struct PreStep<'a> {
    /// The agent's workspace.
    pub cwd: &'a Path,
    /// Input claimed for this step (user prompts, notices), in order.
    pub claimed: &'a [Message],
    /// Names of the tools offered to the model, sorted.
    pub tools: &'a [String],
    pub cancel: &'a CancellationToken,
    journal: &'a Journal,
}

impl PreStep<'_> {
    /// Runs `f` over the session log and a test whether an event is still on
    /// the model-visible surface (not compacted away or replaced). The log
    /// is locked meanwhile, so keep `f` cheap and free of I/O.
    pub fn read_history<R>(&self, f: impl FnOnce(&[SessionEvent], &dyn Fn(u64) -> bool) -> R) -> R {
        self.journal
            .read(|events, surface| f(events, &|seq| surface.nodes.contains(&seq)))
    }
}

/// Extends or rewrites the messages entering a step. Messages added here
/// are logged as `user/message` events like the agent's own context
/// messages, so they stay reconstructable from the session log.
#[async_trait]
pub trait PreStepHook: Send + Sync {
    /// Edits `entering` in place. An error is logged and leaves the batch as
    /// the hook left it, so change it only once the work succeeded.
    async fn pre_step(&self, step: &PreStep<'_>, entering: &mut Vec<Message>)
    -> anyhow::Result<()>;
}

/// Runs every registered hook over `entering`.
pub(crate) async fn run(
    inner: &Arc<Inner>,
    claimed: &[Message],
    entering: &mut Vec<Message>,
    cancel: &CancellationToken,
) {
    let hooks = inner.pre_step_hooks.lock().unwrap().clone();
    if hooks.is_empty() {
        return;
    }
    let cwd = inner.config.lock().unwrap().cwd.clone();
    let tools: Vec<String> = inner.tool_schemas().into_iter().map(|t| t.name).collect();
    let step = PreStep {
        cwd: &cwd,
        claimed,
        tools: &tools,
        cancel,
        journal: &inner.journal,
    };
    for hook in hooks {
        if cancel.is_cancelled() {
            return;
        }
        if let Err(e) = hook.pre_step(&step, entering).await {
            tracing::warn!("pre-step hook failed: {e:#}");
        }
    }
}
