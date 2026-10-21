//! Agent Teams for dsh-codex, a port of DeepSeek Harness 0.2.0-rc.2's opt-in
//! `dsh-experimental-agent-team-profile`: the `agentTeams` service (named
//! durable teammates as continuable children of the Lead, a durable Team
//! mailbox with delivery acknowledgements, a shared task DAG with
//! compare-and-set updates, activity waits) and its model-facing tools,
//! which replace the subagent tools. All Team facts are `team/*` v2 events
//! in the Lead session.
//!
//! ```ignore
//! let teams = TeamService::new(TeamSettings::default(), subagents.clone(), sessions);
//! for t in team_tools(&teams) { tools.register(t); }   // instead of subagent_tools
//! teams.attach_lead(&agent);
//! subagents.set_child_agent(Arc::new(move |child| teams.on_child(child)));
//! ```

mod fold;
mod service;
mod tools;

pub use fold::{Member, QueuedMessage, Task, TeamState};
pub use service::{Membership, TaskUpdate, TeamError, TeamService, TeamSettings, stored_team};
pub use tools::{TOOL_NAMES, team_tools, upstream_schema};
