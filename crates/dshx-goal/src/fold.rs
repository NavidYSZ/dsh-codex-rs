//! The strict replay fold of durable goal changes (dsh `dsh-goal`
//! `lib/types/fold.js` plus the projection that retains the first failure):
//! `goal/change` snapshots and tombstones, and goal-round prompts that
//! advance `roundsStarted`.
use std::collections::HashSet;

use dshx_core::SessionEvent;
use serde_json::{Map, Value, json};

/// Session event type of every goal mutation.
pub const GOAL_CHANGE: &str = "goal/change";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Active => "active",
            Phase::Paused => "paused",
            Phase::Blocked => "blocked",
            Phase::Complete => "complete",
        }
    }

    fn parse(s: &str) -> Option<Phase> {
        Some(match s {
            "active" => Phase::Active,
            "paused" => Phase::Paused,
            "blocked" => Phase::Blocked,
            "complete" => Phase::Complete,
            _ => return None,
        })
    }
}

/// Why a goal is blocked: a lower-kebab-case `code` and a trimmed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockReason {
    pub code: String,
    pub message: String,
}

/// One revision of a goal (dsh `GoalSnapshot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub id: String,
    pub revision: u64,
    pub objective: String,
    pub phase: Phase,
    pub max_goal_rounds: u64,
    /// Only on `blocked`.
    pub blocked_reason: Option<BlockReason>,
}

impl Goal {
    /// The snapshot as logged, in dsh's key order.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("revision".into(), self.revision.into());
        m.insert("objective".into(), self.objective.clone().into());
        m.insert("phase".into(), self.phase.as_str().into());
        m.insert("maxGoalRounds".into(), self.max_goal_rounds.into());
        if let Some(r) = &self.blocked_reason {
            m.insert(
                "blockedReason".into(),
                json!({"code": r.code, "message": r.message}),
            );
        }
        Value::Object(m)
    }
}

/// The current goal with its derived counters (dsh projection `current`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalState {
    pub goal: Goal,
    pub rounds_started: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A decoded `goal/change` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalChange {
    Snapshot {
        operation: String,
        goal: Goal,
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

impl GoalChange {
    /// The payload as logged, in dsh's key order.
    pub fn to_json(&self) -> Value {
        match self {
            GoalChange::Snapshot {
                operation,
                goal,
                rounds_started,
                created_at,
                updated_at,
            } => json!({
                "kind": GOAL_CHANGE,
                "version": 1,
                "operation": operation,
                "goal": goal.to_json(),
                "roundsStarted": rounds_started,
                "createdAt": created_at,
                "updatedAt": updated_at,
            }),
            GoalChange::Clear {
                id,
                revision,
                cleared_at,
            } => json!({
                "kind": GOAL_CHANGE,
                "version": 1,
                "operation": "clear",
                "cleared": {"id": id, "revision": revision},
                "clearedAt": cleared_at,
            }),
        }
    }

    pub fn operation(&self) -> &str {
        match self {
            GoalChange::Snapshot { operation, .. } => operation,
            GoalChange::Clear { .. } => "clear",
        }
    }

    /// The revision identity the change carries (dsh `goalChangeRef`).
    pub fn reference(&self) -> (String, u64) {
        match self {
            GoalChange::Snapshot { goal, .. } => (goal.id.clone(), goal.revision),
            GoalChange::Clear { id, revision, .. } => (id.clone(), *revision),
        }
    }
}

type Result<T> = std::result::Result<T, String>;

fn keys(v: &Map<String, Value>) -> String {
    let mut k: Vec<&str> = v.keys().map(String::as_str).collect();
    k.sort_unstable();
    k.join(",")
}

/// A JS safe integer (`Number.isSafeInteger`) in the given range.
pub(crate) fn safe_integer(v: &Value) -> Option<i64> {
    const MAX: f64 = 9_007_199_254_740_991.0;
    let f = v.as_f64()?;
    (f.fract() == 0.0 && f.abs() <= MAX).then_some(f as i64)
}

fn positive(v: &Value, field: &str) -> Result<u64> {
    match safe_integer(v) {
        Some(n) if n >= 1 => Ok(n as u64),
        _ => Err(format!(
            "goal change {field} must be a positive safe integer"
        )),
    }
}

fn non_negative(v: &Value, field: &str) -> Result<u64> {
    match safe_integer(v) {
        Some(n) if n >= 0 => Ok(n as u64),
        _ => Err(format!(
            "goal change {field} must be a non-negative safe integer"
        )),
    }
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

fn decode_block_reason(v: &Value) -> Result<BlockReason> {
    let Some(m) = v.as_object().filter(|m| keys(m) == "code,message") else {
        return Err(
            "goal change goal.blockedReason must have exactly code and message fields".into(),
        );
    };
    let code = m["code"]
        .as_str()
        .filter(|c| kebab(c))
        .ok_or("goal change goal.blockedReason.code must be lower-kebab-case".to_owned())?;
    let message = m["message"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.trim() == *s)
        .ok_or(
            "goal change goal.blockedReason.message must be non-empty and normalized".to_owned(),
        )?;
    Ok(BlockReason {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

fn decode_snapshot(v: &Value) -> Result<Goal> {
    let m = v.as_object().ok_or("goal change goal must be a record")?;
    let id = m
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("goal change goal.id must be a non-empty string")?;
    let objective = m
        .get("objective")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.trim() == *s)
        .ok_or("goal change goal.objective must be non-empty and normalized")?;
    let phase = m
        .get("phase")
        .and_then(Value::as_str)
        .and_then(Phase::parse)
        .ok_or("goal change goal.phase is invalid")?;
    let expected = if phase == Phase::Blocked {
        "blockedReason,id,maxGoalRounds,objective,phase,revision"
    } else {
        "id,maxGoalRounds,objective,phase,revision"
    };
    if keys(m) != expected {
        return Err(format!(
            "goal change goal for phase {} must have exactly {expected} fields",
            phase.as_str()
        ));
    }
    Ok(Goal {
        id: id.to_owned(),
        revision: positive(&m["revision"], "goal.revision")?,
        objective: objective.to_owned(),
        phase,
        max_goal_rounds: positive(&m["maxGoalRounds"], "goal.maxGoalRounds")?,
        blocked_reason: if phase == Phase::Blocked {
            Some(decode_block_reason(&m["blockedReason"])?)
        } else {
            None
        },
    })
}

/// dsh `decodeGoalChange`: `None` for a value of another kind.
pub fn decode_change(v: &Value) -> Result<Option<GoalChange>> {
    let Some(m) = v
        .as_object()
        .filter(|m| m.get("kind") == Some(&json!(GOAL_CHANGE)))
    else {
        return Ok(None);
    };
    if m.get("version") != Some(&json!(1)) {
        return Err(format!(
            "unsupported goal change version {}",
            m.get("version")
                .map_or("undefined".into(), |v| v.to_string())
        ));
    }
    if m.get("operation") == Some(&json!("clear")) {
        if keys(m) != "cleared,clearedAt,kind,operation,version" {
            return Err(
                "goal clear change must have exactly cleared,clearedAt,kind,operation,version fields"
                    .into(),
            );
        }
        let Some(r) = m["cleared"]
            .as_object()
            .filter(|r| keys(r) == "id,revision")
        else {
            return Err("goal clear tombstone must have exactly id and revision fields".into());
        };
        let id = r["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("goal clear tombstone id must be a non-empty string")?;
        return Ok(Some(GoalChange::Clear {
            id: id.to_owned(),
            revision: positive(&r["revision"], "cleared.revision")?,
            cleared_at: non_negative(&m["clearedAt"], "clearedAt")?,
        }));
    }
    let operation = m
        .get("operation")
        .and_then(Value::as_str)
        .filter(|o| ["create", "edit", "pause", "resume", "complete", "block"].contains(o))
        .ok_or("goal change operation is invalid")?;
    if keys(m) != "createdAt,goal,kind,operation,roundsStarted,updatedAt,version" {
        return Err("goal snapshot change must have exactly createdAt,goal,kind,operation,roundsStarted,updatedAt,version fields".into());
    }
    let created_at = non_negative(&m["createdAt"], "createdAt")?;
    let updated_at = non_negative(&m["updatedAt"], "updatedAt")?;
    if updated_at < created_at {
        return Err("goal change updatedAt cannot precede createdAt".into());
    }
    Ok(Some(GoalChange::Snapshot {
        operation: operation.to_owned(),
        goal: decode_snapshot(&m["goal"])?,
        rounds_started: non_negative(&m["roundsStarted"], "roundsStarted")?,
        created_at,
        updated_at,
    }))
}

/// A goal-round prompt's `{goalId, revision, round}` (dsh `goalSource`).
pub(crate) fn round_source(source: &Value) -> Result<Option<(String, u64, u64)>> {
    if source.get("kind").and_then(Value::as_str) != Some("goal") {
        return Ok(None);
    }
    let id = source["goalId"].as_str().filter(|s| !s.is_empty());
    let revision = safe_integer(&source["revision"]).filter(|n| *n >= 1);
    let round = safe_integer(&source["round"]).filter(|n| *n >= 1);
    match (id, revision, round) {
        (Some(id), Some(rev), Some(round)) => Ok(Some((id.to_owned(), rev as u64, round as u64))),
        _ => Err("goal message source is invalid".into()),
    }
}

/// The durable goal state of one session (dsh `goal` projection state).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalFold {
    pub current: Option<GoalState>,
    pub seen: HashSet<String>,
    /// The first invalid owned event; later events are ignored.
    pub failure: Option<String>,
    /// Events folded so far.
    pub folded: u64,
}

impl GoalFold {
    pub fn from_events(events: &[SessionEvent]) -> GoalFold {
        let mut fold = GoalFold::default();
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
                    "goal replay failed at session event {}: {message}",
                    e.seq
                ));
            }
        }
    }

    /// The current goal, or the retained replay failure.
    pub fn state(&self) -> std::result::Result<Option<&GoalState>, String> {
        match &self.failure {
            Some(f) => Err(f.clone()),
            None => Ok(self.current.as_ref()),
        }
    }

    fn apply(&mut self, e: &SessionEvent) -> Result<()> {
        match e.kind.as_str() {
            GOAL_CHANGE => {
                let change = decode_change(&e.data)?.ok_or_else(|| {
                    format!("goal change at session event {} has an invalid kind", e.seq)
                })?;
                self.apply_change(&change)
            }
            dshx_core::kinds::USER_MESSAGE => {
                let Some((id, revision, round)) = round_source(&e.data["source"])? else {
                    return Ok(());
                };
                let ok = self.current.as_ref().is_some_and(|c| {
                    c.goal.phase == Phase::Active
                        && c.goal.id == id
                        && c.goal.revision == revision
                        && round == c.rounds_started + 1
                        && round <= c.goal.max_goal_rounds
                });
                if !ok {
                    return Err(format!(
                        "goal round at session event {} is not the next admitted round of the active goal",
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

    /// dsh `applyGoalChange`.
    pub fn apply_change(&mut self, change: &GoalChange) -> Result<()> {
        match change {
            GoalChange::Clear {
                id,
                revision,
                cleared_at,
            } => {
                let current = self
                    .current
                    .as_ref()
                    .ok_or("goal clear requires a current goal")?;
                if *id != current.goal.id || *revision != current.goal.revision + 1 {
                    return Err("goal clear must advance the current goal by one revision".into());
                }
                if *cleared_at < current.updated_at {
                    return Err(
                        "goal clear timestamp cannot precede the current goal update".into(),
                    );
                }
                self.current = None;
                Ok(())
            }
            GoalChange::Snapshot {
                operation,
                goal,
                rounds_started,
                created_at,
                updated_at,
            } => {
                if operation == "create" {
                    let replaceable = self
                        .current
                        .as_ref()
                        .is_none_or(|c| c.goal.phase == Phase::Complete);
                    if goal.revision != 1
                        || goal.phase != Phase::Active
                        || *rounds_started != 0
                        || !replaceable
                        || self.seen.contains(&goal.id)
                    {
                        return Err(
                            "goal create requires a fresh active revision-one goal with zero rounds"
                                .into(),
                        );
                    }
                    self.seen.insert(goal.id.clone());
                } else {
                    let current = self
                        .current
                        .as_ref()
                        .ok_or_else(|| format!("goal {operation} requires a current goal"))?;
                    validate_transition(
                        current,
                        operation,
                        goal,
                        *rounds_started,
                        *created_at,
                        *updated_at,
                    )?;
                }
                self.current = Some(GoalState {
                    goal: goal.clone(),
                    rounds_started: *rounds_started,
                    created_at: *created_at,
                    updated_at: *updated_at,
                });
                Ok(())
            }
        }
    }
}

/// dsh `validateSnapshotTransition`.
fn validate_transition(
    current: &GoalState,
    operation: &str,
    next: &Goal,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
) -> Result<()> {
    let cur = &current.goal;
    if next.id != cur.id || next.revision != cur.revision + 1 {
        return Err(format!(
            "goal {operation} must advance the current goal by one revision"
        ));
    }
    if created_at != current.created_at
        || updated_at < current.updated_at
        || rounds_started != current.rounds_started
    {
        return Err(format!(
            "goal {operation} does not preserve the current counters and timestamps"
        ));
    }
    let same_definition = || {
        if next.objective != cur.objective || next.max_goal_rounds != cur.max_goal_rounds {
            Err(format!(
                "goal {operation} cannot change objective or maxGoalRounds"
            ))
        } else {
            Ok(())
        }
    };
    match operation {
        "edit" => {
            if next.phase != cur.phase || next.blocked_reason != cur.blocked_reason {
                return Err("goal edit cannot change phase or blocked reason".into());
            }
        }
        "pause" => {
            same_definition()?;
            if cur.phase != Phase::Active || next.phase != Phase::Paused {
                return Err("goal pause has an invalid phase transition".into());
            }
        }
        "resume" => {
            same_definition()?;
            if cur.phase == Phase::Complete
                || next.phase != Phase::Active
                || current.rounds_started >= next.max_goal_rounds
            {
                return Err(
                    "goal resume has an invalid phase transition or exhausted round budget".into(),
                );
            }
        }
        "complete" => {
            same_definition()?;
            if cur.phase == Phase::Complete || next.phase != Phase::Complete {
                return Err("goal complete has an invalid phase transition".into());
            }
        }
        "block" => {
            same_definition()?;
            if cur.phase != Phase::Active || next.phase != Phase::Blocked {
                return Err("goal block has an invalid phase transition".into());
            }
        }
        _ => return Err("unknown goal snapshot operation".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 0,
            data,
            extra: Default::default(),
        }
    }

    fn goal(rev: u64, phase: &str) -> Value {
        json!({"id": "goal-1", "revision": rev, "objective": "Ship it", "phase": phase, "maxGoalRounds": 2})
    }

    fn change(op: &str, g: Value, rounds: u64, at: u64) -> Value {
        json!({"kind": "goal/change", "version": 1, "operation": op, "goal": g, "roundsStarted": rounds, "createdAt": 10, "updatedAt": at})
    }

    #[test]
    fn folds_create_rounds_and_clear() {
        let round = |r: u64| json!({"id": "m", "role": "user", "content": [], "source": {"kind": "goal", "goalId": "goal-1", "revision": 1, "round": r}});
        let events = vec![
            ev(0, GOAL_CHANGE, change("create", goal(1, "active"), 0, 10)),
            ev(1, "user/message", round(1)),
            ev(2, "user/message", round(2)),
            ev(
                3,
                GOAL_CHANGE,
                json!({"kind": "goal/change", "version": 1, "operation": "clear", "cleared": {"id": "goal-1", "revision": 2}, "clearedAt": 12}),
            ),
        ];
        let mut fold = GoalFold::from_events(&events[..3]);
        let state = fold.state().unwrap().unwrap();
        assert_eq!(state.rounds_started, 2);
        assert_eq!(state.goal.phase, Phase::Active);
        fold.extend(&events);
        assert_eq!(fold.state().unwrap(), None);
        assert!(fold.seen.contains("goal-1"));
        assert_eq!(fold.folded, 4);
    }

    #[test]
    fn invalid_events_are_retained_as_failure() {
        let events = vec![
            ev(0, GOAL_CHANGE, change("create", goal(1, "active"), 0, 10)),
            ev(
                1,
                "user/message",
                json!({"source": {"kind": "goal", "goalId": "goal-1", "revision": 1, "round": 3}}),
            ),
            ev(2, GOAL_CHANGE, change("pause", goal(2, "paused"), 0, 11)),
        ];
        let fold = GoalFold::from_events(&events);
        assert_eq!(
            fold.state().unwrap_err(),
            "goal replay failed at session event 1: goal round at session event 1 is not the next admitted round of the active goal"
        );
        let bad_block = json!({"id": "goal-1", "revision": 2, "objective": "Ship it", "phase": "blocked", "maxGoalRounds": 2, "blockedReason": {"code": "Bad Code", "message": "x"}});
        let fold = GoalFold::from_events(&[
            ev(0, GOAL_CHANGE, change("create", goal(1, "active"), 0, 10)),
            ev(1, GOAL_CHANGE, change("block", bad_block, 0, 11)),
        ]);
        assert!(
            fold.state()
                .unwrap_err()
                .ends_with("goal change goal.blockedReason.code must be lower-kebab-case")
        );
    }

    #[test]
    fn transitions_follow_dsh() {
        let mut fold = GoalFold::default();
        let create = decode_change(&change("create", goal(1, "active"), 0, 10))
            .unwrap()
            .unwrap();
        fold.apply_change(&create).unwrap();
        let resume_complete = decode_change(&change("resume", goal(2, "active"), 0, 11))
            .unwrap()
            .unwrap();
        fold.apply_change(&resume_complete).unwrap();
        let skip = decode_change(&change("pause", goal(4, "paused"), 0, 12))
            .unwrap()
            .unwrap();
        assert_eq!(
            fold.apply_change(&skip).unwrap_err(),
            "goal pause must advance the current goal by one revision"
        );
        assert!(kebab("round-limit") && kebab("model-reported") && !kebab("-x") && !kebab("a--b"));
        assert_eq!(create.to_json()["goal"], goal(1, "active"));
    }
}
