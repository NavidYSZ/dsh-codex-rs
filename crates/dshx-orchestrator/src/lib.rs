//! Orchestration runs for dsh-codex (an Aether feature, no upstream
//! port): a durable contract (objective, acceptance criteria, budgets) in
//! the lead session's event log, planner rounds that submit versioned
//! dependency-ordered plans, code-owned dispatch of one-shot subagent
//! workers, and completion that must cite evidence for every criterion.
//!
//! ```ignore
//! let orchestrator = OrchestratorService::new(settings, subagents.clone());
//! for t in orchestrator_tools(&orchestrator) { tools.register(t); }
//! let agent = Agent::new(config, AgentDeps { tools, .. });
//! orchestrator.attach(&agent, true);  // driver on: rounds and dispatch continue while idle
//! ```

mod driver;
mod fold;
mod service;
mod tools;

pub use driver::round_prompt;
pub use fold::{
    Check, Criterion, Limits, ORCHESTRATION_CHANGE, OrchestratorFold, Phase, Run, RunChange,
    RunState, TaskRecord, TaskSpec, TaskStatus, TaskUpdate, decode_change,
};
pub use service::{
    Activation, ContractInput, Initiator, OrchestrationCommand, OrchestratorError,
    OrchestratorNotice, OrchestratorService, OrchestratorSettings, RunRef, RunView,
    stored_orchestration,
};
pub use tools::{orchestrator_tools, wrapup_text};

/// Names of the tools [`orchestrator_tools`] returns, in request order.
pub const TOOL_NAMES: [&str; 3] = [
    "create_orchestration",
    "get_orchestration",
    "update_orchestration",
];

/// The prompt section of the orchestration tools (order 2450, between
/// goals and subagents).
pub const POLICY: &str = "Orchestration runs coordinate larger multi-step work in this session. A run is a durable contract: one objective, acceptance criteria and budgets. While a run is active, reserved <orchestration_round> prompts ask you to decide the next move: submit the next plan version with update_orchestration (small, dependency-ordered task lists; prefer one task when one task suffices). Planned tasks are dispatched to one-shot worker subagents automatically after your turn — do not delegate planned tasks yourself. Complete the run only when every acceptance criterion is met, citing concrete evidence for each criterion (artifact paths, command results, worker sessions); a claim without evidence does not complete a run. When progress is impossible, block the run with a lower-kebab-case code — needs-input when only the user can unblock.";
