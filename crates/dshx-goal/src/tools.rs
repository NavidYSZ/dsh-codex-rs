//! The model-facing `get_goal`, `create_goal` and `update_goal` tools (dsh
//! `dsh-tool-goal`) with their execution-time authority checks: creation,
//! edits, pause and resume need direct human input on a top-level agent;
//! `complete` and `blocked` are also allowed inside the goal's own round.
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use dshx_core::{
    CallerInfo, ContentBlock, Message, MessageSource, Role, SessionEvent, Tool, ToolContext,
    ToolError, ToolOutput, ToolSchema, kinds,
};
use serde_json::{Map, Value, json};

use crate::fold::{Phase, safe_integer};
use crate::service::{GoalError, GoalRef, GoalService, GoalView, Initiator};

const SCHEMAS_JSON: &str = include_str!("../../../docs/rust/upstream/tools-0.2.0-rc.2/goal.json");

/// The upstream schema of one goal tool.
///
/// # Panics
/// When `name` is not one of the three goal tools.
pub fn upstream_schema(name: &str) -> ToolSchema {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            let golden: Value = serde_json::from_str(SCHEMAS_JSON).expect("goal golden parses");
            golden["tools"]
                .as_array()
                .expect("golden tools")
                .iter()
                .map(|t| ToolSchema {
                    name: t["name"].as_str().unwrap_or_default().to_owned(),
                    description: t["description"].as_str().unwrap_or_default().to_owned(),
                    parameters: t["parameters"].clone(),
                })
                .collect()
        })
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no upstream schema for tool {name}"))
}

/// `create_goal`, `get_goal`, `update_goal` over one service.
pub fn goal_tools(service: &Arc<GoalService>) -> Vec<Arc<dyn Tool>> {
    let weak = Arc::downgrade(service);
    vec![
        Arc::new(GoalTool {
            service: weak.clone(),
            kind: Kind::Create,
        }),
        Arc::new(GoalTool {
            service: weak.clone(),
            kind: Kind::Get,
        }),
        Arc::new(GoalTool {
            service: weak,
            kind: Kind::Update,
        }),
    ]
}

#[derive(Clone, Copy)]
enum Kind {
    Get,
    Create,
    Update,
}

struct GoalTool {
    service: Weak<GoalService>,
    kind: Kind,
}

fn reject(message: &str, code: &'static str) -> ToolError {
    ToolError::coded("HarnessError", code, message)
}

/// dsh `goalToolExecution`: the live calling agent inside its open turn.
fn execution(ctx: &ToolContext) -> Result<CallerInfo, ToolError> {
    ctx.caller.clone().ok_or_else(|| {
        reject(
            "goal tools require an open model turn",
            "GOAL_TOOL_DRIVER_REQUIRED",
        )
    })
}

/// The caller's current goal. Agents the host did not attach (subagents,
/// which inherit the goal tools) read it from their own log, disarmed.
fn current(
    service: &GoalService,
    session: &str,
    caller: &CallerInfo,
) -> Result<Option<GoalView>, ToolError> {
    if service.is_attached(session) {
        return service.get(session).map_err(goal_err);
    }
    crate::service::stored_goal(&caller.session.events_from(0)).map_err(goal_err)
}

/// dsh `hasDirectHumanInput`: a `source.kind: "user"` message in this turn of a root agent.
fn direct_human(caller: &CallerInfo, events: &[SessionEvent]) -> bool {
    caller.root
        && events
            .iter()
            .any(|e| e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "user")
}

/// dsh `isMatchingGoalRound`.
fn matching_round(events: &[SessionEvent], goal: &GoalView) -> bool {
    events.iter().any(|e| {
        let s = &e.data["source"];
        e.kind == kinds::USER_MESSAGE
            && s["kind"] == "goal"
            && s["goalId"].as_str() == Some(goal.goal.id.as_str())
            && s["revision"] == json!(goal.goal.revision)
            && s["round"] == json!(goal.rounds_started)
    })
}

/// dsh `goalValue`: the compact model result.
fn goal_value(view: Option<&GoalView>) -> Value {
    let Some(v) = view else {
        return json!({"goal": null});
    };
    let mut g = Map::new();
    g.insert("id".into(), v.goal.id.clone().into());
    g.insert("revision".into(), v.goal.revision.into());
    g.insert("objective".into(), v.goal.objective.clone().into());
    g.insert("phase".into(), v.goal.phase.as_str().into());
    g.insert("roundsStarted".into(), v.rounds_started.into());
    g.insert("maxGoalRounds".into(), v.goal.max_goal_rounds.into());
    if let Some(r) = &v.goal.blocked_reason {
        g.insert(
            "blockedReason".into(),
            json!({"code": r.code, "message": r.message}),
        );
    }
    json!({"goal": Value::Object(g), "activation": v.activation.as_str()})
}

fn output(value: Value) -> ToolOutput {
    let mut out = ToolOutput::text(serde_json::to_string(&value).unwrap_or_default());
    out.value = Some(value);
    out
}

fn goal_err(e: GoalError) -> ToolError {
    e.into_tool_error()
}

fn invalid(message: &str) -> ToolError {
    reject(message, "GOAL_TOOL_INVALID_UPDATE")
}

/// Meaningful optional text (not a strict-schema empty filler).
fn text_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Meaningful optional round cap (not a strict-schema zero filler).
fn cap_arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key)
        .filter(|v| v.is_number() && v.as_f64() != Some(0.0))
}

/// dsh `boundContextSummary`: 120 UTF-16 units.
fn bound_summary(summary: &str) -> String {
    let units: Vec<u16> = summary.encode_utf16().collect();
    if units.len() <= 120 {
        summary.to_owned()
    } else {
        format!("{}…", String::from_utf16_lossy(&units[..119]))
    }
}

/// dsh `renderWrapupContext`: the closing-message instruction after an
/// automatic round reported `complete` or `blocked`.
pub fn wrapup_text(objective: &str, blocked_reason: Option<&str>) -> String {
    let heading = format!(
        "Objective: {}\n",
        serde_json::to_string(objective).unwrap_or_default()
    );
    match blocked_reason {
        None => format!(
            "<goal_complete>\n{heading}The goal is marked complete and this autonomous run is ending. Write the closing message to the user now: state the outcome, summarize what was done and how it was verified, and point to the concrete results (files, commits, or other artifacts). Report only what earlier rounds and tool results in this session actually establish; when a detail is not in the session, say so instead of inventing it. Note anything the user should review or do next. Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</goal_complete>"
        ),
        Some(reason) => format!(
            "<goal_blocked>\n{heading}Blocked: {}\nThe goal is marked blocked and this autonomous run is ending. Write the closing message to the user now: state what has been completed so far, describe the concrete blocking condition and what you tried, and say exactly what you need from the user to continue. Report only what earlier rounds and tool results in this session actually establish; when a detail is not in the session, say so instead of inventing it. Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</goal_blocked>",
            serde_json::to_string(reason).unwrap_or_default()
        ),
    }
}

fn wrapup_message(action: &str, objective: &str, blocked_reason: Option<&str>) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(wrapup_text(objective, blocked_reason))],
        source: MessageSource::Other(json!({
            "kind": "tool-goal",
            "form": "notice",
            "summary": bound_summary(&format!("{action}: {objective}")),
        })),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

#[async_trait]
impl Tool for GoalTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema(match self.kind {
            Kind::Get => "get_goal",
            Kind::Create => "create_goal",
            Kind::Update => "update_goal",
        })
    }

    fn describe(&self, args: &Value) -> String {
        match self.kind {
            Kind::Get => "Read current goal".into(),
            Kind::Create => "Create goal".into(),
            Kind::Update => match args.get("action").and_then(Value::as_str) {
                Some("blocked") => "Mark goal".into(),
                Some(a) => {
                    let mut c = a.chars();
                    match c.next() {
                        Some(f) => format!("{}{} goal", f.to_uppercase(), c.as_str()),
                        None => "Update goal".into(),
                    }
                }
                None => "Update goal".into(),
            },
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let service = self
            .service
            .upgrade()
            .ok_or_else(|| ToolError::Failed("the goal service is no longer available".into()))?;
        let caller = execution(&ctx)?;
        let session = ctx.session_id.as_str();
        match self.kind {
            Kind::Get => {
                let view = current(&service, session, &caller)?;
                Ok(output(goal_value(view.as_ref())))
            }
            Kind::Create => {
                let events = caller.open_turn_events();
                if !direct_human(&caller, &events) {
                    return Err(reject(
                        "this goal operation requires a direct human turn on a top-level agent",
                        "GOAL_TOOL_AUTHORITY_REQUIRED",
                    ));
                }
                let objective = args["objective"].as_str().unwrap_or_default();
                let view = service
                    .create(
                        session,
                        objective,
                        args.get("max_goal_rounds"),
                        Initiator::Agent,
                    )
                    .await
                    .map_err(goal_err)?;
                Ok(output(goal_value(Some(&view))))
            }
            Kind::Update => self.update(&service, session, &caller, &args).await,
        }
    }
}

impl GoalTool {
    async fn update(
        &self,
        service: &GoalService,
        session: &str,
        caller: &CallerInfo,
        args: &Value,
    ) -> Result<ToolOutput, ToolError> {
        let goal_id = args["goal_id"].as_str().unwrap_or_default();
        let revision = safe_integer(&args["revision"]).filter(|r| *r >= 1);
        let (Some(revision), true) = (revision, !goal_id.is_empty() && goal_id == goal_id.trim())
        else {
            return Err(invalid(
                "goal_id must be non-empty and revision must be a positive safe integer",
            ));
        };
        let r = GoalRef {
            id: goal_id.to_owned(),
            revision: revision as u64,
        };
        let objective = text_arg(args, "objective");
        let cap = cap_arg(args, "max_goal_rounds");
        let blocked_reason = text_arg(args, "blocked_reason");
        let action = args["action"].as_str().unwrap_or_default();
        let events = caller.open_turn_events();
        let human = direct_human(caller, &events);
        let need_human = || {
            if human {
                Ok(())
            } else {
                Err(reject(
                    "this goal operation requires a direct human turn on a top-level agent",
                    "GOAL_TOOL_AUTHORITY_REQUIRED",
                ))
            }
        };
        match action {
            "edit" => {
                need_human()?;
                if blocked_reason.is_some() {
                    return Err(invalid("blocked_reason is valid only with action blocked"));
                }
                let view = service
                    .edit(session, &r, objective, cap, Initiator::Agent)
                    .await
                    .map_err(goal_err)?;
                return Ok(output(goal_value(Some(&view))));
            }
            "pause" | "resume" => {
                need_human()?;
                if objective.is_some() || cap.is_some() || blocked_reason.is_some() {
                    return Err(invalid(
                        "objective and max_goal_rounds are valid only with action edit; blocked_reason is valid only with action blocked",
                    ));
                }
                let current = current(service, session, caller)?;
                if action == "resume"
                    && current.as_ref().is_some_and(|c| {
                        c.goal.id == r.id
                            && c.goal.revision == r.revision
                            && c.goal.phase == Phase::Paused
                    })
                {
                    return Err(reject(
                        "the model cannot resume a paused goal; the user must resume it",
                        "GOAL_TOOL_RESUME_PAUSED",
                    ));
                }
                let view = if action == "pause" {
                    service.pause(session, &r, Initiator::Agent).await
                } else {
                    service.resume(session, &r, Initiator::Agent).await
                }
                .map_err(goal_err)?;
                return Ok(output(goal_value(Some(&view))));
            }
            _ => {}
        }
        // complete / blocked: direct human input or the goal's exact round.
        let round = if human {
            None
        } else {
            match current(service, session, caller)? {
                Some(goal) if matching_round(&events, &goal) => Some(goal),
                _ => {
                    return Err(reject(
                        "complete and blocked require a direct human turn or the current goal round",
                        "GOAL_TOOL_AUTHORITY_REQUIRED",
                    ));
                }
            }
        };
        if objective.is_some() || cap.is_some() {
            return Err(invalid(
                "objective and max_goal_rounds are valid only with action edit",
            ));
        }
        if action == "complete" && blocked_reason.is_some() {
            return Err(invalid("blocked_reason is valid only with action blocked"));
        }
        let reason = args.get("blocked_reason").and_then(Value::as_str);
        if action == "blocked" && reason.is_none_or(|r| r.trim().is_empty()) {
            return Err(invalid("blocked_reason is required with action blocked"));
        }
        let blocked_after = service.settings().blocked_after_consecutive_rounds;
        if action == "blocked"
            && let Some(goal) = &round
            && goal.rounds_started < blocked_after
        {
            return Err(reject(
                &format!(
                    "blocked requires at least {blocked_after} consecutive goal rounds; current round is {}",
                    goal.rounds_started
                ),
                "GOAL_TOOL_BLOCK_THRESHOLD",
            ));
        }
        let view = if action == "complete" {
            service.complete(session, &r, Initiator::Agent).await
        } else {
            let reason = reason.unwrap_or_default();
            service
                .block(session, &r, "model-reported", reason, Initiator::Agent)
                .await
        }
        .map_err(goal_err)?;
        let mut out = output(goal_value(Some(&view)));
        if round.is_some() {
            let blocked = (action == "blocked").then(|| reason.unwrap_or_default());
            out.contexts
                .push(wrapup_message(action, &view.goal.objective, blocked));
        }
        Ok(out)
    }
}
