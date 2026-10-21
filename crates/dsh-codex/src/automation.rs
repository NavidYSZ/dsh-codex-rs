//! Goals, schedules, request clock readings and Agent Teams for the hosts
//! (`dshx-goal`, `dshx-schedule`, `dshx-team`): the tools a session gets,
//! the hooks every agent receives, and the REST handlers of `dsh-codex web`
//! (`/api/sessions/:id/goal`, `/api/schedules`, `/api/sessions/:id/team`).
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dshx_agent::Agent;
use dshx_core::ToolRegistry;
use dshx_core::config::Config;
use dshx_goal::{GoalCommand, GoalService, GoalSettings};
use dshx_schedule::{Retention, ScheduleService, SessionResolver, TimeContextHook};
use dshx_server::{NotFound, StreamMessage};
use dshx_subagent::{SessionFactory, SubagentRuntime};
use dshx_team::{TeamService, TeamSettings};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::runtime::Runtime;

/// The automation services of one process.
pub struct Automation {
    pub goals: Option<Arc<GoalService>>,
    pub teams: Option<Arc<TeamService>>,
    pub time_context: Option<Arc<TimeContextHook>>,
    schedules: OnceLock<Arc<ScheduleService>>,
    drive_goals: AtomicBool,
}

impl Automation {
    pub fn new(
        config: &Config,
        subagents: &Arc<SubagentRuntime>,
        sessions: Arc<dyn SessionFactory>,
    ) -> Automation {
        let codex = &config.codex;
        let goals = codex
            .goal
            .enabled
            .then(|| GoalService::new(GoalSettings::from_config(&codex.goal)));
        let teams = codex.team.enabled.then(|| {
            TeamService::new(
                TeamSettings::from_config(&codex.team),
                subagents.clone(),
                sessions,
            )
        });
        let time_context = codex.time_context_enabled().then(|| {
            TimeContextHook::new(
                codex.time_context.time_zone.as_deref(),
                codex.time_context.refresh_interval_ms,
                None,
            )
        });
        let time_context = match time_context {
            Some(Ok(hook)) => Some(hook),
            Some(Err(e)) => {
                tracing::warn!("{e:#}; time context is off");
                None
            }
            None => None,
        };
        Automation {
            goals,
            teams,
            time_context,
            schedules: OnceLock::new(),
            drive_goals: AtomicBool::new(false),
        }
    }

    pub fn schedules(&self) -> Option<&Arc<ScheduleService>> {
        self.schedules.get()
    }

    /// Long-running hosts continue armed goals automatically.
    pub fn enable_goal_driver(&self) {
        self.drive_goals.store(true, Ordering::SeqCst);
    }

    /// Goal tools, the Team tools in place of the subagent tools, and the
    /// schedule tools while a host delivers reminders.
    pub fn register_tools(&self, tools: &mut ToolRegistry, subagents: &Arc<SubagentRuntime>) {
        match &self.teams {
            Some(teams) => {
                for tool in dshx_team::team_tools(teams) {
                    tools.register(tool);
                }
            }
            None => {
                for tool in dshx_subagent::subagent_tools(subagents.clone()) {
                    tools.register(tool);
                }
            }
        }
        if let Some(goals) = &self.goals {
            for tool in dshx_goal::goal_tools(goals) {
                tools.register(tool);
            }
        }
        if let Some(schedules) = self.schedules.get() {
            for tool in dshx_schedule::schedule_tools(schedules) {
                tools.register(tool);
            }
        }
    }

    /// A child's tools: its parent's without the root-only schedule tools.
    pub fn child_tools(parent: &ToolRegistry) -> ToolRegistry {
        let mut tools = ToolRegistry::new();
        for tool in parent.iter() {
            if !dshx_schedule::TOOL_NAMES.contains(&tool.schema().name.as_str()) {
                tools.register(tool.clone());
            }
        }
        tools
    }

    /// Hooks of a top-level agent; call after every other pre-step hook.
    pub fn attach_root(&self, agent: &Agent) {
        if let Some(goals) = &self.goals {
            goals.attach(agent, self.drive_goals.load(Ordering::SeqCst));
        }
        if let Some(teams) = &self.teams {
            teams.attach_lead(agent);
        }
        if let Some(hook) = &self.time_context {
            agent.add_pre_step_hook(hook.clone());
        }
    }

    /// Hooks of a subagent or teammate; call after its other pre-step hooks.
    pub fn attach_child(&self, child: &Agent) {
        if let Some(teams) = &self.teams {
            teams.on_child(child);
        }
        if let Some(hook) = &self.time_context {
            child.add_pre_step_hook(hook.clone());
        }
    }

    /// Releases what [`Automation::attach_root`] registered.
    pub async fn close(&self, session_id: &str) {
        if let Some(goals) = &self.goals {
            goals.detach(session_id).await;
        }
        if let Some(teams) = &self.teams {
            teams.detach(session_id);
        }
    }

    /// Opens `storages/schedule.json` and starts delivering due reminders
    /// (only `dsh-codex web`, which can reopen any session, delivers).
    pub fn start_schedules(
        &self,
        home: &Path,
        config: &Config,
        resolver: Arc<dyn SessionResolver>,
    ) -> anyhow::Result<()> {
        if !config.codex.schedule.enabled || self.schedules.get().is_some() {
            return Ok(());
        }
        let retention = Retention {
            days: config.codex.schedule.delivery_history_days.clamp(1, 3650),
            records: config
                .codex
                .schedule
                .delivery_history_records
                .clamp(1, 10_000),
        };
        let service = ScheduleService::open(&home.join("storages"), retention, None)?;
        service.start(resolver);
        let _ = self.schedules.set(service);
        Ok(())
    }

    pub async fn shutdown(&self) {
        if let Some(s) = self.schedules.get() {
            s.dispose().await;
        }
        if let Some(t) = &self.teams {
            t.shutdown();
        }
    }

    /// `/api/info` `automation` flags.
    pub fn features(&self) -> Value {
        json!({
            "goals": self.goals.is_some(),
            "schedules": self.schedules.get().is_some(),
            "timeContext": self.time_context.is_some(),
            "team": self.teams.is_some(),
        })
    }

    /// Forwards goal notices and `schedule/changed` into a UI stream.
    pub fn forward(&self, tx: broadcast::Sender<StreamMessage>) {
        if let Some(goals) = &self.goals {
            let mut rx = goals.subscribe();
            let tx = tx.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(n) => {
                            let _ = tx.send(StreamMessage::Goal {
                                session_id: n.session_id,
                                goal: n.goal,
                            });
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
        }
        if let Some(schedules) = self.schedules.get() {
            let mut rx = schedules.subscribe();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                            let _ = tx.send(StreamMessage::ScheduleChanged);
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
        }
    }
}

// ---- REST handlers of `dsh-codex web` ----

fn goals(runtime: &Runtime) -> anyhow::Result<&Arc<GoalService>> {
    runtime
        .automation
        .goals
        .as_ref()
        .ok_or_else(|| NotFound("goals".into()).into())
}

fn schedules(runtime: &Runtime) -> anyhow::Result<&Arc<ScheduleService>> {
    runtime
        .automation
        .schedules()
        .ok_or_else(|| NotFound("schedules".into()).into())
}

fn stored_events(runtime: &Runtime, id: &str) -> anyhow::Result<Vec<dshx_core::SessionEvent>> {
    if runtime.store.locate(id)?.is_none() {
        return Err(NotFound(id.to_owned()).into());
    }
    Ok(runtime.store.read(id)?.events)
}

/// `GET /api/sessions/:id/goal`.
pub fn goal(runtime: &Runtime, id: &str) -> anyhow::Result<Value> {
    let goals = goals(runtime)?;
    let view = if goals.is_attached(id) {
        goals.get(id)?
    } else {
        dshx_goal::stored_goal(&stored_events(runtime, id)?)?
    };
    Ok(json!({"goal": view.map(|v| v.to_json())}))
}

/// `POST /api/sessions/:id/goal` on an open session (dsh `/goal`).
pub async fn goal_command(runtime: &Runtime, id: &str, body: &Value) -> anyhow::Result<Value> {
    let goals = goals(runtime)?;
    let objective = body["objective"].as_str().unwrap_or_default().to_owned();
    let command = match body["action"].as_str().unwrap_or_default() {
        "create" => GoalCommand::Create(objective),
        "edit" => GoalCommand::Edit(objective),
        "pause" => GoalCommand::Pause,
        "resume" => GoalCommand::Resume,
        "clear" => GoalCommand::Clear,
        other => {
            anyhow::bail!("unknown goal action {other:?}: use create, edit, pause, resume or clear")
        }
    };
    let view = goals
        .command(id, command, body.get("maxGoalRounds"))
        .await?;
    Ok(json!({"goal": view.map(|v| v.to_json())}))
}

/// `GET /api/schedules`.
pub fn schedule_catalog(runtime: &Runtime) -> anyhow::Result<Value> {
    Ok(Value::Array(schedules(runtime)?.catalog()))
}

/// `GET /api/sessions/:id/schedules`.
pub fn session_schedules(runtime: &Runtime, id: &str) -> anyhow::Result<Value> {
    Ok(Value::Array(schedules(runtime)?.list_views(id)))
}

fn task_session(service: &ScheduleService, id: &str) -> anyhow::Result<String> {
    service
        .session_of(id)
        .ok_or_else(|| NotFound(id.to_owned()).into())
}

/// A tool-shaped failure value as an HTTP error.
fn failure(value: Value) -> anyhow::Result<Value> {
    if let Some(message) = value["message"].as_str() {
        anyhow::bail!("{message} ({})", value["code"].as_str().unwrap_or_default());
    }
    match value["code"].as_str() {
        Some("schedule_not_found") => {
            Err(NotFound(value["id"].as_str().unwrap_or_default().to_owned()).into())
        }
        Some(code) => anyhow::bail!("{code}"),
        None => Ok(value),
    }
}

/// `PUT /api/schedules/:id` with `schedule_update`'s fields.
pub async fn update_schedule(runtime: &Runtime, id: &str, body: &Value) -> anyhow::Result<Value> {
    let service = schedules(runtime)?;
    let session = task_session(service, id)?;
    let mut args = body.as_object().cloned().unwrap_or_default();
    args.insert("id".into(), id.into());
    let args = Value::Object(args);
    if let Some(invalid) = dshx_schedule::validate_update(&args) {
        return failure(invalid);
    }
    let result = service
        .update_from_args(&session, &args)
        .await
        .map_err(|e| anyhow::anyhow!("{} ({})", e.message, e.code))?;
    failure(result)
}

/// `DELETE /api/schedules/:id`.
pub async fn delete_schedule(runtime: &Runtime, id: &str) -> anyhow::Result<Value> {
    let service = schedules(runtime)?;
    let session = task_session(service, id)?;
    let result = service
        .delete(&session, id)
        .await
        .map_err(|e| anyhow::anyhow!("{} ({})", e.message, e.code))?;
    failure(result)
}

/// `GET /api/schedules/:id/history?limit=&before=`.
pub fn schedule_history(
    runtime: &Runtime,
    id: &str,
    limit: Option<i64>,
    before: Option<&str>,
) -> anyhow::Result<Value> {
    let service = schedules(runtime)?;
    let session = task_session(service, id)?;
    let page = service
        .history(&session, id, limit.unwrap_or(20), before)
        .map_err(|e| anyhow::anyhow!("{} ({})", e.message, e.code))?;
    if page["code"] == "delivery_cursor_not_found" {
        anyhow::bail!("delivery_cursor_not_found");
    }
    failure(page)
}

/// `GET /api/sessions/:id/team`.
pub fn team(runtime: &Runtime, id: &str) -> anyhow::Result<Value> {
    let teams = runtime
        .automation
        .teams
        .as_ref()
        .ok_or_else(|| anyhow::Error::from(NotFound("team".into())))?;
    match teams.team_view(id) {
        Some(view) => Ok(view?),
        None => Ok(dshx_team::stored_team(id, &stored_events(runtime, id)?)),
    }
}

/// A deleted session's reminders stop (dsh `workspace/session-stop`).
pub async fn session_deleted(runtime: &Runtime, id: &str) {
    if let Some(s) = runtime.automation.schedules()
        && let Err(e) = s.stop_session_tasks(id).await
    {
        tracing::warn!("schedule: stopping the reminders of {id} failed: {e:#}");
    }
}
