//! Team state folded from the Lead session's `team/*` v2 events (dsh
//! `agentTeam` projection): roster snapshots, task snapshots, the durable
//! mailbox and delivery acknowledgements. The first invalid owned event is
//! retained as the projection failure.
use std::collections::{HashMap, HashSet};

use dshx_core::SessionEvent;
use serde_json::{Map, Value, json};

pub const TEAM_MEMBER: &str = "team/member";
pub const TEAM_TASK: &str = "team/task";
pub const TEAM_MESSAGE_QUEUED: &str = "team/message/queued";
pub const TEAM_MESSAGE_DELIVERED: &str = "team/message/delivered";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub description: String,
    pub provider: String,
    /// `fresh` or `fork`.
    pub context: String,
    /// `provisioning`, `active` or `failed`.
    pub phase: String,
    pub error: Option<String>,
}

impl Member {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("name".into(), self.name.clone().into());
        m.insert("description".into(), self.description.clone().into());
        m.insert("provider".into(), self.provider.clone().into());
        m.insert("context".into(), self.context.clone().into());
        m.insert("phase".into(), self.phase.clone().into());
        if let Some(e) = &self.error {
            m.insert("error".into(), e.clone().into());
        }
        Value::Object(m)
    }

    fn from_json(v: &Value) -> Result<Member, String> {
        let m = strict(
            v,
            &["id", "name", "description", "provider", "context", "phase"],
            &["error"],
        )?;
        let s = |k: &str| {
            m[k].as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("member {k} must be a string"))
        };
        let member = Member {
            id: s("id").and_then(non_empty)?,
            name: s("name")?,
            description: s("description")?,
            provider: s("provider")?,
            context: s("context")?,
            phase: s("phase")?,
            error: match m.get("error") {
                None => None,
                Some(e) => Some(
                    e.as_str()
                        .ok_or("member error must be a string")?
                        .to_owned(),
                ),
            },
        };
        if !["fresh", "fork"].contains(&member.context.as_str()) {
            return Err("member context must be fresh or fork".into());
        }
        if !["provisioning", "active", "failed"].contains(&member.phase.as_str()) {
            return Err("member phase is invalid".into());
        }
        Ok(member)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub revision: u64,
    pub subject: String,
    pub description: String,
    /// `pending`, `in_progress`, `completed` or `deleted`.
    pub status: String,
    pub owner_id: Option<String>,
    pub blocked_by: Vec<String>,
    pub write_scopes: Vec<String>,
}

impl Task {
    /// The snapshot in dsh's key order; an owner set by this revision comes last.
    pub fn to_json(&self, owner_last: bool) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("revision".into(), self.revision.into());
        m.insert("subject".into(), self.subject.clone().into());
        m.insert("description".into(), self.description.clone().into());
        m.insert("status".into(), self.status.clone().into());
        if let (Some(o), false) = (&self.owner_id, owner_last) {
            m.insert("ownerId".into(), o.clone().into());
        }
        m.insert("blockedBy".into(), json!(self.blocked_by));
        m.insert("writeScopes".into(), json!(self.write_scopes));
        if let (Some(o), true) = (&self.owner_id, owner_last) {
            m.insert("ownerId".into(), o.clone().into());
        }
        Value::Object(m)
    }

    fn from_json(v: &Value) -> Result<Task, String> {
        let m = strict(
            v,
            &[
                "id",
                "revision",
                "subject",
                "description",
                "status",
                "blockedBy",
                "writeScopes",
            ],
            &["ownerId"],
        )?;
        let s = |k: &str| {
            m[k].as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("task {k} must be a string"))
        };
        let list = |k: &str| -> Result<Vec<String>, String> {
            m[k].as_array()
                .ok_or_else(|| format!("task {k} must be an array"))?
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("task {k} entries must be strings"))
                })
                .collect()
        };
        let task = Task {
            id: s("id").and_then(non_empty)?,
            revision: m["revision"]
                .as_u64()
                .filter(|r| *r >= 1)
                .ok_or("task revision must be a positive integer")?,
            subject: s("subject")?,
            description: s("description")?,
            status: s("status")?,
            owner_id: match m.get("ownerId") {
                None => None,
                Some(o) => Some(non_empty(
                    o.as_str()
                        .map(str::to_owned)
                        .ok_or("task ownerId must be a string")?,
                )?),
            },
            blocked_by: list("blockedBy")?,
            write_scopes: list("writeScopes")?,
        };
        if !["pending", "in_progress", "completed", "deleted"].contains(&task.status.as_str()) {
            return Err("task status is invalid".into());
        }
        Ok(task)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedMessage {
    pub id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub target_id: String,
    pub content: Vec<Value>,
}

impl QueuedMessage {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "senderId": self.sender_id,
            "senderName": self.sender_name,
            "targetId": self.target_id,
            "content": self.content,
        })
    }

    fn from_json(v: &Value) -> Result<QueuedMessage, String> {
        let m = strict(
            v,
            &["id", "senderId", "senderName", "targetId", "content"],
            &[],
        )?;
        let s = |k: &str| {
            m[k].as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("message {k} must be a string"))
        };
        Ok(QueuedMessage {
            id: s("id").and_then(non_empty)?,
            sender_id: s("senderId").and_then(non_empty)?,
            sender_name: s("senderName")?,
            target_id: s("targetId").and_then(non_empty)?,
            content: m["content"]
                .as_array()
                .cloned()
                .ok_or("message content must be an array")?,
        })
    }
}

fn non_empty(s: String) -> Result<String, String> {
    if s.is_empty() {
        Err("identity must be non-empty".into())
    } else {
        Ok(s)
    }
}

fn strict<'a>(
    v: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let m = v.as_object().ok_or("expected an object")?;
    if !required.iter().all(|k| m.contains_key(*k))
        || m.keys()
            .any(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()))
    {
        return Err(format!("expected exactly {} fields", required.join(", ")));
    }
    Ok(m)
}

/// One Team's durable state (keyed by the Lead session id).
#[derive(Debug, Clone, PartialEq)]
pub struct TeamState {
    pub id: String,
    pub members: Vec<Member>,
    pub tasks: Vec<Task>,
    pub messages: Vec<QueuedMessage>,
    pub delivered: Vec<String>,
    pub next_task_number: u64,
    pub failure: Option<String>,
    pub folded: u64,
}

/// Which relation of the task graph a candidate breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphViolation {
    Missing,
    Duplicate,
    Cycle,
}

/// dsh `assertTaskGraphCandidate`.
pub fn assert_task_graph(
    current: &[Task],
    candidate: &Task,
) -> Result<(), (String, GraphViolation)> {
    let mut tasks: Vec<Task> = current
        .iter()
        .filter(|t| t.id != candidate.id)
        .cloned()
        .collect();
    let at = current
        .iter()
        .position(|t| t.id == candidate.id)
        .unwrap_or(tasks.len());
    tasks.insert(at.min(tasks.len()), candidate.clone());
    let by_id: HashMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    for task in &tasks {
        if task.status == "deleted" {
            continue;
        }
        let mut seen = HashSet::new();
        for b in &task.blocked_by {
            if *b == task.id {
                return Err((
                    format!("team task \"{}\" cannot block itself", task.id),
                    GraphViolation::Cycle,
                ));
            }
            if seen.contains(b) {
                return Err((
                    format!("team task \"{}\" repeats blocker \"{b}\"", task.id),
                    GraphViolation::Duplicate,
                ));
            }
            if by_id.get(b.as_str()).is_none_or(|t| t.status == "deleted") {
                return Err((
                    format!(
                        "blocker task \"{b}\" for \"{}\" is missing or deleted",
                        task.id
                    ),
                    GraphViolation::Missing,
                ));
            }
            seen.insert(b);
        }
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a Task>,
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<(), (String, GraphViolation)> {
        if visiting.contains(id) {
            return Err((
                format!("task dependency cycle includes \"{id}\""),
                GraphViolation::Cycle,
            ));
        }
        if visited.contains(id) {
            return Ok(());
        }
        let Some(task) = by_id.get(id).filter(|t| t.status != "deleted") else {
            return Ok(());
        };
        visiting.insert(id);
        for b in &task.blocked_by {
            visit(b, by_id, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }
    for task in &tasks {
        visit(&task.id, &by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn task_number(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("task-")?;
    (!digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()))
        .then(|| digits.parse().ok())?
}

impl TeamState {
    pub fn new(lead_id: &str) -> TeamState {
        TeamState {
            id: lead_id.to_owned(),
            members: Vec::new(),
            tasks: Vec::new(),
            messages: Vec::new(),
            delivered: Vec::new(),
            next_task_number: 1,
            failure: None,
            folded: 0,
        }
    }

    pub fn from_events(lead_id: &str, events: &[SessionEvent]) -> TeamState {
        let mut s = TeamState::new(lead_id);
        s.extend(events);
        s
    }

    /// Folds events with `seq >= self.folded`.
    pub fn extend(&mut self, events: &[SessionEvent]) {
        for e in events {
            if e.seq < self.folded {
                continue;
            }
            self.folded = e.seq + 1;
            if self.failure.is_some() || !e.kind.starts_with("team/") {
                continue;
            }
            if let Err(message) = self.apply(e) {
                self.failure = Some(message);
            }
        }
    }

    fn apply(&mut self, e: &SessionEvent) -> Result<(), String> {
        if ![
            TEAM_MEMBER,
            TEAM_TASK,
            TEAM_MESSAGE_QUEUED,
            TEAM_MESSAGE_DELIVERED,
        ]
        .contains(&e.kind.as_str())
        {
            return Ok(());
        }
        let invalid = || format!("persisted Agent Teams {} payload is invalid", e.kind);
        let team = e.data["teamId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(invalid)?;
        let version = e.data["version"].as_u64().ok_or_else(invalid)?;
        if team != self.id {
            return Ok(());
        }
        if version != 2 {
            return Err(format!("unsupported Agent Teams event version {version}"));
        }
        let wrap = |r: Result<(), String>| r.map_err(|_| invalid());
        match e.kind.as_str() {
            TEAM_MEMBER => {
                strict(&e.data, &["version", "teamId", "member"], &[]).map_err(|_| invalid())?;
                let member = Member::from_json(&e.data["member"]).map_err(|_| invalid())?;
                if let Some(named) = self.members.iter().find(|m| m.name == member.name)
                    && named.id != member.id
                {
                    return Err(format!(
                        "teammate name \"{}\" is reused by another member",
                        member.name
                    ));
                }
                match self.members.iter().position(|m| m.id == member.id) {
                    None => {
                        if member.phase != "provisioning" {
                            return Err(format!(
                                "teammate \"{}\" must begin provisioning",
                                member.name
                            ));
                        }
                        self.members.push(member);
                    }
                    Some(i) => {
                        let prior = &self.members[i];
                        if prior.name != member.name
                            || prior.provider != member.provider
                            || prior.context != member.context
                        {
                            return Err(format!(
                                "teammate \"{}\" changed immutable identity fields",
                                member.id
                            ));
                        }
                        if prior.phase != "provisioning" || member.phase == "provisioning" {
                            return Err(format!(
                                "teammate \"{}\" has an invalid {} -> {} transition",
                                member.name, prior.phase, member.phase
                            ));
                        }
                        self.members[i] = member;
                    }
                }
                Ok(())
            }
            TEAM_TASK => {
                strict(&e.data, &["version", "teamId", "task"], &[]).map_err(|_| invalid())?;
                let task = Task::from_json(&e.data["task"]).map_err(|_| invalid())?;
                let prior = self.tasks.iter().position(|t| t.id == task.id);
                match prior {
                    None if task.revision != 1 => {
                        return Err(format!(
                            "team task \"{}\" must begin at revision 1",
                            task.id
                        ));
                    }
                    Some(i) if task.revision != self.tasks[i].revision + 1 => {
                        return Err(format!(
                            "team task \"{}\" revision is not contiguous",
                            task.id
                        ));
                    }
                    _ => {}
                }
                assert_task_graph(&self.tasks, &task).map_err(|(m, _)| m)?;
                if let Some(n) = task_number(&task.id) {
                    self.next_task_number = self.next_task_number.max(n.saturating_add(1));
                }
                match prior {
                    Some(i) => self.tasks[i] = task,
                    None => self.tasks.push(task),
                }
                Ok(())
            }
            TEAM_MESSAGE_QUEUED => {
                wrap(strict(&e.data, &["version", "teamId", "message"], &[]).map(|_| ()))?;
                let message =
                    QueuedMessage::from_json(&e.data["message"]).map_err(|_| invalid())?;
                if self.messages.iter().any(|m| m.id == message.id) {
                    return Err(format!("team message \"{}\" was queued twice", message.id));
                }
                self.messages.push(message);
                Ok(())
            }
            _ => {
                wrap(
                    strict(
                        &e.data,
                        &["version", "teamId", "messageId", "targetId"],
                        &[],
                    )
                    .map(|_| ()),
                )?;
                let id = e.data["messageId"].as_str().ok_or_else(invalid)?;
                let target = e.data["targetId"].as_str().ok_or_else(invalid)?;
                let queued = self.messages.iter().find(|m| m.id == id).ok_or_else(|| {
                    format!("team message \"{id}\" was delivered before queueing")
                })?;
                if queued.target_id != target {
                    return Err(format!("team message \"{id}\" target changed"));
                }
                if self.delivered.iter().any(|d| d == id) {
                    return Err(format!("team message \"{id}\" was delivered twice"));
                }
                self.delivered.push(id.to_owned());
                Ok(())
            }
        }
    }

    /// Whether every blocker of `task` completed (dsh `taskReady`).
    pub fn task_ready(&self, task: &Task) -> bool {
        task.blocked_by.iter().all(|b| {
            self.tasks
                .iter()
                .find(|t| t.id == *b)
                .is_some_and(|t| t.status == "completed")
        })
    }

    /// dsh `projectTaskView`.
    pub fn task_view(&self, task: &Task) -> Value {
        let owner = task.owner_id.as_deref().and_then(|o| {
            if o == self.id {
                Some("lead".to_owned())
            } else {
                self.members
                    .iter()
                    .find(|m| m.id == o)
                    .map(|m| m.name.clone())
            }
        });
        let mut warnings: Vec<String> = Vec::new();
        for other in &self.tasks {
            if other.id == task.id || other.status != "in_progress" {
                continue;
            }
            let overlap = task.write_scopes.iter().any(|l| {
                other.write_scopes.iter().any(|r| {
                    l == r || l.starts_with(&format!("{r}/")) || r.starts_with(&format!("{l}/"))
                })
            });
            let w = format!("write scopes overlap with {}", other.id);
            if overlap && !warnings.contains(&w) {
                warnings.push(w);
            }
        }
        let mut m = Map::new();
        m.insert("id".into(), task.id.clone().into());
        m.insert("revision".into(), task.revision.into());
        m.insert("subject".into(), task.subject.clone().into());
        m.insert("description".into(), task.description.clone().into());
        m.insert("status".into(), task.status.clone().into());
        m.insert("blockedBy".into(), json!(task.blocked_by));
        m.insert("writeScopes".into(), json!(task.write_scopes));
        if let Some(o) = owner {
            m.insert("ownerName".into(), o.into());
        }
        m.insert(
            "ready".into(),
            (task.status == "pending" && self.task_ready(task)).into(),
        );
        m.insert("writeScopeWarnings".into(), json!(warnings));
        Value::Object(m)
    }

    /// The durable client view (dsh `teamProjectionView`).
    pub fn projection(&self) -> Value {
        let mut members =
            vec![json!({"id": self.id, "name": "lead", "role": "lead", "phase": "active"})];
        for m in &self.members {
            let mut v = json!({"id": m.id, "name": m.name, "role": "teammate", "phase": m.phase});
            if let Some(e) = &m.error {
                v["error"] = e.clone().into();
            }
            members.push(v);
        }
        let tasks: Vec<Value> = self
            .tasks
            .iter()
            .filter(|t| t.status != "deleted")
            .map(|t| self.task_view(t))
            .collect();
        let mut v = json!({"members": members, "tasks": tasks});
        if let Some(f) = &self.failure {
            v["failure"] = f.clone().into();
        }
        v
    }

    pub fn undelivered(&self) -> impl Iterator<Item = &QueuedMessage> {
        self.messages
            .iter()
            .filter(|m| !self.delivered.contains(&m.id))
    }
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

    fn task(id: &str, rev: u64, blocked: &[&str]) -> Value {
        json!({"id": id, "revision": rev, "subject": "s", "description": "d", "status": "pending", "blockedBy": blocked, "writeScopes": []})
    }

    #[test]
    fn folds_tasks_and_rejects_cycles() {
        let events = vec![
            ev(
                0,
                TEAM_TASK,
                json!({"version": 2, "teamId": "L", "task": task("task-1", 1, &[])}),
            ),
            ev(
                1,
                TEAM_TASK,
                json!({"version": 2, "teamId": "L", "task": task("task-2", 1, &["task-1"])}),
            ),
            ev(
                2,
                TEAM_TASK,
                json!({"version": 2, "teamId": "other", "task": task("task-9", 5, &[])}),
            ),
        ];
        let s = TeamState::from_events("L", &events);
        assert_eq!(s.failure, None);
        assert_eq!(s.next_task_number, 3);
        assert!(!s.task_ready(&s.tasks[1]));
        let cyc = TeamState::from_events(
            "L",
            &[
                events[0].clone(),
                events[1].clone(),
                ev(
                    3,
                    TEAM_TASK,
                    json!({"version": 2, "teamId": "L", "task": task("task-1", 2, &["task-2"])}),
                ),
            ],
        );
        assert_eq!(
            cyc.failure.as_deref(),
            Some("task dependency cycle includes \"task-1\"")
        );
    }

    #[test]
    fn mailbox_acknowledgements() {
        let msg = json!({"id": "team-message-1", "senderId": "L", "senderName": "lead", "targetId": "C", "content": [{"type": "text", "text": "hi"}]});
        let s = TeamState::from_events(
            "L",
            &[
                ev(
                    0,
                    TEAM_MESSAGE_QUEUED,
                    json!({"version": 2, "teamId": "L", "message": msg}),
                ),
                ev(
                    1,
                    TEAM_MESSAGE_DELIVERED,
                    json!({"version": 2, "teamId": "L", "messageId": "team-message-1", "targetId": "C"}),
                ),
                ev(
                    2,
                    TEAM_MESSAGE_DELIVERED,
                    json!({"version": 2, "teamId": "L", "messageId": "team-message-1", "targetId": "C"}),
                ),
            ],
        );
        assert_eq!(
            s.failure.as_deref(),
            Some("team message \"team-message-1\" was delivered twice")
        );
        assert_eq!(s.delivered, ["team-message-1"]);
    }
}
