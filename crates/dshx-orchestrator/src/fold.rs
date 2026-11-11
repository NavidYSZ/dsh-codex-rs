//! The strict replay fold of durable orchestration changes: one
//! `orchestration/change` event kind whose operations carry the run
//! snapshot (`create`, `plan`, `task`, `complete`, `block`, `pause`,
//! `resume`, `cancel`) plus a `clear` tombstone, and orchestration-round
//! prompts that advance `rounds_started`. A completed or failed task of an
//! earlier plan version stays recorded; only tasks of the current plan
//! version are schedulable.
use std::collections::{HashMap, HashSet};

use dshx_core::SessionEvent;
use serde_json::{Map, Value, json};

/// Session event type of every orchestration mutation.
pub const ORCHESTRATION_CHANGE: &str = "orchestration/change";

/// One acceptance criterion of a run.
pub const MAX_CRITERIA: usize = 16;
/// Tasks one plan version may define.
pub const MAX_PLAN_TASKS: usize = 32;
/// Dependencies one task may declare.
pub const MAX_DEPENDS_ON: usize = 8;
/// Evidence entries one completion check may cite.
pub const MAX_CHECK_EVIDENCE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Active,
    Paused,
    Blocked,
    Complete,
    Cancelled,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Active => "active",
            Phase::Paused => "paused",
            Phase::Blocked => "blocked",
            Phase::Complete => "complete",
            Phase::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Option<Phase> {
        Some(match s {
            "active" => Phase::Active,
            "paused" => Phase::Paused,
            "blocked" => Phase::Blocked,
            "complete" => Phase::Complete,
            "cancelled" => Phase::Cancelled,
            _ => return None,
        })
    }
}

/// Why a run is blocked: a lower-kebab-case `code` and a trimmed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockReason {
    pub code: String,
    pub message: String,
}

/// One acceptance criterion; `id` is the model-facing handle checks cite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub id: String,
    pub criterion: String,
}

/// The enforced execution budgets of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_rounds: u64,
    pub max_parallel: u64,
    pub max_workers: u64,
}

impl Limits {
    pub fn to_json(self) -> Value {
        json!({
            "maxRounds": self.max_rounds,
            "maxParallel": self.max_parallel,
            "maxWorkers": self.max_workers,
        })
    }
}

/// One revision of an orchestration run (the contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub revision: u64,
    pub objective: String,
    pub criteria: Vec<Criterion>,
    pub limits: Limits,
    pub phase: Phase,
    /// Only on `blocked`.
    pub blocked_reason: Option<BlockReason>,
}

impl Run {
    /// The snapshot as logged, in key order.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("revision".into(), self.revision.into());
        m.insert("objective".into(), self.objective.clone().into());
        m.insert(
            "criteria".into(),
            Value::Array(
                self.criteria
                    .iter()
                    .map(|c| json!({"id": c.id, "criterion": c.criterion}))
                    .collect(),
            ),
        );
        m.insert("limits".into(), self.limits.to_json());
        m.insert("phase".into(), self.phase.as_str().into());
        if let Some(r) = &self.blocked_reason {
            m.insert(
                "blockedReason".into(),
                json!({"code": r.code, "message": r.message}),
            );
        }
        Value::Object(m)
    }
}

/// A task as a plan defines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub id: String,
    pub description: String,
    pub depends_on: Vec<String>,
}

impl TaskSpec {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "description": self.description,
            "dependsOn": self.depends_on,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<TaskStatus> {
        Some(match s {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            _ => return None,
        })
    }
}

/// One task's durable record across plan versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: TaskStatus,
    /// The plan version that defined this task.
    pub plan_version: u64,
    /// The worker child session (set on `running`).
    pub child_id: Option<String>,
    /// The worker's reported outcome (set on `completed`/`failed`).
    pub summary: Option<String>,
    pub stop_reason: Option<String>,
}

/// One criterion's completion check: cited evidence pointers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub criterion_id: String,
    pub evidence: Vec<String>,
}

/// The current run with its derived state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunState {
    pub run: Run,
    /// 0 until the first `plan`.
    pub plan_version: u64,
    /// Every task every plan version defined, by id.
    pub tasks: HashMap<String, TaskRecord>,
    /// Task ids already used (unique across plan versions).
    pub seen_tasks: HashSet<String>,
    /// The current plan's task order.
    pub plan_order: Vec<String>,
    /// Every task in first-seen order (history before the current plan).
    pub task_seq: Vec<String>,
    /// Count of `task/running` changes (the worker budget spent).
    pub workers_dispatched: u64,
    /// The completion checks (set by `complete`).
    pub checks: Option<Vec<Check>>,
    pub rounds_started: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

impl RunState {
    /// Current-plan tasks in plan order, then settled history in first-seen
    /// order: the display and dispatch order.
    pub fn ordered_tasks(&self) -> Vec<&TaskRecord> {
        let mut out = self
            .plan_order
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .collect::<Vec<_>>();
        for id in &self.task_seq {
            if !self.plan_order.contains(id)
                && let Some(t) = self.tasks.get(id)
            {
                out.push(t);
            }
        }
        out
    }

    /// Pending tasks of the current plan whose dependencies are all completed.
    pub fn ready_tasks(&self) -> Vec<String> {
        self.plan_order
            .iter()
            .filter(|id| {
                self.tasks.get(*id).as_ref().is_some_and(|t| {
                    t.status == TaskStatus::Pending
                        && t.depends_on.iter().all(|d| {
                            self.tasks
                                .get(d)
                                .is_some_and(|dep| dep.status == TaskStatus::Completed)
                        })
                })
            })
            .cloned()
            .collect()
    }
}

/// A decoded `orchestration/change` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunChange {
    Create {
        run: Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Plan {
        run: Run,
        plan_version: u64,
        tasks: Vec<TaskSpec>,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Task {
        run: Run,
        task: TaskUpdate,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Complete {
        run: Run,
        checks: Vec<Check>,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Block {
        run: Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Pause {
        run: Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Resume {
        run: Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Cancel {
        run: Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
    Clear {
        id: String,
        revision: u64,
        cleared_at: u64,
    },
}

/// A service-recorded task edge: dispatch (`running`) or settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskUpdate {
    pub id: String,
    pub status: TaskStatus,
    pub child_id: Option<String>,
    pub summary: Option<String>,
    pub stop_reason: Option<String>,
}

impl RunChange {
    /// The payload as logged, in key order.
    pub fn to_json(&self) -> Value {
        let counters = |m: &mut Map<String, Value>, rounds: u64, created: u64, updated: u64| {
            m.insert("roundsStarted".into(), rounds.into());
            m.insert("createdAt".into(), created.into());
            m.insert("updatedAt".into(), updated.into());
        };
        match self {
            RunChange::Create {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let mut m = Map::new();
                m.insert("kind".into(), ORCHESTRATION_CHANGE.into());
                m.insert("version".into(), 1.into());
                m.insert("operation".into(), "create".into());
                m.insert("run".into(), run.to_json());
                counters(&mut m, *rounds_started, *created_at, *updated_at);
                Value::Object(m)
            }
            RunChange::Plan {
                run,
                plan_version,
                tasks,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let mut m = Map::new();
                m.insert("kind".into(), ORCHESTRATION_CHANGE.into());
                m.insert("version".into(), 1.into());
                m.insert("operation".into(), "plan".into());
                m.insert("run".into(), run.to_json());
                m.insert(
                    "plan".into(),
                    json!({
                        "version": plan_version,
                        "tasks": tasks.iter().map(TaskSpec::to_json).collect::<Vec<_>>(),
                    }),
                );
                counters(&mut m, *rounds_started, *created_at, *updated_at);
                Value::Object(m)
            }
            RunChange::Task {
                run,
                task,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let mut t = Map::new();
                t.insert("id".into(), task.id.clone().into());
                t.insert("status".into(), task.status.as_str().into());
                if let Some(c) = &task.child_id {
                    t.insert("childId".into(), c.clone().into());
                }
                if let Some(s) = &task.summary {
                    t.insert("summary".into(), s.clone().into());
                }
                if let Some(r) = &task.stop_reason {
                    t.insert("stopReason".into(), r.clone().into());
                }
                let mut m = Map::new();
                m.insert("kind".into(), ORCHESTRATION_CHANGE.into());
                m.insert("version".into(), 1.into());
                m.insert("operation".into(), "task".into());
                m.insert("run".into(), run.to_json());
                m.insert("task".into(), Value::Object(t));
                counters(&mut m, *rounds_started, *created_at, *updated_at);
                Value::Object(m)
            }
            RunChange::Complete {
                run,
                checks,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let mut m = Map::new();
                m.insert("kind".into(), ORCHESTRATION_CHANGE.into());
                m.insert("version".into(), 1.into());
                m.insert("operation".into(), "complete".into());
                m.insert("run".into(), run.to_json());
                m.insert(
                    "checks".into(),
                    Value::Array(
                        checks
                            .iter()
                            .map(|c| json!({"criterionId": c.criterion_id, "evidence": c.evidence}))
                            .collect(),
                    ),
                );
                counters(&mut m, *rounds_started, *created_at, *updated_at);
                Value::Object(m)
            }
            RunChange::Block {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => simple("block", run, *rounds_started, *created_at, *updated_at),
            RunChange::Pause {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => simple("pause", run, *rounds_started, *created_at, *updated_at),
            RunChange::Resume {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => simple("resume", run, *rounds_started, *created_at, *updated_at),
            RunChange::Cancel {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => simple("cancel", run, *rounds_started, *created_at, *updated_at),
            RunChange::Clear {
                id,
                revision,
                cleared_at,
            } => json!({
                "kind": ORCHESTRATION_CHANGE,
                "version": 1,
                "operation": "clear",
                "cleared": {"id": id, "revision": revision},
                "clearedAt": cleared_at,
            }),
        }
    }

    pub fn operation(&self) -> &'static str {
        match self {
            RunChange::Create { .. } => "create",
            RunChange::Plan { .. } => "plan",
            RunChange::Task { .. } => "task",
            RunChange::Complete { .. } => "complete",
            RunChange::Block { .. } => "block",
            RunChange::Pause { .. } => "pause",
            RunChange::Resume { .. } => "resume",
            RunChange::Cancel { .. } => "cancel",
            RunChange::Clear { .. } => "clear",
        }
    }
}

fn simple(
    operation: &str,
    run: &Run,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
) -> Value {
    json!({
        "kind": ORCHESTRATION_CHANGE,
        "version": 1,
        "operation": operation,
        "run": run.to_json(),
        "roundsStarted": rounds_started,
        "createdAt": created_at,
        "updatedAt": updated_at,
    })
}

type Result<T> = std::result::Result<T, String>;

fn keys(v: &Map<String, Value>) -> String {
    let mut k: Vec<&str> = v.keys().map(String::as_str).collect();
    k.sort_unstable();
    k.join(",")
}

/// A JS safe integer (`Number.isSafeInteger`).
pub(crate) fn safe_integer(v: &Value) -> Option<i64> {
    const MAX: f64 = 9_007_199_254_740_991.0;
    let f = v.as_f64()?;
    (f.fract() == 0.0 && f.abs() <= MAX).then_some(f as i64)
}

/// `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$`
pub(crate) fn kebab(code: &str) -> bool {
    let mut parts = code.split('-');
    let Some(first) = parts.next() else {
        return false;
    };
    let mut chars = first.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && parts.all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// A short model-chosen handle: non-empty, ≤ 64 chars, no whitespace.
fn handle(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_whitespace)
}

fn bounded(v: &Value, field: &str, max_units: usize) -> Result<String> {
    let s = v
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.trim() == *s)
        .ok_or_else(|| format!("orchestration change {field} must be non-empty and normalized"))?;
    if s.encode_utf16().count() > max_units {
        return Err(format!(
            "orchestration change {field} must be at most {max_units} UTF-16 units"
        ));
    }
    Ok(s.to_owned())
}

fn positive(v: &Value, field: &str) -> Result<u64> {
    match safe_integer(v) {
        Some(n) if n >= 1 => Ok(n as u64),
        _ => Err(format!(
            "orchestration change {field} must be a positive safe integer"
        )),
    }
}

fn bounded_int(v: &Value, field: &str, max: i64) -> Result<u64> {
    match safe_integer(v) {
        Some(n) if (1..=max).contains(&n) => Ok(n as u64),
        _ => Err(format!(
            "orchestration change {field} must be an integer between 1 and {max}"
        )),
    }
}

fn non_negative(v: &Value, field: &str) -> Result<u64> {
    match safe_integer(v) {
        Some(n) if n >= 0 => Ok(n as u64),
        _ => Err(format!(
            "orchestration change {field} must be a non-negative safe integer"
        )),
    }
}

fn decode_block_reason(v: &Value) -> Result<BlockReason> {
    let Some(m) = v.as_object().filter(|m| keys(m) == "code,message") else {
        return Err(
            "orchestration change blockedReason must have exactly code and message fields".into(),
        );
    };
    let code = m["code"]
        .as_str()
        .filter(|c| kebab(c))
        .ok_or("orchestration change blockedReason.code must be lower-kebab-case".to_owned())?;
    let message = m["message"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.trim() == *s)
        .ok_or(
            "orchestration change blockedReason.message must be non-empty and normalized"
                .to_owned(),
        )?;
    Ok(BlockReason {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

fn decode_limits(v: &Value) -> Result<Limits> {
    let Some(m) = v
        .as_object()
        .filter(|m| keys(m) == "maxParallel,maxRounds,maxWorkers")
    else {
        return Err(
            "orchestration change run.limits must have exactly maxRounds, maxParallel and maxWorkers fields"
                .into(),
        );
    };
    Ok(Limits {
        max_rounds: bounded_int(&m["maxRounds"], "run.limits.maxRounds", 10_000)?,
        max_parallel: bounded_int(&m["maxParallel"], "run.limits.maxParallel", 8)?,
        max_workers: bounded_int(&m["maxWorkers"], "run.limits.maxWorkers", 64)?,
    })
}

fn decode_criteria(v: &Value) -> Result<Vec<Criterion>> {
    let list = v
        .as_array()
        .filter(|a| (1..=MAX_CRITERIA).contains(&a.len()))
        .ok_or_else(|| {
            format!("orchestration change run.criteria must be 1 to {MAX_CRITERIA} entries")
        })?;
    let mut out = Vec::with_capacity(list.len());
    let mut seen = HashSet::new();
    for c in list {
        let Some(m) = c.as_object().filter(|m| keys(m) == "criterion,id") else {
            return Err(
                "orchestration change criterion must have exactly id and criterion fields".into(),
            );
        };
        let id = m["id"].as_str().filter(|s| handle(s)).ok_or_else(|| {
            "orchestration change criterion.id must be a short handle without whitespace".to_owned()
        })?;
        if !seen.insert(id.to_owned()) {
            return Err("orchestration change criterion ids must be unique".into());
        }
        out.push(Criterion {
            id: id.to_owned(),
            criterion: bounded(&m["criterion"], "criterion.criterion", 1000)?,
        });
    }
    Ok(out)
}

fn decode_run(v: &Value) -> Result<Run> {
    let m = v
        .as_object()
        .ok_or("orchestration change run must be a record")?;
    let id = m
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("orchestration change run.id must be a non-empty string")?;
    let objective = m
        .get("objective")
        .map(|o| bounded(o, "run.objective", 4000))
        .transpose()?
        .ok_or("orchestration change run.objective is required")?;
    let phase = m
        .get("phase")
        .and_then(Value::as_str)
        .and_then(Phase::parse)
        .ok_or("orchestration change run.phase is invalid")?;
    let criteria = m
        .get("criteria")
        .map(decode_criteria)
        .transpose()?
        .ok_or("orchestration change run.criteria is required")?;
    let limits = m
        .get("limits")
        .map(decode_limits)
        .transpose()?
        .ok_or("orchestration change run.limits is required")?;
    let expected = if phase == Phase::Blocked {
        "blockedReason,criteria,id,limits,objective,phase,revision"
    } else {
        "criteria,id,limits,objective,phase,revision"
    };
    if keys(m) != expected {
        return Err(format!(
            "orchestration change run for phase {} must have exactly {expected} fields",
            phase.as_str()
        ));
    }
    Ok(Run {
        id: id.to_owned(),
        revision: positive(&m["revision"], "run.revision")?,
        objective,
        criteria,
        limits,
        phase,
        blocked_reason: if phase == Phase::Blocked {
            Some(decode_block_reason(&m["blockedReason"])?)
        } else {
            None
        },
    })
}

fn decode_plan(v: &Value) -> Result<(u64, Vec<TaskSpec>)> {
    let Some(m) = v.as_object().filter(|m| keys(m) == "tasks,version") else {
        return Err("orchestration change plan must have exactly version and tasks fields".into());
    };
    let version = positive(&m["version"], "plan.version")?;
    let list = m["tasks"]
        .as_array()
        .filter(|a| (1..=MAX_PLAN_TASKS).contains(&a.len()))
        .ok_or_else(|| {
            format!("orchestration change plan.tasks must be 1 to {MAX_PLAN_TASKS} entries")
        })?;
    let mut tasks = Vec::with_capacity(list.len());
    for t in list {
        let Some(tm) = t
            .as_object()
            .filter(|tm| keys(tm) == "dependsOn,description,id")
        else {
            return Err(
                "orchestration change task must have exactly id, description and dependsOn fields"
                    .into(),
            );
        };
        let id = tm["id"].as_str().filter(|s| handle(s)).ok_or_else(|| {
            "orchestration change task.id must be a short handle without whitespace".to_owned()
        })?;
        let depends_on = tm["dependsOn"]
            .as_array()
            .filter(|a| a.len() <= MAX_DEPENDS_ON)
            .ok_or_else(|| {
                format!(
                    "orchestration change task.dependsOn must be at most {MAX_DEPENDS_ON} entries"
                )
            })?
            .iter()
            .map(|d| {
                d.as_str()
                    .filter(|s| handle(s))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        "orchestration change task.dependsOn entries must be short handles"
                            .to_owned()
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        tasks.push(TaskSpec {
            id: id.to_owned(),
            description: bounded(&tm["description"], "task.description", 4000)?,
            depends_on,
        });
    }
    Ok((version, tasks))
}

fn decode_task_update(v: &Value) -> Result<TaskUpdate> {
    let m = v
        .as_object()
        .ok_or("orchestration change task must be a record")?;
    let id = m
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| handle(s))
        .ok_or("orchestration change task.id must be a short handle without whitespace")?;
    let status = m
        .get("status")
        .and_then(Value::as_str)
        .and_then(TaskStatus::parse)
        .ok_or("orchestration change task.status is invalid")?;
    let expected = match (
        status,
        m.contains_key("childId"),
        m.contains_key("summary"),
        m.contains_key("stopReason"),
    ) {
        (TaskStatus::Running, true, false, false) => "childId,id,status",
        (TaskStatus::Completed, false, true, false) => "id,status,summary",
        (TaskStatus::Failed, false, true, true) => "id,status,stopReason,summary",
        _ => {
            return Err(
                "orchestration change task fields do not match its status (running carries childId; completed carries summary; failed carries summary and stopReason)"
                    .into(),
            );
        }
    };
    if keys(m) != expected {
        return Err(format!(
            "orchestration change task for status {} must have exactly {expected} fields",
            status.as_str()
        ));
    }
    let child_id = match status {
        TaskStatus::Running => Some(
            m["childId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("orchestration change task.childId must be non-empty")?
                .to_owned(),
        ),
        _ => None,
    };
    let summary = match status {
        TaskStatus::Completed | TaskStatus::Failed => {
            Some(bounded(&m["summary"], "task.summary", 2000)?)
        }
        _ => None,
    };
    let stop_reason = match status {
        TaskStatus::Failed => Some(
            m["stopReason"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("orchestration change task.stopReason must be non-empty")?
                .to_owned(),
        ),
        _ => None,
    };
    Ok(TaskUpdate {
        id: id.to_owned(),
        status,
        child_id,
        summary,
        stop_reason,
    })
}

fn decode_checks(v: &Value, criteria: &[Criterion]) -> Result<Vec<Check>> {
    let list = v
        .as_array()
        .ok_or("orchestration change checks must be an array")?;
    let mut out = Vec::with_capacity(list.len());
    let mut seen = HashSet::new();
    for c in list {
        let Some(m) = c.as_object().filter(|m| keys(m) == "criterionId,evidence") else {
            return Err(
                "orchestration change check must have exactly criterionId and evidence fields"
                    .into(),
            );
        };
        let id = m["criterionId"]
            .as_str()
            .filter(|s| handle(s))
            .ok_or_else(|| {
                "orchestration change check.criterionId must be a short handle".to_owned()
            })?;
        if !criteria.iter().any(|c| c.id == id) {
            return Err(format!(
                "orchestration change check cites unknown criterion \"{id}\""
            ));
        }
        if !seen.insert(id.to_owned()) {
            return Err("orchestration change checks must cite each criterion once".into());
        }
        let evidence = m["evidence"]
            .as_array()
            .filter(|a| (1..=MAX_CHECK_EVIDENCE).contains(&a.len()))
            .ok_or_else(|| {
                format!(
                    "orchestration change check.evidence must be 1 to {MAX_CHECK_EVIDENCE} entries"
                )
            })?
            .iter()
            .map(|e| bounded(e, "check.evidence entry", 480))
            .collect::<Result<Vec<_>>>()?;
        out.push(Check {
            criterion_id: id.to_owned(),
            evidence,
        });
    }
    if seen.len() != criteria.len() {
        return Err(
            "orchestration change checks must cover every acceptance criterion exactly once".into(),
        );
    }
    Ok(out)
}

/// `decodeOrchestrationChange`: `None` for a value of another kind.
pub fn decode_change(v: &Value) -> Result<Option<RunChange>> {
    let Some(m) = v
        .as_object()
        .filter(|m| m.get("kind") == Some(&json!(ORCHESTRATION_CHANGE)))
    else {
        return Ok(None);
    };
    if m.get("version") != Some(&json!(1)) {
        return Err(format!(
            "unsupported orchestration change version {}",
            m.get("version")
                .map_or("undefined".into(), |v| v.to_string())
        ));
    }
    let operation = m
        .get("operation")
        .and_then(Value::as_str)
        .filter(|o| {
            [
                "create", "plan", "task", "complete", "block", "pause", "resume", "cancel", "clear",
            ]
            .contains(o)
        })
        .ok_or("orchestration change operation is invalid")?;
    if operation == "clear" {
        if keys(m) != "cleared,clearedAt,kind,operation,version" {
            return Err(
                "orchestration clear change must have exactly cleared,clearedAt,kind,operation,version fields"
                    .into(),
            );
        }
        let Some(r) = m["cleared"]
            .as_object()
            .filter(|r| keys(r) == "id,revision")
        else {
            return Err(
                "orchestration clear tombstone must have exactly id and revision fields".into(),
            );
        };
        let id = r["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("orchestration clear tombstone id must be a non-empty string")?;
        return Ok(Some(RunChange::Clear {
            id: id.to_owned(),
            revision: positive(&r["revision"], "cleared.revision")?,
            cleared_at: non_negative(&m["clearedAt"], "clearedAt")?,
        }));
    }
    let base = match operation {
        "plan" => "createdAt,kind,operation,plan,roundsStarted,run,updatedAt,version",
        "task" => "createdAt,kind,operation,roundsStarted,run,task,updatedAt,version",
        "complete" => "checks,createdAt,kind,operation,roundsStarted,run,updatedAt,version",
        _ => "createdAt,kind,operation,roundsStarted,run,updatedAt,version",
    };
    if keys(m) != base {
        return Err(format!(
            "orchestration {operation} change must have exactly {base} fields"
        ));
    }
    let run = decode_run(&m["run"])?;
    let rounds_started = non_negative(&m["roundsStarted"], "roundsStarted")?;
    let created_at = non_negative(&m["createdAt"], "createdAt")?;
    let updated_at = non_negative(&m["updatedAt"], "updatedAt")?;
    if updated_at < created_at {
        return Err("orchestration change updatedAt cannot precede createdAt".into());
    }
    Ok(Some(match operation {
        "create" => RunChange::Create {
            run,
            rounds_started,
            created_at,
            updated_at,
        },
        "plan" => {
            let (plan_version, tasks) = decode_plan(&m["plan"])?;
            RunChange::Plan {
                run,
                plan_version,
                tasks,
                rounds_started,
                created_at,
                updated_at,
            }
        }
        "task" => RunChange::Task {
            run,
            task: decode_task_update(&m["task"])?,
            rounds_started,
            created_at,
            updated_at,
        },
        "complete" => {
            let checks = decode_checks(&m["checks"], &run.criteria)?;
            RunChange::Complete {
                run,
                checks,
                rounds_started,
                created_at,
                updated_at,
            }
        }
        "block" => RunChange::Block {
            run,
            rounds_started,
            created_at,
            updated_at,
        },
        "pause" => RunChange::Pause {
            run,
            rounds_started,
            created_at,
            updated_at,
        },
        "resume" => RunChange::Resume {
            run,
            rounds_started,
            created_at,
            updated_at,
        },
        "cancel" => RunChange::Cancel {
            run,
            rounds_started,
            created_at,
            updated_at,
        },
        _ => return Err("orchestration change operation is invalid".into()),
    }))
}

/// An orchestration-round prompt's `{runId, revision, round}`.
pub(crate) fn round_source(source: &Value) -> Result<Option<(String, u64, u64)>> {
    if source.get("kind").and_then(Value::as_str) != Some("orchestration") {
        return Ok(None);
    }
    let id = source["runId"].as_str().filter(|s| !s.is_empty());
    let revision = safe_integer(&source["revision"]).filter(|n| *n >= 1);
    let round = safe_integer(&source["round"]).filter(|n| *n >= 1);
    match (id, revision, round) {
        (Some(id), Some(rev), Some(round)) => Ok(Some((id.to_owned(), rev as u64, round as u64))),
        _ => Err("orchestration message source is invalid".into()),
    }
}

/// Whether `id` is a well-formed run id.
pub(crate) fn run_id(id: &str) -> bool {
    id.starts_with("orch-") && id.len() > 5
}

/// The durable orchestration state of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestratorFold {
    pub current: Option<RunState>,
    /// Run ids ever created (a create cannot reuse one).
    pub seen: HashSet<String>,
    /// The first invalid owned event; later events are ignored.
    pub failure: Option<String>,
    /// Events folded so far.
    pub folded: u64,
}

impl OrchestratorFold {
    pub fn from_events(events: &[SessionEvent]) -> OrchestratorFold {
        let mut fold = OrchestratorFold::default();
        fold.extend(events);
        fold
    }

    /// Folds events with `seq >= self.folded`, in order.
    pub fn extend(&mut self, events: &[SessionEvent]) {
        for e in events {
            if e.seq < self.folded {
                continue;
            }
            self.folded = e.seq + 1;
            if self.failure.is_some() {
                continue;
            }
            if let Err(message) = self.apply(e) {
                self.failure = Some(format!(
                    "orchestration replay failed at session event {}: {message}",
                    e.seq
                ));
            }
        }
    }

    /// The current run, or the retained replay failure.
    pub fn state(&self) -> std::result::Result<Option<&RunState>, String> {
        match &self.failure {
            Some(f) => Err(f.clone()),
            None => Ok(self.current.as_ref()),
        }
    }

    fn apply(&mut self, e: &SessionEvent) -> Result<()> {
        match e.kind.as_str() {
            ORCHESTRATION_CHANGE => {
                let change = decode_change(&e.data)?.ok_or_else(|| {
                    format!(
                        "orchestration change at session event {} has an invalid kind",
                        e.seq
                    )
                })?;
                self.apply_change(&change)
            }
            dshx_core::kinds::USER_MESSAGE => {
                let Some((id, revision, round)) = round_source(&e.data["source"])? else {
                    return Ok(());
                };
                let ok = self.current.as_ref().is_some_and(|c| {
                    c.run.phase == Phase::Active
                        && c.run.id == id
                        && c.run.revision == revision
                        && round == c.rounds_started + 1
                        && round <= c.run.limits.max_rounds
                });
                if !ok {
                    return Err(format!(
                        "orchestration round at session event {} is not the next admitted round of the active run",
                        e.seq
                    ));
                }
                if let Some(c) = self.current.as_mut() {
                    c.rounds_started = round;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Applies one decoded change with full transition validation.
    pub fn apply_change(&mut self, change: &RunChange) -> Result<()> {
        match change {
            RunChange::Clear {
                id,
                revision,
                cleared_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration clear requires a current run")?;
                if *id != current.run.id || *revision != current.run.revision + 1 {
                    return Err(
                        "orchestration clear must advance the current run by one revision".into(),
                    );
                }
                if *cleared_at < current.updated_at {
                    return Err(
                        "orchestration clear timestamp cannot precede the current run update"
                            .into(),
                    );
                }
                self.current = None;
                Ok(())
            }
            RunChange::Create {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let replaceable = self.current.as_ref().is_none_or(|c| {
                    c.run.phase == Phase::Complete || c.run.phase == Phase::Cancelled
                });
                if !run_id(&run.id)
                    || run.revision != 1
                    || run.phase != Phase::Active
                    || *rounds_started != 0
                    || !replaceable
                    || self.seen.contains(&run.id)
                {
                    return Err(
                        "orchestration create requires a fresh active revision-one run with zero rounds"
                            .into(),
                    );
                }
                self.seen.insert(run.id.clone());
                self.current = Some(RunState {
                    run: run.clone(),
                    plan_version: 0,
                    tasks: HashMap::new(),
                    seen_tasks: HashSet::new(),
                    plan_order: Vec::new(),
                    task_seq: Vec::new(),
                    workers_dispatched: 0,
                    checks: None,
                    rounds_started: *rounds_started,
                    created_at: *created_at,
                    updated_at: *updated_at,
                });
                Ok(())
            }
            RunChange::Plan {
                run,
                plan_version,
                tasks,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration plan requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if next.phase != Phase::Active {
                    return Err(format!(
                        "orchestration plan requires the active phase, not {}",
                        next.phase.as_str()
                    ));
                }
                if *plan_version != current.plan_version + 1 {
                    return Err(format!(
                        "orchestration plan must advance the plan version to {}, not {plan_version}",
                        current.plan_version + 1
                    ));
                }
                let completed: HashSet<&str> = current
                    .tasks
                    .values()
                    .filter(|t| t.status == TaskStatus::Completed)
                    .map(|t| t.id.as_str())
                    .collect();
                let mut fresh = HashSet::new();
                for t in tasks {
                    if !current.seen_tasks.contains(&t.id) {
                        if !fresh.insert(t.id.as_str()) {
                            return Err(format!("orchestration plan repeats task id \"{}\"", t.id));
                        }
                    } else if current
                        .tasks
                        .get(&t.id)
                        .is_some_and(|old| old.status != TaskStatus::Pending)
                    {
                        return Err(format!(
                            "orchestration plan cannot redefine task \"{}\" that already ran or settled",
                            t.id
                        ));
                    } else {
                        return Err(format!(
                            "orchestration plan cannot reuse task id \"{}\" from an earlier version",
                            t.id
                        ));
                    }
                }
                for t in tasks {
                    let mut deps = HashSet::new();
                    for d in &t.depends_on {
                        if d == &t.id {
                            return Err(format!(
                                "orchestration task \"{}\" cannot depend on itself",
                                t.id
                            ));
                        }
                        if !deps.insert(d.as_str()) {
                            return Err(format!(
                                "orchestration task \"{}\" repeats dependency \"{d}\"",
                                t.id
                            ));
                        }
                        if !fresh.contains(d.as_str()) && !completed.contains(d.as_str()) {
                            return Err(format!(
                                "orchestration task \"{}\" depends on unknown or unsettled task \"{d}\"",
                                t.id
                            ));
                        }
                    }
                }
                if cyclic(tasks) {
                    return Err("orchestration plan task dependencies must be acyclic".into());
                }
                let mut state = self.next_state(next, current);
                state.plan_version = *plan_version;
                state.plan_order = tasks.iter().map(|t| t.id.clone()).collect();
                for t in tasks {
                    state.seen_tasks.insert(t.id.clone());
                    state.task_seq.push(t.id.clone());
                    state.tasks.insert(
                        t.id.clone(),
                        TaskRecord {
                            id: t.id.clone(),
                            description: t.description.clone(),
                            depends_on: t.depends_on.clone(),
                            status: TaskStatus::Pending,
                            plan_version: *plan_version,
                            child_id: None,
                            summary: None,
                            stop_reason: None,
                        },
                    );
                }
                self.current = Some(state);
                Ok(())
            }
            RunChange::Task {
                run,
                task,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration task change requires a current run")?;
                // Settlements are mechanical facts: they are recorded in any
                // phase, including after a pause or cancel stopped the workers.
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                let record =
                    current.tasks.get(&task.id).cloned().ok_or_else(|| {
                        format!("orchestration task \"{}\" is not planned", task.id)
                    })?;
                let valid = match task.status {
                    TaskStatus::Pending => false,
                    TaskStatus::Running => {
                        record.status == TaskStatus::Pending
                            && record.plan_version == current.plan_version
                            && current.run.phase == Phase::Active
                            && current.workers_dispatched < current.run.limits.max_workers
                            && (current
                                .tasks
                                .values()
                                .filter(|t| t.status == TaskStatus::Running)
                                .count() as u64)
                                < current.run.limits.max_parallel
                            && record.depends_on.iter().all(|id| {
                                current
                                    .tasks
                                    .get(id)
                                    .is_some_and(|t| t.status == TaskStatus::Completed)
                            })
                    }
                    // A settlement also arrives from a refused spawn (never
                    // dispatched) and after the run was paused or cancelled;
                    // only `failed` may skip the dispatch edge.
                    TaskStatus::Completed => record.status == TaskStatus::Running,
                    TaskStatus::Failed => {
                        matches!(record.status, TaskStatus::Pending | TaskStatus::Running)
                    }
                };
                if !valid {
                    return Err(format!(
                        "orchestration task \"{}\" cannot move from {} to {}",
                        task.id,
                        record.status.as_str(),
                        task.status.as_str()
                    ));
                }
                let mut state = self.next_state(next, current);
                let record = state.tasks.get_mut(&task.id).expect("record exists");
                match task.status {
                    TaskStatus::Running => {
                        record.status = TaskStatus::Running;
                        record.child_id = task.child_id.clone();
                        state.workers_dispatched += 1;
                    }
                    TaskStatus::Completed => {
                        record.status = TaskStatus::Completed;
                        record.summary = task.summary.clone();
                    }
                    TaskStatus::Failed => {
                        record.status = TaskStatus::Failed;
                        record.summary = task.summary.clone();
                        record.stop_reason = task.stop_reason.clone();
                    }
                    TaskStatus::Pending => unreachable!("validated above"),
                }
                self.current = Some(state);
                Ok(())
            }
            RunChange::Complete {
                run,
                checks,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration complete requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if current.run.phase != Phase::Active || next.phase != Phase::Complete {
                    return Err("orchestration complete has an invalid phase transition".into());
                }
                let mut state = self.next_state(next, current);
                state.checks = Some(checks.clone());
                self.current = Some(state);
                Ok(())
            }
            RunChange::Block {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration block requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if current.run.phase != Phase::Active || next.phase != Phase::Blocked {
                    return Err("orchestration block has an invalid phase transition".into());
                }
                // `preserved` rejects an inherited blocked_reason on non-block
                // snapshots; a blocked block change must carry one.
                if next.blocked_reason.is_none() {
                    return Err("orchestration block requires a blockedReason".into());
                }
                let state = self.next_state(next, current);
                self.current = Some(state);
                Ok(())
            }
            RunChange::Pause {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration pause requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if current.run.phase != Phase::Active || next.phase != Phase::Paused {
                    return Err("orchestration pause has an invalid phase transition".into());
                }
                let state = self.next_state(next, current);
                self.current = Some(state);
                Ok(())
            }
            RunChange::Resume {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration resume requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if !matches!(
                    current.run.phase,
                    Phase::Active | Phase::Paused | Phase::Blocked
                ) || next.phase != Phase::Active
                {
                    return Err("orchestration resume has an invalid phase transition".into());
                }
                if current.rounds_started >= next.limits.max_rounds {
                    return Err(
                        "orchestration resume cannot exceed the configured round limit".into(),
                    );
                }
                let state = self.next_state(next, current);
                self.current = Some(state);
                Ok(())
            }
            RunChange::Cancel {
                run,
                rounds_started,
                created_at,
                updated_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("orchestration cancel requires a current run")?;
                let next =
                    self.preserved(current, run, *rounds_started, *created_at, *updated_at)?;
                if !matches!(
                    current.run.phase,
                    Phase::Active | Phase::Paused | Phase::Blocked
                ) || next.phase != Phase::Cancelled
                {
                    return Err("orchestration cancel has an invalid phase transition".into());
                }
                let state = self.next_state(next, current);
                self.current = Some(state);
                Ok(())
            }
        }
    }

    /// The next run snapshot must keep the contract and advance one revision.
    fn preserved(
        &self,
        current: &RunState,
        next: &Run,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    ) -> Result<Run> {
        let cur = &current.run;
        if next.id != cur.id || next.revision != cur.revision + 1 {
            return Err("orchestration change must advance the current run by one revision".into());
        }
        if next.objective != cur.objective
            || next.criteria != cur.criteria
            || next.limits != cur.limits
            || (next.blocked_reason.is_some() && next.phase != Phase::Blocked)
        {
            return Err(
                "orchestration change cannot alter the contract (objective, criteria, limits)"
                    .into(),
            );
        }
        if created_at != current.created_at
            || updated_at < current.updated_at
            || rounds_started != current.rounds_started
        {
            return Err(
                "orchestration change does not preserve the current counters and timestamps".into(),
            );
        }
        Ok(next.clone())
    }

    fn next_state(&self, run: Run, current: &RunState) -> RunState {
        RunState {
            run,
            plan_version: current.plan_version,
            tasks: current.tasks.clone(),
            seen_tasks: current.seen_tasks.clone(),
            plan_order: current.plan_order.clone(),
            task_seq: current.task_seq.clone(),
            workers_dispatched: current.workers_dispatched,
            checks: current.checks.clone(),
            rounds_started: current.rounds_started,
            created_at: current.created_at,
            updated_at: current.updated_at,
        }
    }
}

/// Whether the plan's internal dependency graph has a cycle.
fn cyclic(tasks: &[TaskSpec]) -> bool {
    let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let mut visiting = HashSet::new();
    let mut done = HashSet::new();
    fn visit(
        id: &str,
        tasks: &HashMap<&str, &TaskSpec>,
        visiting: &mut HashSet<String>,
        done: &mut HashSet<String>,
        ids: &HashSet<&str>,
    ) -> bool {
        if done.contains(id) {
            return false;
        }
        if !visiting.insert(id.to_owned()) {
            return true;
        }
        if let Some(t) = tasks.get(id) {
            for d in &t.depends_on {
                if ids.contains(d.as_str()) && visit(d, tasks, visiting, done, ids) {
                    return true;
                }
            }
        }
        visiting.remove(id);
        done.insert(id.to_owned());
        false
    }
    let map: HashMap<&str, &TaskSpec> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    tasks
        .iter()
        .any(|t| visit(&t.id, &map, &mut visiting, &mut done, &ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 0,
            data,
            extra: Default::default(),
        }
    }

    fn criteria() -> Value {
        json!([{"id": "report", "criterion": "report.md exists with findings"}])
    }

    fn limits() -> Value {
        json!({"maxRounds": 4, "maxParallel": 2, "maxWorkers": 6})
    }

    fn run(rev: u64, phase: &str) -> Value {
        let mut v = json!({
            "id": "orch-1", "revision": rev, "objective": "Analyze and report",
            "criteria": criteria(), "limits": limits(), "phase": phase,
        });
        if phase == "blocked" {
            v["blockedReason"] = json!({"code": "round-limit", "message": "limit"});
        }
        v
    }

    fn change(op: &str, r: Value, extra: Value, rounds: u64, at: u64) -> Value {
        let mut m = json!({
            "kind": ORCHESTRATION_CHANGE, "version": 1, "operation": op,
            "run": r, "roundsStarted": rounds, "createdAt": 10, "updatedAt": at,
        });
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                m[k] = v.clone();
            }
        }
        m
    }

    #[test]
    fn create_validates_freshness() {
        let mut fold = OrchestratorFold::default();
        let create = decode_change(&change("create", run(1, "active"), json!({}), 0, 10))
            .unwrap()
            .unwrap();
        fold.apply_change(&create).unwrap();
        let duplicate = decode_change(&change("create", run(1, "active"), json!({}), 0, 11))
            .unwrap()
            .unwrap();
        assert!(fold.apply_change(&duplicate).is_err());
        let bad_id = decode_change(&change(
            "create",
            json!({
                "id": "goal-1", "revision": 1, "objective": "x",
                "criteria": criteria(), "limits": limits(), "phase": "active",
            }),
            json!({}),
            0,
            10,
        ))
        .unwrap()
        .unwrap();
        assert!(fold.apply_change(&bad_id).is_err());
    }

    #[test]
    fn plan_task_and_complete_roundtrip() {
        let mut fold = OrchestratorFold::default();
        for op in [
            change("create", run(1, "active"), json!({}), 0, 10),
            change(
                "plan",
                run(2, "active"),
                json!({"plan": {"version": 1, "tasks": [
                    {"id": "t-research", "description": "Gather sources", "dependsOn": []},
                    {"id": "t-write", "description": "Write report", "dependsOn": ["t-research"]},
                ]}}),
                0,
                11,
            ),
        ] {
            fold.apply_change(&decode_change(&op).unwrap().unwrap())
                .unwrap();
        }
        let state = fold.state().unwrap().unwrap();
        assert_eq!(state.plan_version, 1);
        assert_eq!(state.tasks.len(), 2);

        // round 1 admitted
        fold.apply(&ev(
            2,
            "user/message",
            json!({"source": {"kind": "orchestration", "runId": "orch-1", "revision": 2, "round": 1}}),
        ))
        .unwrap();
        // a round of a stale revision is rejected
        assert!(fold
            .apply(&ev(
                3,
                "user/message",
                json!({"source": {"kind": "orchestration", "runId": "orch-1", "revision": 99, "round": 2}}),
            ))
            .is_err());

        // dispatch, settle, complete with evidence
        for op in [
            change(
                "task",
                run(3, "active"),
                json!({"task": {"id": "t-research", "status": "running", "childId": "sess-a"}}),
                1,
                12,
            ),
            change(
                "task",
                run(4, "active"),
                json!({"task": {"id": "t-research", "status": "completed", "summary": "3 sources"}}),
                1,
                13,
            ),
        ] {
            fold.apply_change(&decode_change(&op).unwrap().unwrap())
                .unwrap();
        }
        let state = fold.state().unwrap().unwrap();
        assert_eq!(state.workers_dispatched, 1);

        let complete = change(
            "complete",
            run(5, "complete"),
            json!({"checks": [{"criterionId": "report", "evidence": ["report.md", "session:sess-a"]}]}),
            1,
            14,
        );
        fold.apply_change(&decode_change(&complete).unwrap().unwrap())
            .unwrap();
        assert_eq!(fold.state().unwrap().unwrap().run.phase, Phase::Complete);

        // a complete without evidence coverage is refused
        let mut fold2 = OrchestratorFold::default();
        fold2
            .apply_change(
                &decode_change(&change("create", run(1, "active"), json!({}), 0, 10))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        let empty_checks = change(
            "complete",
            run(2, "complete"),
            json!({"checks": [{"criterionId": "unknown", "evidence": ["x"]}]}),
            0,
            11,
        );
        assert!(decode_change(&empty_checks).is_err());
        let no_checks = change("complete", run(2, "complete"), json!({"checks": []}), 0, 11);
        assert!(decode_change(&no_checks).is_err());
    }

    #[test]
    fn plan_rejects_cycles_unknown_deps_and_reuse() {
        let mut fold = OrchestratorFold::default();
        fold.apply_change(
            &decode_change(&change("create", run(1, "active"), json!({}), 0, 10))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let plan = |tasks: Value, rev: u64| {
            decode_change(&change(
                "plan",
                run(rev, "active"),
                json!({"plan": {"version": 1, "tasks": tasks}}),
                0,
                11,
            ))
            .unwrap()
            .unwrap()
        };
        let cycle = plan(
            json!([
                {"id": "a", "description": "x", "dependsOn": ["b"]},
                {"id": "b", "description": "x", "dependsOn": ["a"]},
            ]),
            2,
        );
        assert!(fold.apply_change(&cycle).is_err());
        let unknown = plan(
            json!([{"id": "a", "description": "x", "dependsOn": ["ghost"]}]),
            2,
        );
        assert!(fold.apply_change(&unknown).is_err());
        let self_dep = plan(
            json!([{"id": "a", "description": "x", "dependsOn": ["a"]}]),
            2,
        );
        assert!(fold.apply_change(&self_dep).is_err());
        let good = plan(json!([{"id": "a", "description": "x", "dependsOn": []}]), 2);
        fold.apply_change(&good).unwrap();
        // version 1 again is refused; so is redefining "a" in version 2
        let same_version = decode_change(&change(
            "plan",
            run(3, "active"),
            json!({"plan": {"version": 1, "tasks": [{"id": "b", "description": "x", "dependsOn": []}]}}),
            0,
            12,
        ))
        .unwrap()
        .unwrap();
        assert!(fold.apply_change(&same_version).is_err());
        let reuse = decode_change(&change(
            "plan",
            run(3, "active"),
            json!({"plan": {"version": 2, "tasks": [{"id": "a", "description": "x2", "dependsOn": []}]}}),
            0,
            12,
        ))
        .unwrap()
        .unwrap();
        assert!(fold.apply_change(&reuse).is_err());
        // depending on the historical completed task is fine
        for op in [
            change(
                "task",
                run(3, "active"),
                json!({"task": {"id": "a", "status": "running", "childId": "s1"}}),
                0,
                12,
            ),
            change(
                "task",
                run(4, "active"),
                json!({"task": {"id": "a", "status": "completed", "summary": "done"}}),
                0,
                13,
            ),
            change(
                "plan",
                run(5, "active"),
                json!({"plan": {"version": 2, "tasks": [{"id": "b", "description": "x", "dependsOn": ["a"]}]} }),
                0,
                14,
            ),
        ] {
            fold.apply_change(&decode_change(&op).unwrap().unwrap())
                .unwrap();
        }
    }

    #[test]
    fn task_transitions_and_worker_budget() {
        let mut fold = OrchestratorFold::default();
        for op in [
            change("create", run(1, "active"), json!({}), 0, 10),
            change(
                "plan",
                run(2, "active"),
                json!({"plan": {"version": 1, "tasks": [{"id": "a", "description": "x", "dependsOn": []}]}}),
                0,
                11,
            ),
        ] {
            fold.apply_change(&decode_change(&op).unwrap().unwrap())
                .unwrap();
        }
        // settle without dispatch is refused
        let skip = decode_change(&change(
            "task",
            run(3, "active"),
            json!({"task": {"id": "a", "status": "completed", "summary": "done"}}),
            0,
            12,
        ))
        .unwrap()
        .unwrap();
        assert!(fold.apply_change(&skip).is_err());
        // double dispatch is refused
        for _ in 0..2 {
            let dispatch = decode_change(&change(
                "task",
                run(3, "active"),
                json!({"task": {"id": "a", "status": "running", "childId": "s1"}}),
                0,
                12,
            ))
            .unwrap()
            .unwrap();
            let r = fold.apply_change(&dispatch);
            if r.is_err() {
                break;
            }
        }
        let state = fold.state().unwrap().unwrap();
        assert_eq!(state.workers_dispatched, 1);
        // a task of an older plan version cannot be dispatched
        let stale = decode_change(&change(
            "plan",
            run(4, "active"),
            json!({"plan": {"version": 2, "tasks": [{"id": "b", "description": "x", "dependsOn": []}]}}),
            0,
            13,
        ))
        .unwrap()
        .unwrap();
        fold.apply_change(&stale).unwrap();
        let old = decode_change(&change(
            "task",
            run(5, "active"),
            json!({"task": {"id": "a", "status": "running", "childId": "s2"}}),
            0,
            14,
        ))
        .unwrap()
        .unwrap();
        assert!(fold.apply_change(&old).is_err());
    }

    #[test]
    fn pause_resume_block_cancel_and_clear() {
        let mut fold = OrchestratorFold::default();
        fold.apply_change(
            &decode_change(&change("create", run(1, "active"), json!({}), 0, 10))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        // pause / resume
        fold.apply_change(
            &decode_change(&change("pause", run(2, "paused"), json!({}), 0, 11))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        fold.apply_change(
            &decode_change(&change("resume", run(3, "active"), json!({}), 0, 12))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        // block with a kebab code
        let block = decode_change(&change(
            "block",
            json!({
                "id": "orch-1", "revision": 4, "objective": "Analyze and report",
                "criteria": criteria(), "limits": limits(), "phase": "blocked",
                "blockedReason": {"code": "needs-input", "message": "waiting for the export"},
            }),
            json!({}),
            0,
            13,
        ))
        .unwrap()
        .unwrap();
        fold.apply_change(&block).unwrap();
        assert_eq!(
            fold.state()
                .unwrap()
                .unwrap()
                .run
                .blocked_reason
                .as_ref()
                .unwrap()
                .code,
            "needs-input"
        );
        // resume from blocked, then cancel, then clear
        fold.apply_change(
            &decode_change(&change("resume", run(5, "active"), json!({}), 0, 14))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        fold.apply_change(
            &decode_change(&change("cancel", run(6, "cancelled"), json!({}), 0, 15))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        fold.apply_change(
            &decode_change(&json!({
                "kind": ORCHESTRATION_CHANGE, "version": 1, "operation": "clear",
                "cleared": {"id": "orch-1", "revision": 7}, "clearedAt": 16,
            }))
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        assert!(fold.state().unwrap().is_none());
        assert!(fold.seen.contains("orch-1"));
        // a cancelled run may be replaced
        let recreate = decode_change(&change(
            "create",
            json!({
                "id": "orch-2", "revision": 1, "objective": "Next one",
                "criteria": criteria(), "limits": limits(), "phase": "active",
            }),
            json!({}),
            0,
            17,
        ))
        .unwrap()
        .unwrap();
        fold.apply_change(&recreate).unwrap();
    }

    #[test]
    fn round_cap_is_enforced_by_admission() {
        let mut fold = OrchestratorFold::default();
        fold.apply_change(
            &decode_change(&change("create", run(1, "active"), json!({}), 0, 10))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let round = |r: u64| {
            ev(
                1,
                "user/message",
                json!({"source": {"kind": "orchestration", "runId": "orch-1", "revision": 1, "round": r}}),
            )
        };
        fold.apply(&round(1)).unwrap();
        fold.apply(&round(2)).unwrap();
        fold.apply(&round(3)).unwrap();
        fold.apply(&round(4)).unwrap();
        assert!(fold.apply(&round(5)).is_err()); // maxRounds = 4
    }

    #[test]
    fn to_json_roundtrips_through_decode() {
        let create = RunChange::Create {
            run: Run {
                id: "orch-9".into(),
                revision: 1,
                objective: "Ship".into(),
                criteria: vec![Criterion {
                    id: "c1".into(),
                    criterion: "tests pass".into(),
                }],
                limits: Limits {
                    max_rounds: 8,
                    max_parallel: 3,
                    max_workers: 12,
                },
                phase: Phase::Active,
                blocked_reason: None,
            },
            rounds_started: 0,
            created_at: 1,
            updated_at: 1,
        };
        assert_eq!(decode_change(&create.to_json()).unwrap(), Some(create));
    }
}
