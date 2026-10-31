//! Pre-step hooks: products extend each step's entering batch the way dsh
//! plugins do with `agent/pre-step` listeners registered after workspace
//! instructions and the runtime context (e.g. `tool-skill`'s session skill
//! catalog and `/name` invocations). Hooks run in registration order, after
//! the runtime-context snapshot was appended; each sees the batch the
//! previous one left. A hook may reject the step (the turn ends `blocked`,
//! later hooks do not run) or mark it as the start of a request series.
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{Message, SessionEvent, ToolRegistry};
use tokio_util::sync::CancellationToken;

use crate::agent::Inner;
use crate::config::{AgentConfig, ExtraSection, ToolUpdate};
use crate::journal::Journal;

/// What a [`PreStepHook`] sees of the step being prepared.
pub struct PreStep<'a> {
    /// The agent's workspace.
    pub cwd: &'a Path,
    /// Input claimed for this step (user prompts, notices), in order.
    pub claimed: &'a [Message],
    /// Names of available capabilities when the hooks started, sorted. In PTC
    /// these are program bindings; the wire schema is the `run_code` transport.
    pub tools: &'a [String],
    /// How the routed model accepts tool-set changes mid-conversation.
    pub tool_update: Option<ToolUpdate>,
    pub cancel: &'a CancellationToken,
    /// The turn and step being prepared.
    pub turn: u64,
    pub step: u64,
    journal: &'a Journal,
    registry: &'a Mutex<ToolRegistry>,
    config: &'a Mutex<AgentConfig>,
    decision: Mutex<Decision>,
}

/// What the hooks decided beyond the entering batch.
#[derive(Debug, Default)]
pub(crate) struct Decision {
    pub reject: Option<Vec<Message>>,
    pub starts_series: bool,
}

impl PreStep<'_> {
    /// Runs `f` over the session log and a test whether an event is still on
    /// the model-visible surface (not compacted away or replaced). The log
    /// is locked meanwhile, so keep `f` cheap and free of I/O.
    pub fn read_history<R>(&self, f: impl FnOnce(&[SessionEvent], &dyn Fn(u64) -> bool) -> R) -> R {
        self.journal
            .read(|events, surface| f(events, &|seq| surface.nodes.contains(&seq)))
    }

    /// Changes the agent's tool set before this step's request is built
    /// (e.g. tools a mention in the claimed input switched on); the request
    /// header records the change like [`crate::Agent::set_tools`] does.
    pub fn update_tools(&self, f: impl FnOnce(&mut ToolRegistry)) {
        f(&mut self.registry.lock().unwrap());
    }

    /// The agent's current tool set (including earlier hooks' changes).
    pub fn current_tools(&self) -> ToolRegistry {
        self.registry.lock().unwrap().clone()
    }

    /// Current product prompt sections, including earlier hooks' changes.
    pub fn prompt_sections(&self) -> Vec<ExtraSection> {
        self.config.lock().unwrap().prompt.extra_sections.clone()
    }

    /// Refreshes product guidance before this step's prompt is rendered.
    pub fn update_prompt_sections(&self, f: impl FnOnce(&mut Vec<ExtraSection>)) {
        f(&mut self.config.lock().unwrap().prompt.extra_sections);
    }

    /// Rejects the step (dsh pre-step `{kind: "reject"}`): nothing enters,
    /// the turn ends `blocked`, and `restore` goes back to the front of the
    /// next-step inbox (claimed input that must not be lost).
    pub fn reject(&self, restore: Vec<Message>) {
        self.decision.lock().unwrap().reject = Some(restore);
    }

    /// Whether an earlier hook rejected the step.
    pub fn rejected(&self) -> bool {
        self.decision.lock().unwrap().reject.is_some()
    }

    /// The step's request starts a new request series (dsh
    /// `startsRequestSeries`, e.g. an automatic goal round).
    pub fn start_request_series(&self) {
        self.decision.lock().unwrap().starts_series = true;
    }
}

/// Extends or rewrites the messages entering a step. Messages added here
/// are logged as `user/message` events like the agent's own context
/// messages, so they stay reconstructable from the session log.
#[async_trait]
pub trait PreStepHook: Send + Sync {
    /// Whether this product benefits from the lower size floor for compaction
    /// after a completed turn. This never enables compaction between tool calls.
    fn compact_short_turns(&self) -> bool {
        false
    }

    /// A stop arrived outside an executing turn body, including terminal
    /// commits, compaction, flushing and idle gaps. Products with automatic
    /// continuation must disarm it here: an aborted turn/end is not assured.
    fn cancel_between_turns(&self) {}

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
    turn: u64,
    step: u64,
    cancel: &CancellationToken,
) -> Decision {
    let hooks = inner.pre_step_hooks.lock().unwrap().clone();
    if hooks.is_empty() {
        return Decision::default();
    }
    let (cwd, tool_update) = {
        let c = inner.config.lock().unwrap();
        (c.cwd.clone(), c.route().tool_update)
    };
    // Hooks compose capabilities, even when PTC exposes only the transport to the model.
    let tools: Vec<String> = inner
        .capability_schemas()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let step = PreStep {
        cwd: &cwd,
        claimed,
        tools: &tools,
        tool_update,
        cancel,
        turn,
        step,
        journal: &inner.journal,
        registry: &inner.tools,
        config: &inner.config,
        decision: Mutex::new(Decision::default()),
    };
    for hook in hooks {
        if cancel.is_cancelled() || step.rejected() {
            break;
        }
        if let Err(e) = hook.pre_step(&step, entering).await {
            tracing::warn!("pre-step hook failed: {e:#}");
        }
    }
    step.decision.into_inner().unwrap()
}
