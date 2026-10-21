//! The Agent Teams service (dsh `TeamService`: roster, mailbox, task board,
//! activity): every Team fact is an event in the exact live Lead session,
//! appended and flushed inside a per-Lead transaction; teammates are
//! continuable direct children of the Lead.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dshx_agent::{Agent, AgentObserver, AgentStatus};
use dshx_core::{ContentBlock, Message, MessageSource, NewEvent, Role, SessionEvent, kinds};
use dshx_subagent::{ContinuableSpec, SessionFactory, SubagentRuntime, subagent_descriptor};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::fold::{
    GraphViolation, Member, QueuedMessage, TEAM_MEMBER, TEAM_MESSAGE_DELIVERED,
    TEAM_MESSAGE_QUEUED, TEAM_TASK, Task, TeamState, assert_task_graph,
};

/// Deployment limits (the `dsh-experimental-agent-team-profile` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSettings {
    pub max_members: usize,
    pub max_tasks: usize,
    pub max_pending_messages_per_member: usize,
    pub max_message_bytes: usize,
    pub fresh_provider: String,
    pub fork_provider: String,
}

impl Default for TeamSettings {
    fn default() -> Self {
        TeamSettings::from_config(&dshx_core::config::TeamConfig::default())
    }
}

impl TeamSettings {
    pub fn from_config(c: &dshx_core::config::TeamConfig) -> Self {
        TeamSettings {
            max_members: c.max_members.max(1) as usize,
            max_tasks: c.max_tasks.max(1) as usize,
            max_pending_messages_per_member: c.max_pending_messages_per_member.max(1) as usize,
            max_message_bytes: c.max_message_bytes.max(1) as usize,
            fresh_provider: c.fresh_provider.clone(),
            fork_provider: c.fork_provider.clone(),
        }
    }
}

/// A refused Team operation (dsh `TeamError`); `code` `None` = a plain `Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamError {
    pub code: Option<&'static str>,
    pub message: String,
}

impl TeamError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        TeamError {
            code: Some(code),
            message: message.into(),
        }
    }

    pub(crate) fn plain(message: impl Into<String>) -> Self {
        TeamError {
            code: None,
            message: message.into(),
        }
    }

    pub fn into_tool_error(self) -> dshx_core::ToolError {
        match self.code {
            Some(code) => dshx_core::ToolError::coded("TeamError", code, self.message),
            None => dshx_core::ToolError::Failed(self.message),
        }
    }
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TeamError {}

fn failed(e: impl std::fmt::Display) -> TeamError {
    TeamError::plain(e.to_string())
}

/// One live Lead and its Team state.
pub(crate) struct Lead {
    pub agent: Agent,
    pub state: Mutex<TeamState>,
    /// Serializes read-check-append operations (dsh `TeamJournal.transact`).
    pub tx: tokio::sync::Mutex<()>,
    /// Bumped on every committed Team event and member status edge.
    pub activity: watch::Sender<u64>,
}

impl Lead {
    pub(crate) fn id(&self) -> String {
        self.agent.session_id()
    }

    /// The current state (folded up to the latest event).
    pub(crate) fn state(&self) -> Result<TeamState, TeamError> {
        let from = self.state.lock().unwrap().folded;
        let events = self.agent.events_from(from);
        let mut st = self.state.lock().unwrap();
        st.extend(&events);
        match &st.failure {
            Some(f) => Err(TeamError::plain(f.clone())),
            None => Ok(st.clone()),
        }
    }

    fn notify(&self) {
        self.activity.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// dsh `appendAndFlush`.
    async fn append(&self, kind: &str, data: Value) -> Result<(), TeamError> {
        self.agent
            .append_event(NewEvent::new(kind, data))
            .await
            .map_err(failed)?;
        self.agent.flush().await.map_err(failed)?;
        self.notify();
        Ok(())
    }
}

/// A caller's Team role (dsh `TeamMembership`).
#[derive(Clone)]
pub struct Membership {
    pub(crate) lead: Arc<Lead>,
    pub lead_id: String,
    pub is_lead: bool,
    pub name: String,
    pub agent: Agent,
}

/// Bumps a Team's activity on member status edges.
struct ActivityRelay(Weak<Lead>);

impl AgentObserver for ActivityRelay {
    fn on_status(&self, _status: &AgentStatus) {
        if let Some(lead) = self.0.upgrade() {
            lead.notify();
        }
    }
}

const MEMBER_LIMIT_NAME: usize = 64;

/// dsh `requiredText` (UTF-16 length).
fn required_text(value: &str, field: &str, max: usize) -> Result<String, TeamError> {
    let t = value.trim();
    if t.is_empty() {
        return Err(TeamError::new(
            "TEAM_INVALID_ARGUMENT",
            format!("{field} must be non-empty"),
        ));
    }
    if t.encode_utf16().count() > max {
        return Err(TeamError::new(
            "TEAM_INVALID_ARGUMENT",
            format!("{field} exceeds {max} characters"),
        ));
    }
    Ok(t.to_owned())
}

/// dsh `writeScope`: a normalized workspace-relative prefix.
fn write_scope(value: &str) -> Result<String, TeamError> {
    let mut n = value.replace('\\', "/");
    if let Some(rest) = n.strip_prefix("./") {
        n = rest.to_owned();
    }
    while n.ends_with('/') {
        n.pop();
    }
    let drive = n.len() >= 2 && n.as_bytes()[0].is_ascii_alphabetic() && n.as_bytes()[1] == b':';
    let bad = n.is_empty()
        || n.starts_with('/')
        || drive
        || n.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(TeamError::new(
            "TEAM_INVALID_WRITE_SCOPE",
            format!(
                "invalid workspace-relative write scope {}",
                serde_json::to_string(value).unwrap_or_default()
            ),
        ));
    }
    Ok(n)
}

/// `^[a-z0-9]+(?:-[a-z0-9]+)*$`
fn member_name_ok(v: &str) -> bool {
    !v.is_empty()
        && v.split('-').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn user_message(content: Vec<ContentBlock>, source: Value) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content,
        source: MessageSource::Other(source),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// The inherited prefix length of a seeded session.
fn own_events<'a>(
    header: &dshx_core::SessionHeader,
    events: &'a [SessionEvent],
) -> &'a [SessionEvent] {
    if !header.is_seeded {
        return events;
    }
    let cut = events
        .iter()
        .rposition(|e| e.kind == "session/end-seed" && e.data["inherited"] == true)
        .unwrap_or(0);
    &events[cut..]
}

/// dsh `messageAccepted`: model-visible or still pending in the inbox.
fn message_accepted(events: &[SessionEvent], predicate: &dyn Fn(&Value) -> bool) -> bool {
    if events
        .iter()
        .any(|e| e.kind == kinds::USER_MESSAGE && predicate(&e.data))
    {
        return true;
    }
    let mut turn: Vec<Value> = Vec::new();
    let mut step: Vec<Value> = Vec::new();
    for e in events.iter().filter(|e| e.kind == kinds::INBOX_SPLICED) {
        let list = if e.data["target"] == "next-step" {
            &mut step
        } else {
            &mut turn
        };
        let start = (e.data["start"].as_u64().unwrap_or(0) as usize).min(list.len());
        let removed =
            (e.data["removedCount"].as_u64().unwrap_or(0) as usize).min(list.len() - start);
        let inserted = e.data["inserted"].as_array().cloned().unwrap_or_default();
        list.splice(start..start + removed, inserted);
    }
    turn.iter().chain(step.iter()).any(predicate)
}

fn team_message_with(id: &str) -> impl Fn(&Value) -> bool + '_ {
    move |m: &Value| {
        m["source"]["kind"] == "team-message" && m["source"]["messageId"].as_str() == Some(id)
    }
}

/// The per-process Agent Teams service.
pub struct TeamService {
    settings: TeamSettings,
    pub(crate) subagents: Arc<SubagentRuntime>,
    sessions: Arc<dyn SessionFactory>,
    leads: Mutex<HashMap<String, Arc<Lead>>>,
    in_flight: Mutex<HashSet<String>>,
    dispatch_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    lifetime: CancellationToken,
    me: Weak<TeamService>,
}

impl TeamService {
    /// `sessions` must be the store the subagent runtime creates children in.
    pub fn new(
        settings: TeamSettings,
        subagents: Arc<SubagentRuntime>,
        sessions: Arc<dyn SessionFactory>,
    ) -> Arc<TeamService> {
        Arc::new_cyclic(|me| TeamService {
            settings,
            subagents,
            sessions,
            leads: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashSet::new()),
            dispatch_locks: Mutex::new(HashMap::new()),
            lifetime: CancellationToken::new(),
            me: me.clone(),
        })
    }

    pub fn settings(&self) -> &TeamSettings {
        &self.settings
    }

    fn arc(&self) -> Option<Arc<TeamService>> {
        self.me.upgrade()
    }

    /// Registers a top-level agent as the Lead of its implicit Team, then
    /// reconciles provisioning and retries its pending mailbox (dsh `recoverFor`).
    pub fn attach_lead(&self, agent: &Agent) {
        let lead = Arc::new(Lead {
            agent: agent.clone(),
            state: Mutex::new(TeamState::new(&agent.session_id())),
            tx: tokio::sync::Mutex::new(()),
            activity: watch::channel(0).0,
        });
        agent.add_observer(Arc::new(ActivityRelay(Arc::downgrade(&lead))));
        self.leads
            .lock()
            .unwrap()
            .insert(agent.session_id(), lead.clone());
        self.watch_receipts(agent);
        if let Some(me) = self.arc() {
            let agent = agent.clone();
            tokio::spawn(async move {
                if let Err(e) = me.reconcile(&lead).await {
                    tracing::warn!("Agent Teams recovery for \"{}\" failed: {e}", lead.id());
                }
                me.recover_mailbox(&agent).await;
            });
        }
    }

    /// Forgets a Lead (after its agent was closed).
    pub fn detach(&self, session_id: &str) {
        self.leads.lock().unwrap().remove(session_id);
    }

    /// Hooks a freshly built child agent: Team status edges, delivery
    /// receipts and the retry of messages queued for it.
    pub fn on_child(&self, child: &Agent) {
        let Some(parent) = child
            .header()
            .extra
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        let Some(lead) = self.leads.lock().unwrap().get(&parent).cloned() else {
            return;
        };
        child.add_observer(Arc::new(ActivityRelay(Arc::downgrade(&lead))));
        self.watch_receipts(child);
        if let Some(me) = self.arc() {
            let child = child.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                me.recover_mailbox(&child).await;
            });
        }
    }

    /// dsh `observeSessionEvent`: a target's durable receipt is checkpointed
    /// into the Lead log as `team/message/delivered`.
    fn watch_receipts(&self, agent: &Agent) {
        let Some(me) = self.arc() else { return };
        let mut rx = agent.subscribe();
        let target = agent.clone();
        let lifetime = self.lifetime.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    e = rx.recv() => e,
                    _ = lifetime.cancelled() => return,
                };
                let event = match event {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                };
                let source = &event.data["source"];
                if event.kind != kinds::USER_MESSAGE || source["kind"] != "team-message" {
                    continue;
                }
                let (Some(team), Some(id)) =
                    (source["teamId"].as_str(), source["messageId"].as_str())
                else {
                    continue;
                };
                let lead = me.leads.lock().unwrap().get(team).cloned();
                if let Some(lead) = lead
                    && let Err(e) = me.checkpoint_delivered(&lead, &target, id).await
                {
                    tracing::warn!("Team message \"{id}\" acknowledgement failed: {e}");
                }
            }
        });
    }

    /// dsh `tryMembership`.
    pub fn membership(&self, session_id: &str) -> Option<Membership> {
        if let Some(lead) = self.leads.lock().unwrap().get(session_id).cloned() {
            return Some(Membership {
                lead_id: lead.id(),
                agent: lead.agent.clone(),
                lead,
                is_lead: true,
                name: "lead".into(),
            });
        }
        let child = self.subagents.child(session_id)?;
        let parent = child
            .header()
            .extra
            .get("parentSession")?
            .as_str()?
            .to_owned();
        let lead = self.leads.lock().unwrap().get(&parent).cloned()?;
        let state = lead.state().ok()?;
        let member = state
            .members
            .iter()
            .find(|m| m.id == session_id && (m.phase == "active" || m.phase == "provisioning"))?;
        Some(Membership {
            lead_id: lead.id(),
            name: member.name.clone(),
            lead,
            is_lead: false,
            agent: child,
        })
    }

    /// dsh `membership` (throwing).
    pub(crate) fn require(&self, session_id: &str) -> Result<Membership, TeamError> {
        self.membership(session_id).ok_or_else(|| {
            TeamError::new(
                "TEAM_NOT_MEMBER",
                format!("agent \"{session_id}\" is not a member of an active Agent Team"),
            )
        })
    }

    fn availability(&self, id: &str, lead: &Lead) -> &'static str {
        let status = if id == lead.id() {
            Some(lead.agent.status())
        } else {
            self.subagents.child(id).map(|a| a.status())
        };
        match status {
            Some(AgentStatus::Running | AgentStatus::WaitingApproval) => "running",
            _ => "inactive",
        }
    }

    fn model_of(&self, id: &str) -> Option<String> {
        self.subagents.child(id).map(|a| a.config().model)
    }

    /// dsh `roster.list` → model rows (`target` = name).
    pub fn list_members(&self, m: &Membership) -> Result<Vec<Value>, TeamError> {
        let lead = &m.lead;
        let state = lead.state()?;
        let lead_model = lead.agent.config().model;
        let mut rows = vec![json!({
            "target": "lead",
            "role": "lead",
            "status": self.availability(&lead.id(), lead),
            "model": lead_model,
            "diagnostics": [],
        })];
        for member in &state.members {
            let status = match member.phase.as_str() {
                "failed" => "failed",
                "provisioning" => "provisioning",
                _ => self.availability(&member.id, lead),
            };
            let model = self
                .model_of(&member.id)
                .unwrap_or_else(|| lead_model.clone());
            rows.push(json!({
                "target": member.name,
                "role": "teammate",
                "status": status,
                "description": member.description,
                "provider": member.provider,
                "context": member.context,
                "model": model,
                "diagnostics": member.error.iter().collect::<Vec<_>>(),
            }));
        }
        Ok(rows)
    }

    /// The Team of a Lead session for UIs: the durable projection plus the
    /// live roster (`None` when no Lead with that id is attached).
    pub fn team_view(&self, lead_id: &str) -> Option<Result<Value, TeamError>> {
        let lead = self.leads.lock().unwrap().get(lead_id).cloned()?;
        Some((|| {
            let state = lead.state()?;
            let mut view = state.projection();
            let membership = Membership {
                lead_id: lead.id(),
                agent: lead.agent.clone(),
                lead: lead.clone(),
                is_lead: true,
                name: "lead".into(),
            };
            view["roster"] = Value::Array(self.list_members(&membership)?);
            Ok(view)
        })())
    }

    fn resolve_active(
        &self,
        state: &TeamState,
        lead_id: &str,
        raw: &str,
    ) -> Result<(String, String), TeamError> {
        let name = raw.trim();
        if name == "lead" {
            return Ok((lead_id.to_owned(), name.to_owned()));
        }
        state
            .members
            .iter()
            .find(|m| m.name == name && m.phase == "active")
            .map(|m| (m.id.clone(), name.to_owned()))
            .ok_or_else(|| {
                TeamError::new(
                    "TEAM_MEMBER_NOT_FOUND",
                    format!("active teammate \"{name}\" not found"),
                )
            })
    }

    // ---- roster ----

    /// dsh `spawnTeammate`: one named continuable child of the Lead.
    pub async fn spawn(
        &self,
        caller: &Membership,
        name: &str,
        description: &str,
        prompt: &str,
        context: &str,
        cancel: &CancellationToken,
    ) -> Result<Value, TeamError> {
        if !caller.is_lead {
            return Err(TeamError::new(
                "TEAM_LEAD_REQUIRED",
                "only the Team Lead can create teammates",
            ));
        }
        if cancel.is_cancelled() {
            return Err(TeamError::plain("This operation was aborted"));
        }
        let lead = caller.lead.clone();
        if !member_name_ok(name)
            || name.encode_utf16().count() > MEMBER_LIMIT_NAME
            || name == "lead"
        {
            return Err(TeamError::new(
                "TEAM_INVALID_MEMBER_NAME",
                "teammate name must be lower-kebab-case, at most 64 characters, and not \"lead\"",
            ));
        }
        let description = required_text(description, "description", 200)?;
        let provider = if context == "fork" {
            self.settings.fork_provider.clone()
        } else {
            self.settings.fresh_provider.clone()
        };
        let child_id = uuid::Uuid::new_v4().to_string();
        let member = Member {
            id: child_id.clone(),
            name: name.to_owned(),
            description: description.clone(),
            provider: required_text(&provider, "provider", 200)?,
            context: context.to_owned(),
            phase: "provisioning".into(),
            error: None,
        };
        {
            let _tx = lead.tx.lock().await;
            let state = lead.state()?;
            if state.members.iter().any(|m| m.name == name) {
                return Err(TeamError::new(
                    "TEAM_MEMBER_NAME_TAKEN",
                    format!("teammate name \"{name}\" was already used in this Team"),
                ));
            }
            if state.members.len() >= self.settings.max_members {
                return Err(TeamError::new(
                    "TEAM_MEMBER_LIMIT",
                    format!("Team member limit {} reached", self.settings.max_members),
                ));
            }
            lead.append(
                TEAM_MEMBER,
                json!({"version": 2, "teamId": lead.id(), "member": member.to_json()}),
            )
            .await?;
        }
        let reminder = format!(
            "<system-reminder>\nYou are teammate \"{}\".\nYour Team Lead is named \"lead\".\nUse list_agents({{}}) to find your teammates and their names.\nTo message your Team Lead, use send_message({{ target: \"lead\", message: \"...\" }}).\nTo message another teammate, use send_message({{ target: \"<teammate name>\", message: \"...\" }}).\n</system-reminder>\n\n",
            name.trim()
        );
        let started = self
            .subagents
            .start_continuable_child(
                &lead.id(),
                ContinuableSpec {
                    child_id: Some(child_id.clone()),
                    provider: member.provider.clone(),
                    label: description.clone(),
                    prompt: vec![ContentBlock::text(reminder), ContentBlock::text(prompt)],
                    max_depth: Some(self.subagents.settings().max_depth),
                },
            )
            .await
            .map_err(|e| TeamError::plain(e.message));
        let started = match started {
            Ok((_, message_id)) => {
                self.checkpoint_initial(&child_id, &message_id, cancel)
                    .await
            }
            Err(e) => Err(e),
        };
        if let Err(error) = started {
            let failed_member = Member {
                phase: "failed".into(),
                error: Some(error.message.clone()),
                ..member.clone()
            };
            self.settle(&lead, failed_member).await?;
            let _ = self
                .subagents
                .drain_children(&lead.id(), std::slice::from_ref(&child_id))
                .await;
            return Err(error);
        }
        let active = Member {
            phase: "active".into(),
            ..member
        };
        if self.settle(&lead, active.clone()).await? == "failed" {
            let _ = self
                .subagents
                .drain_children(&lead.id(), std::slice::from_ref(&child_id))
                .await;
            return Err(TeamError::new(
                "TEAM_PROVISIONING_CONFLICT",
                format!(
                    "teammate \"{name}\" was reconciled as failed while creation was in progress"
                ),
            ));
        }
        let mut row = json!({
            "target": active.name,
            "role": "teammate",
            "status": self.availability(&active.id, &lead),
            "description": active.description,
            "provider": active.provider,
            "context": active.context,
        });
        if let Some(model) = self.model_of(&active.id) {
            row["model"] = model.into();
        }
        row["diagnostics"] = json!([]);
        Ok(json!({"member": row}))
    }

    /// dsh `checkpointInitialPrompt`: the accepted initial prompt is durable
    /// before the Lead may record the member as active.
    async fn checkpoint_initial(
        &self,
        child_id: &str,
        message_id: &str,
        cancel: &CancellationToken,
    ) -> Result<(), TeamError> {
        let accepted = |events: &[SessionEvent]| {
            message_accepted(events, &|m: &Value| m["id"].as_str() == Some(message_id))
        };
        loop {
            if cancel.is_cancelled() {
                return Err(TeamError::new("TEAM_DISPOSED", "teammate creation aborted"));
            }
            let Some(child) = self.subagents.child(child_id) else {
                let (header, events) = self.sessions.read(child_id).await.map_err(failed)?;
                if accepted(own_events(&header, &events)) {
                    return Ok(());
                }
                return Err(TeamError::new(
                    "TEAM_PROVISIONING_CONFLICT",
                    format!("teammate \"{child_id}\" initial prompt was not durably accepted"),
                ));
            };
            let mut rx = child.subscribe();
            child.flush().await.map_err(failed)?;
            if accepted(own_events(&child.header(), &child.events())) {
                return Ok(());
            }
            tokio::select! {
                _ = rx.recv() => {}
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    /// dsh `settleProvisioning`: one terminal edge unless recovery settled it.
    async fn settle(&self, lead: &Lead, terminal: Member) -> Result<&'static str, TeamError> {
        let _tx = lead.tx.lock().await;
        let state = lead.state()?;
        let Some(current) = state.members.iter().find(|m| m.id == terminal.id) else {
            return Err(TeamError::new(
                "TEAM_PROVISIONING_CONFLICT",
                format!("provisioned teammate \"{}\" disappeared", terminal.id),
            ));
        };
        if current.phase != "provisioning" {
            return Ok(if current.phase == "active" {
                "active"
            } else {
                "failed"
            });
        }
        let phase = if terminal.phase == "active" {
            "active"
        } else {
            "failed"
        };
        lead.append(
            TEAM_MEMBER,
            json!({"version": 2, "teamId": lead.id(), "member": terminal.to_json()}),
        )
        .await?;
        Ok(phase)
    }

    /// dsh `reconcileProvisioning`: members left provisioning by a crash.
    async fn reconcile(&self, lead: &Arc<Lead>) -> Result<(), TeamError> {
        let provisioning: Vec<Member> = lead
            .state()?
            .members
            .into_iter()
            .filter(|m| m.phase == "provisioning")
            .collect();
        for member in provisioning {
            if self.subagents.child(&member.id).is_some() {
                continue;
            }
            let (phase, failure) = match self.sessions.read(&member.id).await {
                Ok((header, events)) => {
                    let own = own_events(&header, &events);
                    let descriptor = subagent_descriptor(&header, &events);
                    let ok = header.extra.get("parentSession").and_then(Value::as_str)
                        == Some(lead.id().as_str())
                        && descriptor.as_ref().is_some_and(|d| {
                            d["mode"] == "continuable" && d["provider"] == json!(member.provider)
                        })
                        && message_accepted(own, &|m: &Value| m["source"]["kind"] == "user");
                    if ok {
                        ("active", None)
                    } else {
                        ("failed", Some("persisted child Session does not match the provisioned continuation".to_owned()))
                    }
                }
                Err(e) => (
                    "failed",
                    Some(format!("child Session recovery failed: {e:#}")),
                ),
            };
            let _tx = lead.tx.lock().await;
            let state = lead.state()?;
            let Some(current) = state.members.iter().find(|m| m.id == member.id) else {
                continue;
            };
            if current.phase != "provisioning" {
                continue;
            }
            let settled = Member {
                phase: phase.into(),
                error: failure.or_else(|| current.error.clone()),
                ..current.clone()
            };
            lead.append(
                TEAM_MEMBER,
                json!({"version": 2, "teamId": lead.id(), "member": settled.to_json()}),
            )
            .await?;
        }
        Ok(())
    }

    /// dsh `interrupt`: the Lead stops one teammate's turn, keeping its inbox.
    pub fn interrupt(&self, caller: &Membership, target: &str) -> Result<Value, TeamError> {
        if !caller.is_lead {
            return Err(TeamError::new(
                "TEAM_LEAD_REQUIRED",
                "only the Team Lead can interrupt teammates",
            ));
        }
        let state = caller.lead.state()?;
        let (id, _) = self.resolve_active(&state, &caller.lead_id, target)?;
        if id == caller.lead_id {
            return Err(TeamError::new(
                "TEAM_INVALID_TARGET",
                "the Team Lead cannot interrupt itself",
            ));
        }
        if self.subagents.child(&id).is_none() {
            return Ok(json!({"previousStatus": "inactive"}));
        }
        let previous = self.availability(&id, &caller.lead);
        self.subagents
            .interrupt(&id, &caller.lead_id)
            .map_err(|e| TeamError::plain(e.message))?;
        Ok(json!({"previousStatus": previous}))
    }

    /// dsh `waitForChange` with the tool's no-active-peer shortcut.
    pub async fn wait(
        &self,
        caller: &Membership,
        timeout_ms: i64,
        cancel: &CancellationToken,
    ) -> Result<Value, TeamError> {
        if !(10_000..=3_600_000).contains(&timeout_ms) {
            return Err(TeamError::new(
                "TEAM_INVALID_TIMEOUT",
                "timeoutMs must be an integer from 10000 through 3600000",
            ));
        }
        let rows = self.list_members(caller)?;
        let caller_target = caller.name.as_str();
        let active_peer = rows.iter().any(|r| {
            r["target"].as_str() != Some(caller_target)
                && matches!(r["status"].as_str(), Some("running" | "provisioning"))
        });
        if !active_peer {
            return Ok(
                json!({"timedOut": false, "noProgress": {"reason": "no-active-peer", "message": crate::tools::NO_ACTIVE_PEER_MESSAGE}}),
            );
        }
        let mut rx = caller.lead.activity.subscribe();
        rx.borrow_and_update();
        tokio::select! {
            changed = rx.changed() => {
                let _ = changed;
                Ok(json!({"timedOut": false}))
            }
            _ = tokio::time::sleep(Duration::from_millis(timeout_ms as u64)) => Ok(json!({"timedOut": true})),
            _ = cancel.cancelled() => Err(TeamError::new("TEAM_WAIT_ABORTED", "wait_agent aborted")),
        }
    }

    // ---- mailbox ----

    fn delivery_content(message: &QueuedMessage) -> Vec<Value> {
        let mut content = vec![
            json!({"type": "text", "text": format!("Team message {} from {}:", message.id, message.sender_name)}),
        ];
        content.extend(message.content.iter().cloned());
        content
    }

    /// dsh `mailbox.send`: queue durably, then attempt delivery.
    pub async fn send(
        &self,
        caller: &Membership,
        target: &str,
        text: &str,
    ) -> Result<Value, TeamError> {
        let lead = caller.lead.clone();
        let content = vec![json!({"type": "text", "text": text})];
        let queued = {
            let _tx = lead.tx.lock().await;
            let state = lead.state()?;
            let (target_id, target_name) = self.resolve_active(&state, &caller.lead_id, target)?;
            if target_id == caller.agent.session_id() {
                return Err(TeamError::new(
                    "TEAM_SELF_MESSAGE",
                    "a Team member cannot message itself",
                ));
            }
            let pending = state
                .undelivered()
                .filter(|m| m.target_id == target_id)
                .count();
            if pending >= self.settings.max_pending_messages_per_member {
                return Err(TeamError::new(
                    "TEAM_MAILBOX_FULL",
                    format!("teammate \"{target_name}\" has {pending} pending messages"),
                ));
            }
            let queued = QueuedMessage {
                id: format!("team-message-{}", uuid::Uuid::new_v4()),
                sender_id: caller.agent.session_id(),
                sender_name: caller.name.clone(),
                target_id,
                content,
            };
            let size = serde_json::to_string(&Self::delivery_content(&queued))
                .unwrap_or_default()
                .len();
            if size > self.settings.max_message_bytes {
                return Err(TeamError::new(
                    "TEAM_MESSAGE_TOO_LARGE",
                    format!(
                        "team message exceeds {} bytes",
                        self.settings.max_message_bytes
                    ),
                ));
            }
            lead.append(
                TEAM_MESSAGE_QUEUED,
                json!({"version": 2, "teamId": lead.id(), "message": queued.to_json()}),
            )
            .await?;
            queued
        };
        let accepted = self.try_dispatch(&lead, &queued).await;
        Ok(json!({"messageId": queued.id, "status": if accepted { "accepted" } else { "queued" }}))
    }

    /// dsh `recoverFor` (mailbox): retry the member's undelivered messages.
    async fn recover_mailbox(&self, agent: &Agent) {
        let Some(m) = self.membership(&agent.session_id()) else {
            return;
        };
        let Ok(state) = m.lead.state() else { return };
        let id = agent.session_id();
        let pending: Vec<QueuedMessage> = state
            .undelivered()
            .filter(|q| m.is_lead || q.target_id == id)
            .cloned()
            .collect();
        for q in pending {
            self.try_dispatch(&m.lead, &q).await;
        }
    }

    /// One delivery attempt per message at a time in this process.
    async fn try_dispatch(&self, lead: &Arc<Lead>, message: &QueuedMessage) -> bool {
        if !self.in_flight.lock().unwrap().insert(message.id.clone()) {
            return false;
        }
        let lock = self
            .dispatch_locks
            .lock()
            .unwrap()
            .entry(message.target_id.clone())
            .or_default()
            .clone();
        let result = {
            let _guard = lock.lock().await;
            self.dispatch_through(lead, message).await
        };
        self.in_flight.lock().unwrap().remove(&message.id);
        result
    }

    /// dsh `dispatchThrough`: deliver every earlier pending message of the
    /// same target first, in durable queue order.
    async fn dispatch_through(&self, lead: &Arc<Lead>, message: &QueuedMessage) -> bool {
        let Ok(state) = lead.state() else {
            return false;
        };
        let pending: Vec<QueuedMessage> = state
            .undelivered()
            .filter(|m| m.target_id == message.target_id)
            .cloned()
            .collect();
        let Some(index) = pending.iter().position(|m| m.id == message.id) else {
            return state.delivered.contains(&message.id);
        };
        for candidate in &pending[..=index] {
            let owns = candidate.id == message.id
                || self.in_flight.lock().unwrap().insert(candidate.id.clone());
            let ok = self.dispatch_once(lead, candidate).await;
            if owns && candidate.id != message.id {
                self.in_flight.lock().unwrap().remove(&candidate.id);
            }
            if !ok {
                return false;
            }
        }
        true
    }

    fn target_agent(&self, lead: &Lead, id: &str) -> Option<Agent> {
        if id == lead.id() {
            Some(lead.agent.clone())
        } else {
            self.subagents.child(id)
        }
    }

    /// dsh `dispatchOnce`.
    async fn dispatch_once(&self, lead: &Arc<Lead>, message: &QueuedMessage) -> bool {
        let result: Result<bool, TeamError> = async {
            let target = self.target_agent(lead, &message.target_id);
            if let Some(t) = &target
                && message_accepted(
                    own_events(&t.header(), &t.events()),
                    &team_message_with(&message.id),
                )
            {
                return self.checkpoint_delivered(lead, t, &message.id).await;
            }
            let source = json!({
                "kind": "team-message",
                "teamId": lead.id(),
                "messageId": message.id,
                "senderId": message.sender_id,
                "senderName": message.sender_name,
            });
            let content: Vec<ContentBlock> = Self::delivery_content(message)
                .into_iter()
                .filter_map(|b| serde_json::from_value(b).ok())
                .collect();
            let input = user_message(content, source);
            if message.target_id == lead.id() {
                lead.agent.steer_detached(input).await.map_err(failed)?;
                return self
                    .checkpoint_delivered(lead, &lead.agent, &message.id)
                    .await;
            }
            if target.is_none() {
                match self.sessions.read(&message.target_id).await {
                    Err(e) => {
                        tracing::warn!(
                            "cannot read Team message target \"{}\": {e:#}",
                            message.target_id
                        );
                        return Ok(false);
                    }
                    Ok((header, events)) => {
                        if message_accepted(
                            own_events(&header, &events),
                            &team_message_with(&message.id),
                        ) {
                            self.mark_delivered(lead, &message.id, &message.target_id)
                                .await?;
                            return Ok(true);
                        }
                    }
                }
            }
            self.subagents
                .steer_prompt(&lead.id(), &message.target_id, input)
                .await
                .map_err(|e| TeamError::plain(e.message))?;
            match target {
                None => Ok(true),
                Some(t) => self.checkpoint_delivered(lead, &t, &message.id).await,
            }
        }
        .await;
        match result {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!("team message \"{}\" remains queued: {e}", message.id);
                false
            }
        }
    }

    /// dsh `checkpointDelivered`: flush the target's receipt, then record delivery.
    async fn checkpoint_delivered(
        &self,
        lead: &Arc<Lead>,
        target: &Agent,
        id: &str,
    ) -> Result<bool, TeamError> {
        target.flush().await.map_err(failed)?;
        if !message_accepted(
            own_events(&target.header(), &target.events()),
            &team_message_with(id),
        ) {
            return Ok(false);
        }
        self.mark_delivered(lead, id, &target.session_id()).await?;
        Ok(true)
    }

    async fn mark_delivered(
        &self,
        lead: &Arc<Lead>,
        id: &str,
        target: &str,
    ) -> Result<(), TeamError> {
        let _tx = lead.tx.lock().await;
        let state = lead.state()?;
        if state.delivered.iter().any(|d| d == id) {
            return Ok(());
        }
        if !state
            .messages
            .iter()
            .any(|m| m.id == id && m.target_id == target)
        {
            return Ok(());
        }
        lead.append(
            TEAM_MESSAGE_DELIVERED,
            json!({"version": 2, "teamId": lead.id(), "messageId": id, "targetId": target}),
        )
        .await
    }

    // ---- task board ----

    fn dependencies(
        &self,
        values: &[String],
        state: &TeamState,
        own: Option<&str>,
    ) -> Result<Vec<String>, TeamError> {
        let mut out: Vec<String> = Vec::new();
        for id in values {
            if Some(id.as_str()) == own {
                return Err(TeamError::new(
                    "TEAM_TASK_DEPENDENCY_CYCLE",
                    "a team task cannot block itself",
                ));
            }
            if out.contains(id) {
                return Err(TeamError::new(
                    "TEAM_INVALID_ARGUMENT",
                    format!("duplicate blocker \"{id}\""),
                ));
            }
            if !state
                .tasks
                .iter()
                .any(|t| t.id == *id && t.status != "deleted")
            {
                return Err(TeamError::new(
                    "TEAM_TASK_NOT_FOUND",
                    format!("blocker task \"{id}\" not found"),
                ));
            }
            out.push(id.clone());
        }
        Ok(out)
    }

    fn write_scopes(values: &[String]) -> Result<Vec<String>, TeamError> {
        let mut out: Vec<String> = Vec::new();
        for v in values {
            let s = write_scope(v)?;
            if !out.contains(&s) {
                out.push(s);
            }
        }
        Ok(out)
    }

    fn assert_graph(state: &TeamState, task: &Task) -> Result<(), TeamError> {
        assert_task_graph(&state.tasks, task).map_err(|(message, v)| {
            let code = match v {
                GraphViolation::Missing => "TEAM_TASK_NOT_FOUND",
                GraphViolation::Duplicate => "TEAM_INVALID_ARGUMENT",
                GraphViolation::Cycle => "TEAM_TASK_DEPENDENCY_CYCLE",
            };
            TeamError::new(code, message)
        })
    }

    /// dsh `tasks.create`.
    pub async fn create_task(
        &self,
        caller: &Membership,
        subject: &str,
        description: &str,
        blocked_by: Option<Vec<String>>,
        write_scopes: Option<Vec<String>>,
    ) -> Result<Value, TeamError> {
        let lead = caller.lead.clone();
        let _tx = lead.tx.lock().await;
        let state = lead.state()?;
        if state.tasks.iter().filter(|t| t.status != "deleted").count() >= self.settings.max_tasks {
            return Err(TeamError::new(
                "TEAM_TASK_LIMIT",
                format!("Team task limit {} reached", self.settings.max_tasks),
            ));
        }
        let id = format!("task-{}", state.next_task_number);
        if state.tasks.iter().any(|t| t.id == id) {
            return Err(TeamError::new(
                "TEAM_TASK_LIMIT",
                "Team task id space exhausted",
            ));
        }
        let task = Task {
            id,
            revision: 1,
            subject: required_text(subject, "subject", 200)?,
            description: required_text(description, "description", 16_384)?,
            status: "pending".into(),
            owner_id: None,
            blocked_by: self.dependencies(&blocked_by.unwrap_or_default(), &state, None)?,
            write_scopes: Self::write_scopes(&write_scopes.unwrap_or_default())?,
        };
        Self::assert_graph(&state, &task)?;
        lead.append(
            TEAM_TASK,
            json!({"version": 2, "teamId": lead.id(), "task": task.to_json(false)}),
        )
        .await?;
        Ok(state.task_view(&task))
    }

    /// dsh `tasks.get` (deleted tombstones included).
    pub fn get_task(&self, caller: &Membership, id: &str) -> Result<Value, TeamError> {
        let state = caller.lead.state()?;
        let task = state.tasks.iter().find(|t| t.id == id).ok_or_else(|| {
            TeamError::new(
                "TEAM_TASK_NOT_FOUND",
                format!("team task \"{id}\" not found"),
            )
        })?;
        Ok(state.task_view(task))
    }

    /// dsh `tasks.list`: non-deleted views in creation order.
    pub fn list_tasks(&self, caller: &Membership) -> Result<Vec<Value>, TeamError> {
        let state = caller.lead.state()?;
        Ok(state
            .tasks
            .iter()
            .filter(|t| t.status != "deleted")
            .map(|t| state.task_view(t))
            .collect())
    }

    /// dsh `tasks.update`: one compare-and-set transition.
    pub async fn update_task(
        &self,
        caller: &Membership,
        request: TaskUpdate,
    ) -> Result<Value, TeamError> {
        let lead = caller.lead.clone();
        let _tx = lead.tx.lock().await;
        let state = lead.state()?;
        let current = state
            .tasks
            .iter()
            .find(|t| t.id == request.task_id)
            .cloned()
            .ok_or_else(|| {
                TeamError::new(
                    "TEAM_TASK_NOT_FOUND",
                    format!("team task \"{}\" not found", request.task_id),
                )
            })?;
        if current.revision as i64 != request.expected_revision {
            return Err(TeamError::new(
                "TEAM_TASK_STALE_REVISION",
                format!(
                    "stale team task \"{}\" revision {}; current revision is {}",
                    current.id, request.expected_revision, current.revision
                ),
            ));
        }
        if current.status == "deleted" {
            return Err(TeamError::new(
                "TEAM_TASK_DELETED",
                format!("team task \"{}\" is deleted", current.id),
            ));
        }
        let caller_id = caller.agent.session_id();
        let owner = current.owner_id.as_deref() == Some(caller_id.as_str());
        let authorize = || {
            if caller.is_lead || owner {
                Ok(())
            } else {
                Err(TeamError::new(
                    "TEAM_TASK_UNAUTHORIZED",
                    "task mutation requires its owner or Team Lead",
                ))
            }
        };
        let had_owner = current.owner_id.is_some();
        let mut next = current.clone();
        let transition = |m: &str| TeamError::new("TEAM_TASK_INVALID_TRANSITION", m);
        match request.action.as_str() {
            "claim" => {
                if current.owner_id.as_ref().is_some_and(|o| *o != caller_id) {
                    return Err(TeamError::new(
                        "TEAM_TASK_ALREADY_CLAIMED",
                        format!("team task \"{}\" is owned by another member", current.id),
                    ));
                }
                if current.status != "pending" || !state.task_ready(&current) {
                    return Err(TeamError::new(
                        "TEAM_TASK_BLOCKED",
                        format!("team task \"{}\" is not ready to claim", current.id),
                    ));
                }
                next.status = "in_progress".into();
                next.owner_id = Some(caller_id.clone());
            }
            "release" => {
                authorize()?;
                if current.status != "in_progress" {
                    return Err(transition("only an in-progress task can be released"));
                }
                next.status = "pending".into();
                next.owner_id = None;
            }
            "edit" => {
                authorize()?;
                if request.subject.is_none()
                    && request.description.is_none()
                    && request.write_scopes.is_none()
                {
                    return Err(TeamError::new(
                        "TEAM_INVALID_ARGUMENT",
                        "task edit requires subject, description, or write_scopes",
                    ));
                }
                if let Some(s) = &request.subject {
                    next.subject = required_text(s, "subject", 200)?;
                }
                if let Some(d) = &request.description {
                    next.description = required_text(d, "description", 16_384)?;
                }
                if let Some(w) = &request.write_scopes {
                    next.write_scopes = Self::write_scopes(w)?;
                }
            }
            "set_dependencies" => {
                authorize()?;
                let Some(b) = &request.blocked_by else {
                    return Err(TeamError::new(
                        "TEAM_INVALID_ARGUMENT",
                        "set_dependencies requires blocked_by",
                    ));
                };
                next.blocked_by = self.dependencies(b, &state, Some(&current.id))?;
            }
            "complete" => {
                authorize()?;
                if current.status != "in_progress" {
                    return Err(transition("only an in-progress task can complete"));
                }
                next.status = "completed".into();
            }
            "reopen" => {
                authorize()?;
                if current.status != "completed" {
                    return Err(transition("only a completed task can reopen"));
                }
                next.status = "pending".into();
                next.owner_id = None;
            }
            "reassign" => {
                if !caller.is_lead {
                    return Err(TeamError::new(
                        "TEAM_LEAD_REQUIRED",
                        "only the Team Lead can reassign tasks",
                    ));
                }
                if current.status != "pending" && current.status != "in_progress" {
                    return Err(transition(
                        "only a pending or in-progress task can be reassigned",
                    ));
                }
                match request.owner.as_deref().filter(|o| !o.trim().is_empty()) {
                    None => {
                        next.status = "pending".into();
                        next.owner_id = None;
                    }
                    Some(o) => {
                        if !state.task_ready(&current) {
                            return Err(TeamError::new(
                                "TEAM_TASK_BLOCKED",
                                format!("team task \"{}\" is blocked", current.id),
                            ));
                        }
                        let (id, _) = self.resolve_active(&state, &caller.lead_id, o)?;
                        next.status = "in_progress".into();
                        next.owner_id = Some(id);
                    }
                }
            }
            "delete" => {
                authorize()?;
                if let Some(d) = state.tasks.iter().find(|t| {
                    t.status != "deleted"
                        && t.id != current.id
                        && t.blocked_by.contains(&current.id)
                }) {
                    return Err(TeamError::new(
                        "TEAM_TASK_HAS_DEPENDENTS",
                        format!("team task \"{}\" still blocks \"{}\"", current.id, d.id),
                    ));
                }
                next.status = "deleted".into();
            }
            other => {
                return Err(TeamError::new(
                    "TEAM_INVALID_ARGUMENT",
                    format!("unsupported task action {other}"),
                ));
            }
        }
        next.revision = current.revision + 1;
        Self::assert_graph(&state, &next)?;
        let owner_last = !had_owner && next.owner_id.is_some();
        lead.append(
            TEAM_TASK,
            json!({"version": 2, "teamId": lead.id(), "task": next.to_json(owner_last)}),
        )
        .await?;
        Ok(state.task_view(&next))
    }

    /// Stops background receipt watchers (process teardown).
    pub fn shutdown(&self) {
        self.lifetime.cancel();
    }
}

/// One `team_task_update` request (dsh `TeamTaskUpdateRequest`).
#[derive(Debug, Clone, Default)]
pub struct TaskUpdate {
    pub task_id: String,
    pub expected_revision: i64,
    pub action: String,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub blocked_by: Option<Vec<String>>,
    pub write_scopes: Option<Vec<String>>,
    pub owner: Option<String>,
}

/// The durable Team view of a stored Lead session (no live roster).
pub fn stored_team(lead_id: &str, events: &[SessionEvent]) -> Value {
    TeamState::from_events(lead_id, events).projection()
}
