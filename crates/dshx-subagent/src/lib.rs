//! In-process subagents for dsh-codex, a port of DeepSeek Harness 0.2.0-rc.2
//! `dsh-subagent` with the `spawn` and `fork` in-process providers and the
//! model-facing `subagent`, `subagent_fork`, `send_message`,
//! `interrupt_agent` and `list_agents` tools (dsh-base composition).
//!
//! A child is another [`dshx_agent::Agent`] over its own session in the
//! parent's store: header `parentSession`, `origin: "subagent"`,
//! `delegationDepth` (fork children are `isSeeded` with the parent's
//! completed-turn prefix and a `session/end-seed {inherited: true}` cut); the
//! parent records `subagent/catalog`, the child `subagent/descriptor`.
//!
//! ```ignore
//! let runtime = SubagentRuntime::new(RuntimeDeps {
//!     llm: llm.clone(),
//!     sessions: Arc::new(StoreSessions::new(store.clone())),
//!     attachments: attachments.clone(),
//!     approver: approver.clone(),
//! });
//! runtime.set_observer(sdk_notifier);            // subagent.started / .finished
//! let mut tools = ToolRegistry::new();
//! for t in dshx_tools::default_tools(&config) { tools.register(t); }
//! for t in subagent_tools(runtime.clone()) { tools.register(t); }
//! let agent = Agent::new(agent_config, AgentDeps { tools, .. });
//! runtime.attach(&agent);                        // its tool calls may delegate
//! agent.run_turn(input).await?;
//! runtime.wait_idle(&agent).await;               // optional: let children report back
//! runtime.drain_descendants(&agent.session_id()).await;  // before disposing it
//! ```

mod error;
mod events;
mod log;
mod records;
mod runtime;
mod sessions;
mod tools;

pub use error::SubagentError;
pub use events::{SubagentFinished, SubagentObserver, SubagentStarted};
pub use records::DELEGATION_CONTEXT;
pub use records::subagent_descriptor;
pub use runtime::{
    ChildAgentHook, ChildObserverHook, ChildToolsHook, ContinuableSpec, RuntimeDeps,
    SubagentRuntime, SubagentSettings,
};
pub use sessions::{MemorySessions, SessionFactory, StoreSessions};
pub use tools::{subagent_tools, upstream_schema};

/// Names of the tools [`subagent_tools`] returns, in request order.
pub const TOOL_NAMES: [&str; 5] = [
    "interrupt_agent",
    "list_agents",
    "send_message",
    "subagent",
    "subagent_fork",
];
