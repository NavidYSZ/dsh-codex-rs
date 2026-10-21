//! Host reminders and request clock readings for dsh-codex, a port of
//! DeepSeek Harness 0.2.0-rc.2's `dsh-experimental-schedule-bundle`:
//! `dsh-schedule` (durable host tasks in `storages/schedule.json`, the
//! delivery runtime, `schedule_create`/`list`/`delete`/`update` for
//! top-level agents) and `dsh-time-context`.
//!
//! ```ignore
//! let schedules = ScheduleService::open(&home.join("storages"), Retention::default(), None)?;
//! for t in schedule_tools(&schedules) { root_tools.register(t); }
//! schedules.start(resolver);            // delivers due reminders as follow-ups
//! agent.add_pre_step_hook(TimeContextHook::new(None, 600_000, None)?);
//! ```

mod cron;
mod record;
mod service;
mod store;
mod time;
mod time_context;
mod tools;

pub use record::{Occurrence, Record, Rule, batch_text, reminder_text, resolve_occurrence};
pub use service::{Clock, Retention, ScheduleService, SessionResolver};
pub use store::{Status, Task};
pub use time::iso;
pub use time_context::{TimeContextHook, format_duration, format_timestamp};
pub use tools::{TOOL_NAMES, schedule_tools, upstream_schema, validate_update};

/// A rule or input that cannot become a schedule (dsh `ScheduleInputError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub code: &'static str,
    pub message: String,
}

impl InputError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        InputError {
            code,
            message: message.into(),
        }
    }

    /// `{code, message}`, the tools' error value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"code": self.code, "message": self.message})
    }
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InputError {}
