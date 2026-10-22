//! The dsh-codex agent loop, a Rust port of DeepSeek Harness'
//! `core/agent-loop` and the plugins the dsh `headless` profile mounts around
//! it: tool scheduling (`tool-calls.ts`), request retries (`llm-retry`), the
//! system prompt and runtime context, workspace instructions
//! (`agent-instructions`), compaction (`compaction-basic`, pruner), session
//! titles and the repeat-tool reminder. Every durable fact goes to the
//! [`dshx_core::SessionLog`] with dsh's event names and payload shapes; live
//! deltas go to an [`AgentObserver`].
//!
//! ```ignore
//! let agent = Agent::new(AgentConfig::new("deepseek-flash", cwd), AgentDeps { .. });
//! let outcome = agent.run_turn(vec![ContentBlock::text("fix the tests")]).await?;
//! ```

mod agent;
mod compaction;
pub mod config;
mod events;
mod hooks;
mod instructions;
mod jobs;
mod journal;
pub mod observer;
mod offload;
mod prompt;
mod recovery;
pub mod retry;
mod scheduler;
mod schema;
mod stream;
pub mod testing;
mod title;

pub use agent::{Agent, AgentDeps};
pub use config::{
    AgentConfig, ApprovalPolicy, CompactionConfig, ExtraSection, InstructionsConfig, PromptConfig,
    PrunerConfig, RepeatReminderConfig, RouteCapabilities, TitleConfig, ToolUpdate,
};
pub use events::{CancelCause, TurnEndReason, TurnOutcome};
pub use hooks::{PreStep, PreStepHook};
pub use observer::{AgentObserver, AgentStatus, DeltaKind, NoopObserver, StreamDelta};
pub use prompt::render_system_prompt;
pub use retry::{RetryMode, RetryPolicy};

/// Event type of a model attempt that committed no surface message.
pub const ASSISTANT_ATTEMPT: &str = "assistant/attempt";
