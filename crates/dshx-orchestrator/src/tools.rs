//! The model-facing `get_orchestration`, `create_orchestration` and
//! `update_orchestration` tools with their execution-time authority checks:
//! creation needs direct human input on a top-level agent; `plan`,
//! `complete` and `blocked` are also allowed inside the run's own round.
//! Worker dispatch is code-owned: the model never spawns workers itself.
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use dshx_core::{
    CallerInfo, ContentBlock, Message, MessageSource, Role, SessionEvent, Tool, ToolContext,
    ToolError, ToolOutput, ToolSchema, kinds,
};
use serde_json::{Value, json};

use crate::fold::{Check, Criterion, TaskSpec, safe_integer};
use crate::service::{
    ContractInput, Initiator, OrchestratorError, OrchestratorService, RunRef, RunView,
};

/// The schema of one orchestration tool (Aether's own; no upstream golden).
fn schema(name: &str) -> ToolSchema {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            let create = json!({
                "name": "create_orchestration",
                "description": "Start an orchestration run: a durable objective with acceptance criteria the completion must cite evidence for. Requires direct human input. A planner loop decomposes the objective into dependency-ordered tasks executed by worker subagents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "objective": {
                            "type": "string",
                            "description": "The overall objective of the run."
                        },
                        "criteria": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 16,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {
                                        "type": "string",
                                        "description": "Short unique handle, e.g. \"report-exists\"."
                                    },
                                    "criterion": {
                                        "type": "string",
                                        "description": "One verifiable acceptance criterion."
                                    }
                                },
                                "required": ["id", "criterion"],
                                "additionalProperties": false
                            }
                        },
                        "limits": {
                            "type": "object",
                            "properties": {
                                "maxRounds": {"type": "integer", "minimum": 1, "description": "Planner-round cap (default 8)."},
                                "maxParallel": {"type": "integer", "minimum": 1, "maximum": 8, "description": "Concurrent workers (default 3)."},
                                "maxWorkers": {"type": "integer", "minimum": 1, "maximum": 64, "description": "Total worker dispatches (default 12)."}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["objective", "criteria"],
                    "additionalProperties": false
                }
            });
            let get = json!({
                "name": "get_orchestration",
                "description": "Read the current orchestration run: contract, board with task statuses and results, budgets and rounds.",
                "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
            });
            let update = json!({
                "name": "update_orchestration",
                "description": "Advance the current orchestration run: submit the next plan version (action plan), complete it with per-criterion evidence (action complete), or report it blocked (action blocked).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "run_id": {"type": "string"},
                        "revision": {"type": "integer", "minimum": 1, "description": "The revision this update applies to."},
                        "action": {
                            "type": "string",
                            "enum": ["plan", "complete", "blocked"],
                            "description": "plan: submit the next plan version. complete: finish, citing evidence per criterion. blocked: report an unresolvable blocker."
                        },
                        "tasks": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 32,
                            "description": "action plan: the full open task list of the new plan version. Task ids are single-use; completed tasks of earlier versions may be depended on.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string", "description": "Short unique handle, e.g. \"crawl\"."},
                                    "description": {"type": "string", "description": "What the worker must do and report."},
                                    "dependsOn": {
                                        "type": "array",
                                        "maxItems": 8,
                                        "items": {"type": "string"},
                                        "description": "Task ids that must complete first."
                                    }
                                },
                                "required": ["id", "description", "dependsOn"],
                                "additionalProperties": false
                            }
                        },
                        "checks": {
                            "type": "array",
                            "description": "action complete: one check per acceptance criterion.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "criterionId": {"type": "string"},
                                    "evidence": {
                                        "type": "array",
                                        "minItems": 1,
                                        "maxItems": 8,
                                        "items": {"type": "string"},
                                        "description": "Concrete pointers: artifact paths, URLs, commands, worker session ids."
                                    }
                                },
                                "required": ["criterionId", "evidence"],
                                "additionalProperties": false
                            }
                        },
                        "blocked_reason": {
                            "type": "object",
                            "description": "action blocked: why the run cannot proceed.",
                            "properties": {
                                "code": {"type": "string", "description": "lower-kebab-case, e.g. needs-input"},
                                "message": {"type": "string"}
                            },
                            "required": ["code", "message"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["run_id", "revision", "action"],
                    "additionalProperties": false
                }
            });
            [create, get, update]
                .into_iter()
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
        .unwrap_or_else(|| panic!("no schema for orchestration tool {name}"))
}

/// `create_orchestration`, `get_orchestration`, `update_orchestration` over
/// one service.
pub fn orchestrator_tools(service: &Arc<OrchestratorService>) -> Vec<Arc<dyn Tool>> {
    let weak = Arc::downgrade(service);
    vec![
        Arc::new(OrchestrationTool {
            service: weak.clone(),
            kind: Kind::Create,
        }),
        Arc::new(OrchestrationTool {
            service: weak.clone(),
            kind: Kind::Get,
        }),
        Arc::new(OrchestrationTool {
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

struct OrchestrationTool {
    service: Weak<OrchestratorService>,
    kind: Kind,
}

fn reject(message: &str, code: &'static str) -> ToolError {
    ToolError::coded("HarnessError", code, message)
}

fn execution(ctx: &ToolContext) -> Result<CallerInfo, ToolError> {
    ctx.caller.clone().ok_or_else(|| {
        reject(
            "orchestration tools require an open model turn",
            "ORCH_TOOL_DRIVER_REQUIRED",
        )
    })
}

/// The caller's current run. Agents the host did not attach read it from
/// their own log, disarmed.
fn current(
    service: &OrchestratorService,
    session: &str,
    caller: &CallerInfo,
) -> Result<Option<RunView>, ToolError> {
    if service.is_attached(session) {
        return service.get(session).map_err(orch_err);
    }
    crate::service::stored_orchestration(&caller.session.events_from(0)).map_err(orch_err)
}

/// A `source.kind: "user"` message in this turn of a root agent.
fn direct_human(caller: &CallerInfo, events: &[SessionEvent]) -> bool {
    caller.root
        && events
            .iter()
            .any(|e| e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "user")
}

/// The turn contains the run's exact reserved round.
fn matching_round(events: &[SessionEvent], view: &RunView) -> bool {
    events.iter().any(|e| {
        let s = &e.data["source"];
        e.kind == kinds::USER_MESSAGE
            && s["kind"] == "orchestration"
            && s["runId"].as_str() == Some(view.state.run.id.as_str())
            && s["revision"] == json!(view.state.run.revision)
            && s["round"] == json!(view.state.rounds_started)
    })
}

fn output(value: Value) -> ToolOutput {
    let mut out = ToolOutput::text(serde_json::to_string(&value).unwrap_or_default());
    out.value = Some(value);
    out
}

fn orch_err(e: OrchestratorError) -> ToolError {
    e.into_tool_error()
}

fn invalid(message: &str) -> ToolError {
    reject(message, "ORCH_TOOL_INVALID_UPDATE")
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

/// The closing-message instruction after a run reported `complete` or
/// `blocked` inside its own round.
pub fn wrapup_text(objective: &str, blocked_reason: Option<&str>) -> String {
    let heading = format!(
        "Objective: {}\n",
        serde_json::to_string(objective).unwrap_or_default()
    );
    match blocked_reason {
        None => format!(
            "<orchestration_complete>\n{heading}The run is marked complete and this autonomous orchestration is ending. Write the closing message to the user now: state the outcome, list which acceptance criteria were met with the evidence cited for each, and point to the concrete results (files, artifacts, worker sessions). Report only what the recorded task results and checks actually establish; when a detail is not recorded, say so instead of inventing it. Note anything the user should review or do next. Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</orchestration_complete>"
        ),
        Some(reason) => format!(
            "<orchestration_blocked>\n{heading}Blocked: {}\nThe run is marked blocked and this autonomous orchestration is ending. Write the closing message to the user now: state what has been completed so far (from the recorded task results), describe the concrete blocking condition and what was tried, and say exactly what you need from the user to continue. Report only what the recorded task results actually establish; when a detail is not recorded, say so instead of inventing it. Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</orchestration_blocked>",
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
            "kind": "tool-orchestration",
            "form": "notice",
            "summary": bound_summary(&format!("{action}: {objective}")),
        })),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

fn parse_criteria(v: &Value) -> Result<Vec<Criterion>, ToolError> {
    let list = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| invalid("criteria must be a non-empty array"))?;
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        let id = c["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_whitespace))
            .ok_or_else(|| invalid("criterion.id must be a short handle without whitespace"))?;
        let criterion = c["criterion"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("criterion.criterion must be a non-empty string"))?;
        out.push(Criterion {
            id: id.to_owned(),
            criterion: criterion.to_owned(),
        });
    }
    Ok(out)
}

fn parse_tasks(v: &Value) -> Result<Vec<TaskSpec>, ToolError> {
    let list = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| invalid("action plan requires a non-empty tasks array"))?;
    let mut out = Vec::with_capacity(list.len());
    for t in list {
        let id = t["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_whitespace))
            .ok_or_else(|| invalid("task.id must be a short handle without whitespace"))?
            .to_owned();
        let description = t["description"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("task.description must be a non-empty string"))?
            .to_owned();
        let depends_on = t["dependsOn"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|d| {
                        d.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("task.dependsOn entries must be strings"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        out.push(TaskSpec {
            id,
            description,
            depends_on,
        });
    }
    Ok(out)
}

fn parse_checks(v: &Value) -> Result<Vec<Check>, ToolError> {
    let list = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| invalid("action complete requires a non-empty checks array"))?;
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        let criterion_id = c["criterionId"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("check.criterionId must be a non-empty string"))?
            .to_owned();
        let evidence = c["evidence"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| invalid("check.evidence must be a non-empty array of pointers"))?
            .iter()
            .map(|e| {
                e.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("check.evidence entries must be non-empty strings"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.push(Check {
            criterion_id,
            evidence,
        });
    }
    Ok(out)
}

#[async_trait]
impl Tool for OrchestrationTool {
    fn schema(&self) -> ToolSchema {
        schema(match self.kind {
            Kind::Get => "get_orchestration",
            Kind::Create => "create_orchestration",
            Kind::Update => "update_orchestration",
        })
    }

    fn describe(&self, args: &Value) -> String {
        match self.kind {
            Kind::Get => "Read current orchestration run".into(),
            Kind::Create => "Create orchestration run".into(),
            Kind::Update => match args.get("action").and_then(Value::as_str) {
                Some(action) => {
                    let mut c = action.chars();
                    match c.next() {
                        Some(f) => format!("{}{} orchestration run", f.to_uppercase(), c.as_str()),
                        None => "Update orchestration run".into(),
                    }
                }
                None => "Update orchestration run".into(),
            },
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let service = self
            .service
            .upgrade()
            .ok_or_else(|| ToolError::Failed("the orchestrator service is gone".into()))?;
        let caller = execution(&ctx)?;
        let session = ctx.session_id.as_str();
        match self.kind {
            Kind::Get => {
                let view = current(&service, session, &caller)?;
                Ok(output(json!({
                    "orchestration": view.map(|v| v.to_json()),
                })))
            }
            Kind::Create => {
                let events = caller.open_turn_events();
                if !direct_human(&caller, &events) {
                    return Err(reject(
                        "this orchestration operation requires a direct human turn on a top-level agent",
                        "ORCH_TOOL_AUTHORITY_REQUIRED",
                    ));
                }
                let objective = args["objective"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("objective must be a non-empty string"))?;
                let contract = ContractInput {
                    objective: objective.to_owned(),
                    criteria: parse_criteria(&args["criteria"])?,
                    limits: service
                        .settings()
                        .parse_limits(&args["limits"])
                        .map_err(orch_err)?,
                };
                let view = service
                    .create(session, contract, Initiator::Agent)
                    .await
                    .map_err(orch_err)?;
                Ok(output(json!({"orchestration": view.to_json()})))
            }
            Kind::Update => self.update(&service, session, &caller, &args).await,
        }
    }
}

impl OrchestrationTool {
    async fn update(
        &self,
        service: &OrchestratorService,
        session: &str,
        caller: &CallerInfo,
        args: &Value,
    ) -> Result<ToolOutput, ToolError> {
        let run_id = args["run_id"].as_str().unwrap_or_default();
        let revision = safe_integer(&args["revision"]).filter(|r| *r >= 1);
        let (Some(revision), true) = (revision, !run_id.is_empty() && run_id == run_id.trim())
        else {
            return Err(invalid(
                "run_id must be non-empty and revision must be a positive safe integer",
            ));
        };
        let r = RunRef {
            id: run_id.to_owned(),
            revision: revision as u64,
        };
        let action = args["action"].as_str().unwrap_or_default();
        let events = caller.open_turn_events();
        let human = direct_human(caller, &events);
        // plan / complete / blocked: direct human input or the run's exact round.
        let round = if human {
            None
        } else {
            match current(service, session, caller)? {
                Some(view) if matching_round(&events, &view) => Some(view),
                _ => {
                    return Err(reject(
                        "plan, complete and blocked require a direct human turn or the current orchestration round",
                        "ORCH_TOOL_AUTHORITY_REQUIRED",
                    ));
                }
            }
        };
        let view = match action {
            "plan" => {
                if args.get("checks").is_some() || args.get("blocked_reason").is_some() {
                    return Err(invalid(
                        "checks belongs to action complete; blocked_reason belongs to action blocked",
                    ));
                }
                let tasks = parse_tasks(&args["tasks"])?;
                service
                    .plan(session, &r, tasks, Initiator::Agent)
                    .await
                    .map_err(orch_err)?
            }
            "complete" => {
                if args.get("tasks").is_some() || args.get("blocked_reason").is_some() {
                    return Err(invalid(
                        "tasks belongs to action plan; blocked_reason belongs to action blocked",
                    ));
                }
                let checks = parse_checks(&args["checks"])?;
                service
                    .complete(session, &r, checks, Initiator::Agent)
                    .await
                    .map_err(orch_err)?
            }
            "blocked" => {
                if args.get("tasks").is_some() || args.get("checks").is_some() {
                    return Err(invalid(
                        "tasks belongs to action plan; checks belongs to action complete",
                    ));
                }
                let reason = args
                    .get("blocked_reason")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| {
                        invalid(
                            "blocked_reason with code and message is required with action blocked",
                        )
                    })?;
                let code = reason["code"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("blocked_reason.code must be a non-empty string"))?;
                let message = reason["message"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("blocked_reason.message must be a non-empty string"))?;
                let blocked_after = service.settings().blocked_after_consecutive_rounds;
                if let Some(view) = &round
                    && view.state.rounds_started < blocked_after
                {
                    return Err(reject(
                        &format!(
                            "blocked requires at least {blocked_after} consecutive rounds; current round is {}",
                            view.state.rounds_started
                        ),
                        "ORCH_TOOL_BLOCK_THRESHOLD",
                    ));
                }
                service
                    .block(session, &r, code, message, Initiator::Agent)
                    .await
                    .map_err(orch_err)?
            }
            _ => return Err(invalid("action must be plan, complete or blocked")),
        };
        let mut out = output(json!({"orchestration": view.to_json()}));
        if round.is_some() {
            let blocked = (action == "blocked").then(|| {
                let reason = args["blocked_reason"].as_object();
                reason
                    .and_then(|r| r["message"].as_str())
                    .unwrap_or_default()
                    .to_owned()
            });
            out.contexts.push(wrapup_message(
                action,
                &view.state.run.objective,
                blocked.as_deref(),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapup_names_outcome_and_evidence_duty() {
        let complete = wrapup_text("Ship", None);
        assert!(complete.starts_with("<orchestration_complete>"));
        assert!(complete.contains("acceptance criteria were met with the evidence cited"));
        let blocked = wrapup_text("Ship", Some("needs the export"));
        assert!(blocked.starts_with("<orchestration_blocked>"));
        assert!(blocked.contains("Blocked: \"needs the export\""));
    }

    #[test]
    fn parse_inputs_reject_abuse() {
        assert!(parse_criteria(&json!([])).is_err());
        assert!(parse_criteria(&json!([{"id": "a", "criterion": "x"}])).is_ok());
        assert!(parse_tasks(&json!([{"id": "a"}])).is_err());
        assert!(parse_tasks(&json!([{"id": "a", "description": "d", "dependsOn": []}])).is_ok());
        assert!(parse_checks(&json!([{"criterionId": "a", "evidence": []}])).is_err());
    }
}
