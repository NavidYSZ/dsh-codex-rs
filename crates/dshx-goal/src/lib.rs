//! Same-session goals for dsh-codex, a port of DeepSeek Harness 0.2.0-rc.2
//! `dsh-goal` (event-sourced state, compare-and-set mutations, process-local
//! activation), `dsh-tool-goal` (`get_goal`, `create_goal`, `update_goal`)
//! and `dsh-goal-round-driver` (automatic `<goal_round>` continuation).
//!
//! ```ignore
//! let goals = GoalService::new(GoalSettings::default());
//! for t in goal_tools(&goals) { tools.register(t); }
//! let agent = Agent::new(config, AgentDeps { tools, .. });
//! goals.attach(&agent, true);          // driver on: rounds continue while idle
//! ```

mod driver;
mod fold;
mod service;
mod tools;

pub use driver::round_prompt;
pub use fold::{
    BlockReason, GOAL_CHANGE, Goal, GoalChange, GoalFold, GoalState, Phase, decode_change,
};
pub use service::{
    Activation, GoalCommand, GoalError, GoalNotice, GoalRef, GoalService, GoalSettings, GoalView,
    Initiator, stored_goal,
};
pub use tools::{goal_tools, upstream_schema, wrapup_text};

/// Names of the tools [`goal_tools`] returns, in request order.
pub const TOOL_NAMES: [&str; 3] = ["create_goal", "get_goal", "update_goal"];
