//! The in-process delegation service (dsh `@deepseek-ai/dsh-subagent` with
//! the spawn/fork providers and the in-process driver): child composition,
//! one-shot runs, continuable activations with settlement notices, adjacent
//! messaging, interrupts, discovery and teardown.
//!
//! A child is an ordinary [`Agent`] over its own session log in the same
//! store, with `parentSession`, `origin: "subagent"` and `delegationDepth` in
//! its header, the parent's tools, model route and permission scope (approval
//! pinned to `never`), and the `subagent:delegation` runtime-context statement.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dshx_agent::{
    Agent, AgentConfig, AgentDeps, AgentObserver, AgentStatus, ApprovalPolicy, CancelCause,
    ExtraSection, StreamDelta,
};
use dshx_core::{
    Approver, AttachmentStore, ContentBlock, LlmClient, Message, NewEvent, SESSION_FORMAT_VERSION,
    SandboxMode, SessionEvent, SessionHeader, SessionLog, ToolRegistry, kinds, now_ms,
};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::error::SubagentError;
use crate::events::{SubagentFinished, SubagentObserver, SubagentStarted};
use crate::log::DescriptorLog;
use crate::records::{self, CATALOG, CatalogEntry, DELEGATION_CONTEXT, DELEGATION_CONTEXT_NAME};
use crate::sessions::SessionFactory;

/// Fallback poll of idle checks (the agent's driver lock emits no event).
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Collaborators every child agent is built from.
pub struct RuntimeDeps {
    /// The parent's model client; children use the same provider.
    pub llm: Arc<dyn LlmClient>,
    /// The store children are created in (the parent's store).
    pub sessions: Arc<dyn SessionFactory>,
    pub attachments: Arc<dyn AttachmentStore>,
    /// Children run with approval policy `never`, so this is never asked; it
    /// stays wired for tools that consult it directly.
    pub approver: Arc<dyn Approver>,
}

/// Delegation limits (dsh `subagent` host settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSettings {
    /// Deepest allowed child (`1` = direct children only, `0` = no delegation).
    pub max_depth: u32,
    /// Live continuable children per activation tree.
    pub max_active: usize,
}

impl Default for SubagentSettings {
    fn default() -> Self {
        SubagentSettings {
            max_depth: 1,
            max_active: 8,
        }
    }
}

/// What [`SubagentRuntime::start_continuable_child`] establishes (dsh
/// `ContinuableStartSpec`).
#[derive(Debug, Clone)]
pub struct ContinuableSpec {
    /// The child's session id; `None` = a fresh UUID. An id already in use
    /// is refused (`DUPLICATE_CHILD`).
    pub child_id: Option<String>,
    /// `spawn` (fresh child) or `fork` (seeded with the parent's completed turns).
    pub provider: String,
    /// The catalog label (dsh `label`).
    pub label: String,
    /// The initial task. The adjacent-agent return guidance follows it when
    /// the child's `send_message` is the subagent tool.
    pub prompt: Vec<ContentBlock>,
    pub max_depth: Option<u32>,
}

/// Builds a child's tools from its parent's registry (default: the same set,
/// like dsh's preset composition).
pub type ChildToolsHook = Arc<dyn Fn(&ToolRegistry) -> ToolRegistry + Send + Sync>;
/// Live observer for one child, given `(child id, parent id)`.
pub type ChildObserverHook =
    Arc<dyn Fn(&str, &str) -> Option<Arc<dyn AgentObserver>> + Send + Sync>;
/// Called with every new child agent before it runs (e.g. to add pre-step hooks).
pub type ChildAgentHook = Arc<dyn Fn(&Agent) + Send + Sync>;

#[derive(Default, Clone)]
struct Hooks {
    child_tools: Option<ChildToolsHook>,
    child_observer: Option<ChildObserverHook>,
    child_agent: Option<ChildAgentHook>,
    events: Option<Arc<dyn SubagentObserver>>,
}

/// A live agent the runtime can address.
#[derive(Clone)]
pub(crate) struct Live {
    pub agent: Agent,
    pub parent: Option<String>,
    /// Stops a running one-shot child (parent cause).
    cancel: Option<CancellationToken>,
}

impl Live {
    pub(crate) fn id(&self) -> String {
        self.agent.session_id()
    }
}

struct ActState {
    announced: bool,
    closing: bool,
    /// Deliveries spawned by the runtime that have not returned yet.
    in_flight: usize,
    /// Continuable children this activation must outlive.
    owned: HashSet<String>,
}

/// One residency epoch of a continuable child (dsh `Activation`).
struct Activation {
    child_id: String,
    parent_id: String,
    provider: String,
    /// Capacity pool key (the nearest non-resident ancestor).
    pool: String,
    agent: Agent,
    /// The child itself and its live lineage when it became resident.
    ancestry: HashSet<String>,
    /// Own events start here (the epoch's output and stop reason).
    boundary: u64,
    poke: Arc<watch::Sender<u64>>,
    st: Mutex<ActState>,
}

impl Activation {
    fn poke(&self) {
        self.poke.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn ready(&self) -> bool {
        let ready = {
            let st = self.st.lock().unwrap();
            !st.closing && st.in_flight == 0 && st.owned.is_empty()
        };
        ready && self.agent.is_idle()
    }
}

#[derive(Default)]
struct Registry {
    agents: HashMap<String, Live>,
    resident: HashMap<String, Arc<Activation>>,
    /// Slots reserved by materializations in flight, per pool.
    reserved: HashMap<String, usize>,
    draining: bool,
    /// Agents whose descendants are being drained (admission closed below them).
    closing: HashSet<String>,
}

/// Terminal facts of one one-shot run (dsh `SubagentResult`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunResult {
    pub output: Vec<Value>,
    pub stop_reason: &'static str,
    /// Infrastructure failure (the run's promise rejected in dsh).
    pub failure: Option<String>,
}

/// A published one-shot child.
pub(crate) struct OneShot {
    pub child_id: String,
    pub cancel: CancellationToken,
    pub result: oneshot::Receiver<RunResult>,
}

/// How a message enters the target's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// dsh `followup`: its own turn.
    Queue,
    /// dsh `steer`: the nearest step (a turn when idle).
    Steer,
}

/// Facts read from the parent at delegation time.
struct ParentFacts {
    header: SessionHeader,
    model: String,
    /// Provider route of `model` (the parent's last request).
    provider: Option<String>,
    effort: Option<String>,
    max_tokens: Option<u32>,
    sandbox: SandboxMode,
    /// Explicit `sandbox/mode` of the parent session.
    sandbox_override: Option<String>,
    /// `auto` / `danger-full-access` preset identity.
    preset: Option<String>,
    events: Vec<SessionEvent>,
}

struct Child {
    agent: Agent,
    header: SessionHeader,
    poke: Arc<watch::Sender<u64>>,
    boundary: u64,
}

/// Live notifications from a child agent: wake its settlement watcher and
/// hosts waiting for idleness, then forward to the host's observer.
struct ChildObserver {
    poke: Arc<watch::Sender<u64>>,
    changed: watch::Sender<u64>,
    host: Option<Arc<dyn AgentObserver>>,
}

impl AgentObserver for ChildObserver {
    fn on_delta(&self, delta: &StreamDelta) {
        if let Some(h) = &self.host {
            h.on_delta(delta);
        }
    }
    fn on_status(&self, status: &AgentStatus) {
        self.poke.send_modify(|v| *v = v.wrapping_add(1));
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        if let Some(h) = &self.host {
            h.on_status(status);
        }
    }
    fn on_attempt(&self, turn: u64, step: u64, attempt: u32) {
        if let Some(h) = &self.host {
            h.on_attempt(turn, step, attempt);
        }
    }
}

fn sandbox_from(name: &str) -> Option<SandboxMode> {
    serde_json::from_value(json!(name)).ok()
}

fn last_data<'a>(events: &'a [SessionEvent], kind: &str, field: &str) -> Option<&'a str> {
    events
        .iter()
        .rev()
        .find(|e| e.kind == kind)
        .and_then(|e| e.data.get(field))
        .and_then(Value::as_str)
}

fn failed(e: impl std::fmt::Display) -> SubagentError {
    SubagentError::new("FAILED", e.to_string())
}

/// The in-process subagent service. Build one per process, [`attach`] every
/// top-level agent whose tool calls may delegate, and register
/// [`crate::subagent_tools`] in the agents' tool registries.
///
/// [`attach`]: SubagentRuntime::attach
pub struct SubagentRuntime {
    deps: RuntimeDeps,
    settings: Mutex<SubagentSettings>,
    hooks: Mutex<Hooks>,
    reg: Mutex<Registry>,
    changed: watch::Sender<u64>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    me: Weak<SubagentRuntime>,
}

impl SubagentRuntime {
    pub fn new(deps: RuntimeDeps) -> Arc<SubagentRuntime> {
        Arc::new_cyclic(|me| SubagentRuntime {
            deps,
            settings: Mutex::new(SubagentSettings::default()),
            hooks: Mutex::new(Hooks::default()),
            reg: Mutex::new(Registry::default()),
            changed: watch::channel(0).0,
            locks: Mutex::new(HashMap::new()),
            me: me.clone(),
        })
    }

    /// Current limits; changes apply to the next delegation (like dsh).
    pub fn settings(&self) -> SubagentSettings {
        self.settings.lock().unwrap().clone()
    }

    pub fn set_settings(&self, settings: SubagentSettings) {
        *self.settings.lock().unwrap() = settings;
    }

    /// Lifecycle notifications (SDK `subagent.started` / `subagent.finished`).
    pub fn set_observer(&self, observer: Arc<dyn SubagentObserver>) {
        self.hooks.lock().unwrap().events = Some(observer);
    }

    /// Live observer factory for child agents (UI deltas and status).
    pub fn set_child_observer(&self, hook: ChildObserverHook) {
        self.hooks.lock().unwrap().child_observer = Some(hook);
    }

    /// Runs `hook` on every child agent right after it is built.
    pub fn set_child_agent(&self, hook: ChildAgentHook) {
        self.hooks.lock().unwrap().child_agent = Some(hook);
    }

    /// Overrides how a child's tools derive from its parent's.
    pub fn set_child_tools(&self, hook: ChildToolsHook) {
        self.hooks.lock().unwrap().child_tools = Some(hook);
    }

    fn hooks(&self) -> Hooks {
        self.hooks.lock().unwrap().clone()
    }

    fn arc(&self) -> Arc<SubagentRuntime> {
        self.me.upgrade().expect("runtime is alive while used")
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Registers a top-level agent so its tool calls can delegate.
    pub fn attach(&self, agent: &Agent) {
        let header = agent.header();
        let parent = header
            .extra
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.reg.lock().unwrap().agents.insert(
            agent.session_id(),
            Live {
                agent: agent.clone(),
                parent,
                cancel: None,
            },
        );
        self.bump();
    }

    /// Forgets a top-level agent (after it was disposed).
    pub fn detach(&self, session_id: &str) {
        let mut reg = self.reg.lock().unwrap();
        reg.agents.remove(session_id);
        reg.closing.remove(session_id);
        drop(reg);
        self.bump();
    }

    pub(crate) fn live(&self, id: &str) -> Option<Live> {
        self.reg.lock().unwrap().agents.get(id).cloned()
    }

    /// The live child agent with this id (resident or running one-shot).
    pub fn child(&self, id: &str) -> Option<Agent> {
        let reg = self.reg.lock().unwrap();
        reg.agents
            .get(id)
            .filter(|l| l.parent.is_some())
            .map(|l| l.agent.clone())
    }

    async fn child_lock(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Live ancestors of `id` (itself first).
    fn lineage(reg: &Registry, id: &str) -> Vec<String> {
        let mut out = vec![id.to_owned()];
        let mut seen: HashSet<String> = out.iter().cloned().collect();
        let mut next = reg.agents.get(id).and_then(|l| l.parent.clone());
        while let Some(p) = next {
            if !seen.insert(p.clone()) || !reg.agents.contains_key(&p) {
                break;
            }
            next = reg.agents.get(&p).and_then(|l| l.parent.clone());
            out.push(p);
        }
        out
    }

    fn assert_admitting(&self, agent: &str) -> Result<(), SubagentError> {
        let reg = self.reg.lock().unwrap();
        if reg.draining {
            return Err(SubagentError::new(
                "DRAINING",
                "continuable subagents are draining; the operation was not admitted",
            ));
        }
        if let Some(root) = Self::lineage(&reg, agent)
            .into_iter()
            .find(|a| reg.closing.contains(a))
        {
            return Err(SubagentError::new(
                "DRAINING",
                format!(
                    "continuable subagents below parent \"{root}\" are draining; the operation was not admitted"
                ),
            ));
        }
        Ok(())
    }

    /// dsh `resolveChildDepth`.
    fn child_depth(parent: &SessionHeader, max_depth: Option<u32>) -> Result<u32, SubagentError> {
        let depth = parent.delegation_depth + 1;
        if let Some(max) = max_depth
            && depth > max
        {
            return Err(SubagentError::depth(depth, max));
        }
        Ok(depth)
    }

    /// dsh `parentAgentOptionsForDelegation` + `captureDelegatedPolicyOverrides`.
    fn parent_facts(parent: &Agent) -> ParentFacts {
        let config = parent.config();
        let events = parent.events();
        let header_config = events
            .iter()
            .rev()
            .find(|e| e.kind == kinds::REQUEST_HEADER)
            .map(|e| e.data["header"]["config"].clone());
        let (model, provider, effort) = match &header_config {
            Some(c) => (
                c["model"].as_str().unwrap_or(&config.model).to_owned(),
                c["provider"]
                    .as_str()
                    .map(str::to_owned)
                    .or(config.provider.clone()),
                c["reasoningEffort"].as_str().map(str::to_owned),
            ),
            None => (
                config.model.clone(),
                config.provider.clone(),
                config.effort.clone(),
            ),
        };
        let sandbox_override = last_data(&events, kinds::SANDBOX_MODE, "mode").map(str::to_owned);
        let preset = last_data(&events, kinds::PERMISSION_PRESET, "preset")
            .filter(|p| *p == "auto" || *p == "danger-full-access")
            .map(str::to_owned);
        ParentFacts {
            header: parent.header(),
            model,
            provider,
            effort,
            max_tokens: config.max_tokens,
            sandbox: config.sandbox,
            sandbox_override,
            preset,
            events,
        }
    }

    /// Fork: the balanced completed-turn prefix of the parent's log.
    fn fork_seed(provider: &str, facts: &ParentFacts) -> Option<Vec<SessionEvent>> {
        if provider != "fork" {
            return None;
        }
        facts
            .events
            .iter()
            .rposition(|e| e.kind == kinds::TURN_END)
            .map(|end| facts.events[..=end].to_vec())
    }

    /// dsh `appendDelegatedPolicyOverrides`.
    fn delegated_events(facts: &ParentFacts) -> Vec<NewEvent> {
        let mut out = Vec::new();
        if let Some(mode) = &facts.sandbox_override {
            out.push(NewEvent::new(
                kinds::SANDBOX_MODE,
                json!({"mode": mode, "source": "delegation"}),
            ));
        }
        out.push(NewEvent::new(
            kinds::APPROVAL_POLICY,
            json!({"policy": "never", "source": "delegation"}),
        ));
        if let Some(preset) = &facts.preset {
            out.push(NewEvent::new(
                kinds::PERMISSION_PRESET,
                json!({"preset": preset}),
            ));
        }
        out
    }

    /// A child's configuration: the parent's, with the child's route and
    /// permission scope and the delegation statement in its runtime context.
    #[allow(clippy::too_many_arguments)]
    fn child_config(
        parent: &Agent,
        cwd: PathBuf,
        model: &str,
        effort: Option<String>,
        max_tokens: Option<u32>,
        sandbox: SandboxMode,
        approval: ApprovalPolicy,
    ) -> AgentConfig {
        let mut config = parent.config();
        if config.model != model {
            config.route = None;
        }
        config.model = model.to_owned();
        config.effort = effort;
        config.max_tokens = max_tokens;
        config.sandbox = sandbox;
        config.approval = approval;
        config.cwd = cwd;
        config
            .prompt
            .extra_context
            .retain(|s| s.name != DELEGATION_CONTEXT_NAME);
        config.prompt.extra_context.push(ExtraSection {
            name: DELEGATION_CONTEXT_NAME.into(),
            order: 1000,
            text: DELEGATION_CONTEXT.into(),
            literal: false,
        });
        config
    }

    /// Puts a child on the provider route its model came from; the route's
    /// capabilities then come from the model catalog again.
    fn child_route(config: &mut AgentConfig, provider: Option<&str>) {
        if let Some(provider) = provider
            && config.provider.as_deref() != Some(provider)
        {
            config.provider = Some(provider.to_owned());
            config.route = None;
            config.title.model = None;
        }
    }

    fn build_agent(
        &self,
        parent: &Agent,
        id: &str,
        config: AgentConfig,
        log: Arc<dyn SessionLog>,
        poke: Arc<watch::Sender<u64>>,
    ) -> Agent {
        let hooks = self.hooks();
        let parent_tools = parent.tools();
        let tools = match &hooks.child_tools {
            Some(f) => f(&parent_tools),
            None => parent_tools,
        };
        let host = hooks
            .child_observer
            .as_ref()
            .and_then(|f| f(id, &parent.session_id()));
        let child = Agent::new(
            config,
            AgentDeps {
                llm: self.deps.llm.clone(),
                tools,
                log,
                attachments: self.deps.attachments.clone(),
                approver: self.deps.approver.clone(),
                observer: Some(Arc::new(ChildObserver {
                    poke,
                    changed: self.changed.clone(),
                    host,
                })),
                tool_state: None,
            },
        );
        if let Some(hook) = &hooks.child_agent {
            hook(&child);
        }
        child
    }

    /// Creates a child session and agent (dsh `ctx.agents.create` with the
    /// subagent `setup`) and registers it as live.
    #[allow(clippy::too_many_arguments)]
    async fn create_child(
        &self,
        parent: &Agent,
        facts: &ParentFacts,
        depth: u32,
        seed: Option<Vec<SessionEvent>>,
        setup: Vec<NewEvent>,
        one_shot_descriptor: Option<Value>,
        cancel: Option<CancellationToken>,
        id: Option<String>,
    ) -> Result<Child, SubagentError> {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut extra = serde_json::Map::new();
        extra.insert("parentSession".into(), json!(facts.header.id));
        extra.insert("origin".into(), json!("subagent"));
        let header = SessionHeader {
            kind: "session".into(),
            version: SESSION_FORMAT_VERSION,
            id: id.clone(),
            created_at: now_ms(),
            cwd: facts.header.cwd.clone(),
            is_seeded: seed.is_some(),
            delegation_depth: depth,
            extra,
        };
        let seeded = seed.is_some();
        let log = self
            .deps
            .sessions
            .create(header.clone(), seed.unwrap_or_default())
            .await
            .map_err(failed)?;
        let mut events = Vec::new();
        if seeded {
            // The Session constructor marks the inherited cut.
            events.push(NewEvent::new(records::END_SEED, json!({"inherited": true})));
        }
        events.extend(setup);
        log.append(events).await.map_err(failed)?;
        let boundary = log.events().len() as u64;
        let log: Arc<dyn SessionLog> = match one_shot_descriptor {
            Some(d) => Arc::new(DescriptorLog::new(log, d)),
            None => log,
        };
        let cwd = if header.cwd.is_empty() {
            parent.config().cwd
        } else {
            PathBuf::from(&header.cwd)
        };
        let mut config = Self::child_config(
            parent,
            cwd,
            &facts.model,
            facts.effort.clone(),
            facts.max_tokens,
            facts
                .sandbox_override
                .as_deref()
                .and_then(sandbox_from)
                .unwrap_or(facts.sandbox),
            ApprovalPolicy::Never,
        );
        Self::child_route(&mut config, facts.provider.as_deref());
        let poke = Arc::new(watch::channel(0u64).0);
        let agent = self.build_agent(parent, &id, config, log, poke.clone());
        self.reg.lock().unwrap().agents.insert(
            id.clone(),
            Live {
                agent: agent.clone(),
                parent: Some(facts.header.id.clone()),
                cancel,
            },
        );
        if let Some(events) = self.hooks().events {
            events.on_started(&SubagentStarted {
                parent_session_id: facts.header.id.clone(),
                child_session_id: id.clone(),
            });
        }
        self.bump();
        Ok(Child {
            agent,
            header,
            poke,
            boundary,
        })
    }

    /// Unregisters and releases a child whose work is over.
    async fn release_child(&self, agent: &Agent) {
        let id = agent.session_id();
        // A one-shot child's own continuable children go with it.
        self.drain_below(&id).await;
        agent.dispose().await;
        if let Err(e) = self.deps.sessions.close(&id).await {
            tracing::warn!(child = %id, "closing the child session failed: {e:#}");
        }
        self.reg.lock().unwrap().agents.remove(&id);
        self.locks.lock().unwrap().remove(&id);
        self.bump();
    }

    fn finished(&self, provider: &str, parent: &str, child: &str, stop: &str, output: &[Value]) {
        if let Some(events) = self.hooks().events {
            events.on_finished(&SubagentFinished {
                provider: provider.to_owned(),
                agent_id: child.to_owned(),
                parent_session_id: parent.to_owned(),
                child_session_id: child.to_owned(),
                status: SubagentFinished::status_for(stop, false).to_owned(),
                stop_reason: stop.to_owned(),
                last_assistant_message: (!output.is_empty()).then(|| output.to_vec()),
            });
        }
    }

    /// Waits until `agent` is idle (status changes poke, the driver lock is polled).
    async fn until_idle(agent: &Agent, poke: &watch::Sender<u64>) {
        let mut rx = poke.subscribe();
        loop {
            rx.borrow_and_update();
            if agent.is_idle() {
                return;
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }

    // ---- one-shot runs (foreground spawn/fork, background fork jobs) ----

    /// Establishes one published one-shot child (dsh `subagents.start` with the
    /// in-process driver) and drives its single turn in the background.
    pub(crate) async fn start_one_shot(
        &self,
        parent: &Live,
        provider: &'static str,
        label: &str,
        prompt: &str,
        max_depth: Option<u32>,
        cancel: CancellationToken,
    ) -> Result<OneShot, SubagentError> {
        if cancel.is_cancelled() {
            return Err(SubagentError::new(
                "ABORTED",
                "subagent request was aborted before child publication",
            ));
        }
        let facts = Self::parent_facts(&parent.agent);
        let depth = Self::child_depth(&facts.header, max_depth)?;
        let seed = Self::fork_seed(provider, &facts);
        let setup = Self::delegated_events(&facts);
        let child = self
            .create_child(
                &parent.agent,
                &facts,
                depth,
                seed,
                setup,
                Some(records::one_shot_descriptor(provider, label)),
                Some(cancel.clone()),
                None,
            )
            .await?;
        let catalog = NewEvent::new(
            CATALOG,
            records::catalog_fact(&child.header, "one-shot", label),
        );
        if let Err(e) = parent.agent.append_event(catalog).await {
            self.release_child(&child.agent).await;
            return Err(failed(e));
        }
        let (tx, rx) = oneshot::channel();
        let runtime = self.arc();
        let child_id = child.header.id.clone();
        let parent_id = parent.id();
        let prompt = prompt.to_owned();
        let token = cancel.clone();
        tokio::spawn(async move {
            let result = runtime
                .drive_one_shot(child, &parent_id, provider, &prompt, token)
                .await;
            let _ = tx.send(result);
        });
        Ok(OneShot {
            child_id,
            cancel,
            result: rx,
        })
    }

    /// dsh `drivePublishedRun` + `readResult`.
    async fn drive_one_shot(
        self: Arc<Self>,
        child: Child,
        parent_id: &str,
        provider: &str,
        prompt: &str,
        cancel: CancellationToken,
    ) -> RunResult {
        let agent = child.agent.clone();
        let mut failure = None;
        if !cancel.is_cancelled() {
            let message = records::prompt_message(vec![ContentBlock::text(prompt)]);
            let run = agent.run_message(message);
            tokio::pin!(run);
            let outcome = tokio::select! {
                r = &mut run => r,
                _ = cancel.cancelled() => {
                    agent.cancel_with(CancelCause::Parent, false).await;
                    run.await
                }
            };
            if let Err(e) = outcome {
                failure = Some(format!("{e:#}"));
            }
            // Later wake-ups (e.g. the child's own job notices) belong to the run.
            let idle = Self::until_idle(&agent, &child.poke);
            tokio::select! {
                _ = idle => {}
                _ = cancel.cancelled() => {
                    agent.cancel_with(CancelCause::Parent, false).await;
                    Self::until_idle(&agent, &child.poke).await;
                }
            }
        }
        let events = agent.events();
        let own = &events[(child.boundary as usize).min(events.len())..];
        let (end, _) = records::consumed_work(own);
        let recorded = records::one_shot_stop_reason(end.as_ref());
        let stop_reason = if cancel.is_cancelled() && recorded != "completed" {
            "aborted"
        } else {
            recorded
        };
        let output = records::final_output(own).unwrap_or_default();
        let _ = agent.flush().await;
        self.finished(provider, parent_id, &child.header.id, stop_reason, &output);
        self.release_child(&agent).await;
        RunResult {
            output,
            stop_reason,
            failure,
        }
    }

    // ---- continuable children ----

    /// Reserves one activation slot in `pool` (dsh `ActivationPool.reserve`).
    fn reserve(&self, pool: &str) -> Result<(), SubagentError> {
        let capacity = self.settings().max_active;
        let mut reg = self.reg.lock().unwrap();
        let used = reg.resident.values().filter(|a| a.pool == pool).count()
            + reg.reserved.get(pool).copied().unwrap_or(0);
        if used >= capacity {
            return Err(SubagentError::new(
                "ACTIVATION_LIMIT_REACHED",
                format!(
                    "subagent limit reached (active child limit: {capacity}); wait for an existing child to finish or complete this work with the current agents"
                ),
            ));
        }
        *reg.reserved.entry(pool.to_owned()).or_default() += 1;
        Ok(())
    }

    fn unreserve(&self, pool: &str) {
        let mut reg = self.reg.lock().unwrap();
        if let Some(n) = reg.reserved.get_mut(pool) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                reg.reserved.remove(pool);
            }
        }
    }

    /// The pool a child of `parent` draws from: its resident parent's, else the parent's own.
    fn pool_of(&self, parent: &str) -> String {
        let reg = self.reg.lock().unwrap();
        reg.resident
            .get(parent)
            .map(|a| a.pool.clone())
            .unwrap_or_else(|| parent.to_owned())
    }

    /// Keeps a resident parent from settling while it establishes a child.
    fn hold(&self, parent: &str, child: &str) -> Result<bool, SubagentError> {
        let reg = self.reg.lock().unwrap();
        let Some(act) = reg.resident.get(parent) else {
            return Ok(false);
        };
        let mut st = act.st.lock().unwrap();
        if st.closing {
            return Err(SubagentError::new(
                "ACTIVATION_CLOSING",
                format!(
                    "subagent parent \"{parent}\" is being disposed; the child was not established"
                ),
            ));
        }
        Ok(st.owned.insert(child.to_owned()))
    }

    fn release_hold(&self, parent: &str, child: &str) {
        let reg = self.reg.lock().unwrap();
        if reg.resident.contains_key(child) {
            return;
        }
        if let Some(act) = reg.resident.get(parent)
            && act.st.lock().unwrap().owned.remove(child)
        {
            act.poke();
        }
    }

    /// Publishes a resident activation and starts its settlement watcher.
    fn publish(&self, child: &Child, parent: &str, provider: &str, pool: &str) -> Arc<Activation> {
        let act = {
            let mut reg = self.reg.lock().unwrap();
            let mut ancestry: HashSet<String> = Self::lineage(&reg, parent).into_iter().collect();
            ancestry.insert(child.header.id.clone());
            let act = Arc::new(Activation {
                child_id: child.header.id.clone(),
                parent_id: parent.to_owned(),
                provider: provider.to_owned(),
                pool: pool.to_owned(),
                agent: child.agent.clone(),
                ancestry,
                boundary: child.agent.log_len(),
                poke: child.poke.clone(),
                st: Mutex::new(ActState {
                    announced: false,
                    closing: false,
                    in_flight: 0,
                    owned: HashSet::new(),
                }),
            });
            reg.resident.insert(act.child_id.clone(), act.clone());
            if let Some(p) = reg.resident.get(parent) {
                p.st.lock().unwrap().owned.insert(act.child_id.clone());
            }
            act
        };
        self.unreserve(pool);
        self.bump();
        act
    }

    /// Starts the settlement watcher once the first delivery counts as activity.
    fn start_watch(&self, act: &Arc<Activation>) {
        tokio::spawn(self.arc().watch(act.clone()));
    }

    /// dsh `startContinuable`: establishes a background child and queues its
    /// task; resolves once the child accepted it.
    pub(crate) async fn start_continuable(
        &self,
        parent: &Live,
        provider: &'static str,
        label: &str,
        prompt: &str,
        max_depth: Option<u32>,
    ) -> Result<(String, String), SubagentError> {
        self.start_continuable_spec(
            parent,
            ContinuableSpec {
                child_id: None,
                provider: provider.to_owned(),
                label: label.to_owned(),
                prompt: vec![ContentBlock::text(prompt)],
                max_depth,
            },
        )
        .await
    }

    /// dsh `subagents.startContinuable(spec)` for host services (Agent
    /// Teams): `parent_id` must be an attached live agent. Returns the child
    /// id and the accepted prompt's message id.
    pub async fn start_continuable_child(
        &self,
        parent_id: &str,
        spec: ContinuableSpec,
    ) -> Result<(String, String), SubagentError> {
        let parent = self.live(parent_id).ok_or_else(|| {
            SubagentError::new(
                "UNAUTHORIZED",
                format!("agent \"{parent_id}\" is not live in this runtime"),
            )
        })?;
        self.start_continuable_spec(&parent, spec).await
    }

    async fn start_continuable_spec(
        &self,
        parent: &Live,
        spec: ContinuableSpec,
    ) -> Result<(String, String), SubagentError> {
        let ContinuableSpec {
            child_id,
            provider,
            label,
            prompt,
            max_depth,
        } = spec;
        let label = label.as_str();
        let provider = provider.as_str();
        let parent_id = parent.id();
        self.assert_admitting(&parent_id)?;
        if let Some(id) = &child_id {
            let taken = self.live(id).is_some() || self.deps.sessions.read(id).await.is_ok();
            if taken {
                return Err(SubagentError::new(
                    "DUPLICATE_CHILD",
                    format!("subagent \"{id}\" already exists"),
                ));
            }
        }
        let facts = Self::parent_facts(&parent.agent);
        let depth = Self::child_depth(&facts.header, max_depth)?;
        let agent_provider = facts
            .provider
            .clone()
            .unwrap_or_else(|| self.deps.llm.provider().to_owned());
        let descriptor = records::continuable_descriptor(
            provider,
            label,
            &agent_provider,
            &facts.model,
            facts.effort.as_deref(),
        );
        let pool = self.pool_of(&parent_id);
        self.reserve(&pool)?;
        let mut setup = vec![NewEvent::new(records::DESCRIPTOR, descriptor)];
        setup.extend(Self::delegated_events(&facts));
        let seed = Self::fork_seed(provider, &facts);
        let child = match self
            .create_child(
                &parent.agent,
                &facts,
                depth,
                seed,
                setup,
                None,
                None,
                child_id,
            )
            .await
        {
            Ok(child) => child,
            Err(e) => {
                self.unreserve(&pool);
                return Err(e);
            }
        };
        let child_id = child.header.id.clone();
        let held = match self.hold(&parent_id, &child_id) {
            Ok(held) => held,
            Err(e) => {
                self.unreserve(&pool);
                self.release_child(&child.agent).await;
                return Err(e);
            }
        };
        let act = self.publish(&child, &parent_id, provider, &pool);
        let mut content = prompt;
        // dsh appends the guidance only for the adjacent-agent `send_message`
        // (whose target is `agent_id`), not for a replacement such as Teams'.
        let adjacent = child.agent.tools().get("send_message").is_some_and(|t| {
            t.schema().parameters["properties"]
                .get("agent_id")
                .is_some()
        });
        if adjacent {
            content.push(ContentBlock::text(records::return_guidance(&parent_id)));
        }
        let message = records::prompt_message(content);
        let message_id = message.id.clone();
        // Publication guard: no settlement before the creation fact decided
        // whether the parent hears about this child.
        act.st.lock().unwrap().in_flight += 1;
        self.submit(&act, message, Delivery::Queue);
        self.start_watch(&act);
        let catalog = NewEvent::new(
            CATALOG,
            records::catalog_fact(&child.header, "continuable", label),
        );
        let committed = parent.agent.append_event(catalog).await;
        {
            let mut st = act.st.lock().unwrap();
            st.announced = committed.is_ok();
            st.in_flight -= 1;
        }
        act.poke();
        if let Err(e) = committed {
            if held {
                self.release_hold(&parent_id, &child_id);
            }
            self.dispose_activation(&act).await;
            return Err(failed(e));
        }
        Ok((child_id, message_id))
    }

    /// Delivers into a resident child, counting the delivery as activity.
    fn submit(&self, act: &Arc<Activation>, message: Message, mode: Delivery) {
        act.st.lock().unwrap().in_flight += 1;
        let act = act.clone();
        let runtime = self.arc();
        tokio::spawn(async move {
            let result = match mode {
                Delivery::Queue => act.agent.run_message(message).await.map(|_| ()),
                Delivery::Steer => act.agent.steer_message(message).await.map(|_| ()),
            };
            if let Err(e) = result {
                tracing::warn!(child = %act.child_id, "subagent delivery failed: {e:#}");
            }
            act.st.lock().unwrap().in_flight -= 1;
            act.poke();
            runtime.bump();
        });
    }

    /// dsh `sendWaking`: through the target's activation when it is resident.
    fn send_waking(&self, target: &Live, message: Message, mode: Delivery) {
        let act = self.reg.lock().unwrap().resident.get(&target.id()).cloned();
        if let Some(act) = act {
            self.submit(&act, message, mode);
            return;
        }
        let agent = target.agent.clone();
        let runtime = self.arc();
        tokio::spawn(async move {
            let result = match mode {
                Delivery::Queue => agent.run_message(message).await.map(|_| ()),
                Delivery::Steer => agent.steer_message(message).await.map(|_| ()),
            };
            if let Err(e) = result {
                tracing::warn!(agent = %agent.session_id(), "message delivery failed: {e:#}");
            }
            runtime.bump();
        });
    }

    /// Follows one activation to natural settlement (dsh `watchSettlement`).
    async fn watch(self: Arc<Self>, act: Arc<Activation>) {
        let mut rx = act.poke.subscribe();
        loop {
            rx.borrow_and_update();
            if act.st.lock().unwrap().closing {
                return;
            }
            if act.ready() {
                let seq = act.agent.log_len();
                if let Err(e) = act.agent.flush().await {
                    tracing::warn!(child = %act.child_id, "best-effort final session flush failed: {e:#}");
                }
                let guard = self.child_lock(&act.child_id).await;
                if act.ready() && act.agent.log_len() == seq {
                    self.finish(&act, true).await;
                    drop(guard);
                    return;
                }
                drop(guard);
                continue;
            }
            tokio::select! {
                changed = rx.changed() => if changed.is_err() { return },
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }

    /// Stops (unless settled naturally), captures, releases and announces one
    /// activation (dsh `finishDisposal`). Call with the child lock held.
    fn finish<'a>(
        &'a self,
        act: &'a Arc<Activation>,
        natural: bool,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            {
                let mut st = act.st.lock().unwrap();
                if st.closing {
                    return;
                }
                st.closing = true;
            }
            act.poke();
            if !natural {
                act.agent.cancel_with(CancelCause::Parent, false).await;
                let owned: Vec<Arc<Activation>> = {
                    let reg = self.reg.lock().unwrap();
                    let st = act.st.lock().unwrap();
                    st.owned
                        .iter()
                        .filter_map(|c| reg.resident.get(c).cloned())
                        .collect()
                };
                for child in owned {
                    self.dispose_activation(&child).await;
                }
                Self::until_idle(&act.agent, &act.poke).await;
                let _ = act.agent.flush().await;
            }
            let events = act.agent.events();
            let own = &events[(act.boundary as usize).min(events.len())..];
            let stop_reason = records::epoch_stop_reason(own);
            let output = records::final_output(own);
            act.agent.dispose().await;
            if let Err(e) = self.deps.sessions.close(&act.child_id).await {
                tracing::warn!(child = %act.child_id, "closing the child session failed: {e:#}");
            }
            let announced = {
                let mut reg = self.reg.lock().unwrap();
                reg.resident.remove(&act.child_id);
                reg.agents.remove(&act.child_id);
                act.st.lock().unwrap().announced
            };
            if announced {
                self.notify_settlement(act, stop_reason, output.as_deref());
            }
            // Release ownership: the parent may settle now.
            let owners: Vec<Arc<Activation>> = {
                let reg = self.reg.lock().unwrap();
                reg.resident
                    .values()
                    .filter(|a| a.st.lock().unwrap().owned.remove(&act.child_id))
                    .cloned()
                    .collect()
            };
            for owner in owners {
                owner.poke();
            }
            self.finished(
                &act.provider,
                &act.parent_id,
                &act.child_id,
                stop_reason,
                output.as_deref().unwrap_or_default(),
            );
            self.bump();
        })
    }

    /// dsh `notifySettlement`: the notice queues a turn on an idle parent and
    /// steers a busy one; parents in teardown get none.
    fn notify_settlement(&self, act: &Activation, stop_reason: &str, output: Option<&[Value]>) {
        let parent = {
            let reg = self.reg.lock().unwrap();
            if reg.draining
                || Self::lineage(&reg, &act.parent_id)
                    .iter()
                    .any(|a| reg.closing.contains(a))
            {
                return;
            }
            reg.agents.get(&act.parent_id).cloned()
        };
        let Some(parent) = parent else { return };
        let message = records::settlement_message(&act.child_id, stop_reason, output);
        let mode = match parent.agent.status() {
            AgentStatus::Idle | AgentStatus::Error { .. } => Delivery::Queue,
            _ => Delivery::Steer,
        };
        self.send_waking(&parent, message, mode);
    }

    /// Disposes one activation under its child lock.
    async fn dispose_activation(&self, act: &Arc<Activation>) {
        let guard = self.child_lock(&act.child_id).await;
        self.finish(act, false).await;
        drop(guard);
    }

    // ---- control: messaging, interrupt, discovery ----

    /// dsh `sendMessage`: to the sender's direct parent (resident continuable
    /// senders only) or to a direct continuable child (cold-resumed when absent).
    pub(crate) async fn send_message(
        &self,
        sender: &Live,
        target: &str,
        text: &str,
    ) -> Result<String, SubagentError> {
        let sender_id = sender.id();
        self.assert_admitting(&sender_id)?;
        let own = self.reg.lock().unwrap().resident.get(&sender_id).cloned();
        if let Some(act) = own.filter(|a| a.parent_id == target) {
            if act.st.lock().unwrap().closing {
                return Err(SubagentError::new(
                    "ACTIVATION_CLOSING",
                    format!(
                        "subagent \"{sender_id}\" activation is being disposed; the message was not delivered"
                    ),
                ));
            }
            let Some(parent) = self.live(target) else {
                return Err(SubagentError::new(
                    "PARENT_UNAVAILABLE",
                    "direct parent is not live; the message was not delivered",
                ));
            };
            let message = records::agent_message(&sender_id, text);
            let id = message.id.clone();
            self.send_waking(&parent, message, Delivery::Steer);
            return Ok(id);
        }
        if sender.parent.as_deref() == Some(target) {
            return Err(SubagentError::new(
                "UNAUTHORIZED",
                format!(
                    "agent \"{sender_id}\" is not a resident continuable child and cannot send to parent \"{target}\""
                ),
            ));
        }
        let held = self.hold(&sender_id, target)?;
        let message = records::agent_message(&sender_id, text);
        let result = self.deliver_to_child(sender, target, message, false).await;
        if result.is_err() && held {
            self.release_hold(&sender_id, target);
        }
        result
    }

    /// Steers `message` into a direct continuable child, cold-resuming an
    /// absent one. With `accepted` the call returns only after the child's
    /// inbox holds the message (dsh's synchronous `steer`).
    async fn deliver_to_child(
        &self,
        parent: &Live,
        child_id: &str,
        message: Message,
        accepted: bool,
    ) -> Result<String, SubagentError> {
        let guard = self.child_lock(child_id).await;
        let parent_id = parent.id();
        let resident = self.reg.lock().unwrap().resident.get(child_id).cloned();
        let result = match resident {
            Some(act) => {
                if act.parent_id != parent_id {
                    Err(SubagentError::new(
                        "UNAUTHORIZED",
                        format!("subagent \"{child_id}\" belongs to another parent session"),
                    ))
                } else if act.st.lock().unwrap().closing {
                    Err(SubagentError::new(
                        "ACTIVATION_CLOSING",
                        format!(
                            "subagent \"{child_id}\" activation is being disposed; the message was not accepted"
                        ),
                    ))
                } else {
                    let id = message.id.clone();
                    self.deliver(&act, message, accepted).await?;
                    act.st.lock().unwrap().announced = true;
                    Ok(id)
                }
            }
            None => self.cold_resume(parent, child_id, message, accepted).await,
        };
        drop(guard);
        result
    }

    /// Steers into a resident child: in the background, or (with
    /// `accepted`) once the child's inbox accepted it.
    async fn deliver(
        &self,
        act: &Arc<Activation>,
        message: Message,
        accepted: bool,
    ) -> Result<(), SubagentError> {
        if !accepted {
            self.submit(act, message, Delivery::Steer);
            return Ok(());
        }
        act.st.lock().unwrap().in_flight += 1;
        let result = act.agent.steer_detached(message).await;
        act.st.lock().unwrap().in_flight -= 1;
        act.poke();
        self.bump();
        result.map_err(failed)
    }

    /// dsh `steerHostSubagentPrompt`: steers a host-authored message (its own
    /// `source`, e.g. a Team mailbox item) into a direct continuable child
    /// of `parent_id`, cold-resuming an absent child. Returns the message id
    /// once the child's inbox accepted it.
    pub async fn steer_prompt(
        &self,
        parent_id: &str,
        child_id: &str,
        message: Message,
    ) -> Result<String, SubagentError> {
        let parent = self.live(parent_id).ok_or_else(|| {
            SubagentError::new(
                "UNAUTHORIZED",
                format!("subagent \"{child_id}\" delivery requires the exact live parent agent"),
            )
        })?;
        self.assert_admitting(parent_id)?;
        let held = self.hold(parent_id, child_id)?;
        let result = self
            .deliver_to_child(&parent, child_id, message, true)
            .await;
        if result.is_err() && held {
            self.release_hold(parent_id, child_id);
        }
        result
    }

    /// dsh `drainContinuableChildren`: releases the selected resident direct
    /// children of `parent_id`; absent targets are accepted no-ops.
    pub async fn drain_children(
        &self,
        parent_id: &str,
        child_ids: &[String],
    ) -> Result<(), SubagentError> {
        let mut targets = Vec::new();
        {
            let reg = self.reg.lock().unwrap();
            for id in child_ids {
                if let Some(act) = reg.resident.get(id) {
                    if act.parent_id != parent_id {
                        return Err(SubagentError::new(
                            "UNAUTHORIZED",
                            format!(
                                "subagent \"{id}\" is not a direct child of agent \"{parent_id}\""
                            ),
                        ));
                    }
                    targets.push(act.clone());
                }
            }
        }
        futures::future::join_all(targets.iter().map(|a| self.dispose_activation(a))).await;
        Ok(())
    }

    /// Reads a session, preferring the live agent's log.
    async fn read_session(&self, id: &str) -> anyhow::Result<(SessionHeader, Vec<SessionEvent>)> {
        if let Some(live) = self.live(id) {
            return Ok((live.agent.header(), live.agent.events()));
        }
        self.deps.sessions.read(id).await
    }

    /// dsh `coldResume`: rebuilds an absent continuable child from its log
    /// and steers the message into it.
    async fn cold_resume(
        &self,
        parent: &Live,
        child_id: &str,
        message: Message,
        accepted: bool,
    ) -> Result<String, SubagentError> {
        let unavailable = || {
            SubagentError::new(
                "NOT_RESUMABLE",
                format!("subagent \"{child_id}\" is unavailable"),
            )
        };
        let (header, events) = self
            .read_session(child_id)
            .await
            .map_err(|_| unavailable())?;
        let parent_id = parent.id();
        if header.extra.get("parentSession").and_then(Value::as_str) != Some(parent_id.as_str()) {
            return Err(SubagentError::new(
                "UNAUTHORIZED",
                format!("subagent \"{child_id}\" belongs to another parent session"),
            ));
        }
        let descriptor = records::own_descriptor(&header, &events)
            .filter(|d| d["mode"] == "continuable")
            .ok_or_else(|| {
                SubagentError::new(
                    "NOT_RESUMABLE",
                    format!(
                        "subagent \"{child_id}\" has no supported continuation state and cannot be resumed; choose a different target"
                    ),
                )
            })?;
        if self.live(child_id).is_some() {
            // A running one-shot child with a continuable descriptor cannot exist;
            // a live agent here is a different lifecycle owner.
            return Err(unavailable());
        }
        let pool = self.pool_of(&parent_id);
        self.reserve(&pool)?;
        let log = match self.deps.sessions.resume(child_id).await {
            Ok(log) => log,
            Err(_) => {
                self.unreserve(&pool);
                return Err(unavailable());
            }
        };
        let events = log.events();
        let sandbox = last_data(&events, kinds::SANDBOX_MODE, "mode")
            .and_then(sandbox_from)
            .unwrap_or_else(|| parent.agent.config().sandbox);
        let approval = last_data(&events, kinds::APPROVAL_POLICY, "policy")
            .map(ApprovalPolicy::parse)
            .unwrap_or(ApprovalPolicy::Never);
        let parent_config = parent.agent.config();
        let model = descriptor["agentModel"]
            .as_str()
            .unwrap_or(&parent_config.model)
            .to_owned();
        let effort = descriptor["agentReasoningEffort"]
            .as_str()
            .map(str::to_owned);
        let cwd = if header.cwd.is_empty() {
            parent_config.cwd.clone()
        } else {
            PathBuf::from(&header.cwd)
        };
        let config =
            Self::child_config(&parent.agent, cwd, &model, effort, None, sandbox, approval);
        let mut config = config;
        Self::child_route(&mut config, descriptor["agentProvider"].as_str());
        let poke = Arc::new(watch::channel(0u64).0);
        let agent = self.build_agent(&parent.agent, child_id, config, log, poke.clone());
        self.reg.lock().unwrap().agents.insert(
            child_id.to_owned(),
            Live {
                agent: agent.clone(),
                parent: Some(parent_id.clone()),
                cancel: None,
            },
        );
        if let Some(events) = self.hooks().events {
            events.on_started(&SubagentStarted {
                parent_session_id: parent_id.clone(),
                child_session_id: child_id.to_owned(),
            });
        }
        let child = Child {
            agent,
            header,
            poke,
            boundary: 0,
        };
        let provider = descriptor["provider"]
            .as_str()
            .unwrap_or("spawn")
            .to_owned();
        let act = self.publish(&child, &parent_id, &provider, &pool);
        let id = message.id.clone();
        if let Err(e) = self.deliver(&act, message, accepted).await {
            self.dispose_activation(&act).await;
            return Err(e);
        }
        act.st.lock().unwrap().announced = true;
        self.start_watch(&act);
        Ok(id)
    }

    /// dsh `interrupt` under an ancestor's authority: cancels the target's
    /// current turn (cause `parent`) and keeps its queued input. Absent and
    /// one-shot targets are accepted no-ops.
    pub fn interrupt(&self, target: &str, caller: &str) -> Result<(), SubagentError> {
        if caller == target {
            return Err(SubagentError::new(
                "UNAUTHORIZED",
                format!("agent \"{caller}\" cannot interrupt itself"),
            ));
        }
        let Some(act) = self.reg.lock().unwrap().resident.get(target).cloned() else {
            return Ok(());
        };
        if !act.ancestry.contains(caller) {
            return Err(SubagentError::new(
                "UNAUTHORIZED",
                format!("subagent \"{target}\" is not a live descendant of agent \"{caller}\""),
            ));
        }
        if act.st.lock().unwrap().closing {
            return Ok(());
        }
        let agent = act.agent.clone();
        tokio::spawn(async move { agent.cancel_with(CancelCause::Parent, true).await });
        Ok(())
    }

    /// Whether `id` is working (dsh `agents.get(id)?.status === "running"`).
    pub fn is_running(&self, id: &str) -> bool {
        self.live(id)
            .is_some_and(|l| l.agent.status() == AgentStatus::Running)
    }

    /// The direct-child catalog of a session (live log preferred).
    pub(crate) async fn catalog(&self, id: &str) -> anyhow::Result<Vec<CatalogEntry>> {
        let (header, events) = self.read_session(id).await?;
        Ok(records::catalog_entries(&header, &events))
    }

    // ---- host lifecycle ----

    /// Live agents below `id` (any depth).
    fn descendants(&self, id: &str) -> Vec<Live> {
        let reg = self.reg.lock().unwrap();
        reg.agents
            .values()
            .filter(|l| {
                l.parent.is_some() && Self::lineage(&reg, &l.id()).iter().skip(1).any(|a| a == id)
            })
            .cloned()
            .collect()
    }

    /// Waits until `agent` is idle and no child below it is live, so
    /// settlement notices and job notices could produce the final answer
    /// (the headless/SDK host loop of dsh: `whenIdle` + `waitForChildren`).
    pub async fn wait_idle(&self, agent: &Agent) {
        let id = agent.session_id();
        let mut rx = self.changed.subscribe();
        loop {
            rx.borrow_and_update();
            if agent.is_idle() && self.descendants(&id).is_empty() {
                let seq = agent.log_len();
                tokio::time::sleep(Duration::from_millis(50)).await;
                if agent.is_idle() && agent.log_len() == seq && self.descendants(&id).is_empty() {
                    return;
                }
                continue;
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }

    /// Stops every child below `id` (cause `parent`) and waits for their
    /// release, child first. New delegations below `id` are refused meanwhile.
    pub async fn drain_descendants(&self, id: &str) {
        self.reg.lock().unwrap().closing.insert(id.to_owned());
        self.drain_below(id).await;
        self.reg.lock().unwrap().closing.remove(id);
    }

    async fn drain_below(&self, id: &str) {
        let below = self.descendants(id);
        if below.is_empty() {
            return;
        }
        let (roots, one_shots): (Vec<Arc<Activation>>, Vec<Live>) = {
            let reg = self.reg.lock().unwrap();
            let roots = below
                .iter()
                .filter_map(|l| reg.resident.get(&l.id()).cloned())
                .filter(|a| !reg.resident.contains_key(&a.parent_id) || a.parent_id == id)
                .collect();
            let one_shots = below
                .iter()
                .filter(|l| !reg.resident.contains_key(&l.id()))
                .cloned()
                .collect();
            (roots, one_shots)
        };
        for live in &one_shots {
            if let Some(cancel) = &live.cancel {
                cancel.cancel();
            }
        }
        futures::future::join_all(roots.iter().map(|a| self.dispose_activation(a))).await;
        // One-shot runs release themselves once their turn ended.
        let mut rx = self.changed.subscribe();
        loop {
            rx.borrow_and_update();
            if self.descendants(id).is_empty() {
                return;
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }

    /// Closes admission and releases every child (process teardown).
    pub async fn shutdown(&self) {
        let roots: Vec<String> = {
            let mut reg = self.reg.lock().unwrap();
            reg.draining = true;
            reg.agents
                .values()
                .filter(|l| l.parent.is_none())
                .map(Live::id)
                .collect()
        };
        for root in roots {
            self.drain_below(&root).await;
        }
    }
}
