//! The host-wide schedule service (dsh `ScheduleService` + `ScheduleRuntime`):
//! management never activates a session; the runtime delivers each due
//! occurrence as a follow-up into its original session, flushes that
//! session, and only then commits the task's next target and receipt.
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use dshx_agent::Agent;
use dshx_core::{ContentBlock, Message, MessageSource, Role};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, broadcast};

use crate::InputError;
use crate::record::{
    self, Record, Selector, batch_text, create_record, parse_at, parse_cron_input, parse_daily,
    parse_weekly, reminder_text, resolve_occurrence, schedule_title,
};
use crate::store::{History, Receipt, Status, Store, Task};
use crate::time::iso;

/// Largest timer delay (Node clamps beyond it; one re-check per ~24 days).
const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;
const DAY_MS: i64 = 86_400_000;

/// Delivery-history retention (dsh `deliveryHistoryDays`/`deliveryHistoryRecords`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub days: u32,
    pub records: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            days: 30,
            records: 200,
        }
    }
}

/// Wall clock in epoch milliseconds (injectable for tests).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Opens the live agent of a stored session for delivery (the host's
/// session controller: resume from disk when it is not open).
#[async_trait]
pub trait SessionResolver: Send + Sync {
    async fn resolve(&self, session_id: &str) -> anyhow::Result<Agent>;
}

/// The schedule service of one process.
pub struct ScheduleService {
    store: Store,
    retention: Retention,
    clock: Clock,
    tasks: Mutex<Vec<Task>>,
    /// Serializes management writes and delivery (dsh `serialize`).
    ops: tokio::sync::Mutex<()>,
    changed: broadcast::Sender<()>,
    resolver: Mutex<Option<Arc<dyn SessionResolver>>>,
    wake: Notify,
    stopping: AtomicBool,
    runtime: Mutex<Option<tokio::task::JoinHandle<()>>>,
    me: Weak<ScheduleService>,
}

fn user_message(text: String) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(text)],
        source: MessageSource::Other(json!({"kind": "schedule"})),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// dsh `appendDelivery`: append, then prune to the window and record cap.
fn append_delivery(task: &Task, receipt: Receipt, retention: Retention) -> (Receipt, History) {
    let history = task.history_or_legacy();
    let mut appended = history.records.clone();
    appended.push((receipt.clone(), Some(task.record.prompt.clone())));
    let delivered = crate::time::parse_iso(&receipt.delivered_at).unwrap_or(0);
    let floor = delivered - i64::from(retention.days) * DAY_MS;
    let mut retained: Vec<_> = appended
        .iter()
        .filter(|(r, _)| crate::time::parse_iso(&r.delivered_at).unwrap_or(0) >= floor)
        .cloned()
        .collect();
    let cap = retention.records as usize;
    if retained.len() > cap {
        retained.drain(..retained.len() - cap);
    }
    let pruned = retained.len() != appended.len();
    (
        receipt,
        History {
            records: retained,
            earlier_unavailable: history.earlier_unavailable || pruned,
            earlier_pruned: Some(history.earlier_pruned == Some(true) || pruned),
        },
    )
}

/// One `/api/schedules` row (dsh `catalog()` entry).
fn catalog_row(task: &Task) -> Value {
    let mut v = task.record.to_json();
    v["sessionId"] = task.session_id.clone().into();
    v["status"] = task.status.as_str().into();
    if let Some(r) = &task.last_delivery {
        v["lastDelivery"] = json!({"scheduledAt": r.scheduled_at, "deliveredAt": r.delivered_at, "messageId": r.message_id});
    }
    v
}

impl ScheduleService {
    /// Opens `<root>/schedule.json` (malformed rows refuse to open, like dsh).
    pub fn open(
        root: &Path,
        retention: Retention,
        clock: Option<Clock>,
    ) -> anyhow::Result<Arc<ScheduleService>> {
        let store = Store::new(root);
        let tasks = store.load()?;
        let clock = clock.unwrap_or_else(|| Arc::new(|| dshx_core::now_ms() as i64));
        Ok(Arc::new_cyclic(|me| ScheduleService {
            store,
            retention,
            clock,
            tasks: Mutex::new(tasks),
            ops: tokio::sync::Mutex::new(()),
            changed: broadcast::channel(64).0,
            resolver: Mutex::new(None),
            wake: Notify::new(),
            stopping: AtomicBool::new(false),
            runtime: Mutex::new(None),
            me: me.clone(),
        }))
    }

    pub fn retention(&self) -> Retention {
        self.retention
    }

    /// The service clock (epoch ms).
    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// `schedule/changed`: after every durable task write.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    fn emit_changed(&self) {
        let _ = self.changed.send(());
    }

    fn persist(&self, tasks: &[Task]) -> anyhow::Result<()> {
        self.store.save(tasks)
    }

    /// Sets how due occurrences reach their sessions (without a timer;
    /// [`ScheduleService::drive_now`] delivers).
    pub fn set_resolver(&self, resolver: Arc<dyn SessionResolver>) {
        *self.resolver.lock().unwrap() = Some(resolver);
    }

    /// Starts the delivery timer; due occurrences go through `resolver`.
    pub fn start(&self, resolver: Arc<dyn SessionResolver>) {
        self.set_resolver(resolver);
        let Some(me) = self.me.upgrade() else { return };
        let task = tokio::spawn(async move { me.run().await });
        if let Some(old) = self.runtime.lock().unwrap().replace(task) {
            old.abort();
        }
    }

    /// Stops the timer after an accepted delivery drained.
    pub async fn dispose(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.wake.notify_one();
        let task = self.runtime.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// Recomputes the nearest obligation (dsh `requestDrive`).
    pub fn request_drive(&self) {
        self.wake.notify_one();
    }

    async fn run(self: Arc<Self>) {
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            let failed = self.drive_now().await;
            let next = {
                let tasks = self.tasks.lock().unwrap();
                tasks
                    .iter()
                    .filter(|t| t.status == Status::Active && !failed.contains(&t.record.id))
                    .map(|t| t.record.scheduled_ms())
                    .min()
            };
            let notified = self.wake.notified();
            match next {
                Some(at) => {
                    let delay = (at - self.now()).clamp(0, MAX_TIMER_DELAY_MS) as u64;
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    /// One delivery pass over every due task (dsh `ScheduleRuntime.drive`);
    /// returns the ids that failed and wait for the next change.
    pub async fn drive_now(&self) -> HashSet<String> {
        let _ops = self.ops.lock().await;
        let mut failed = HashSet::new();
        let Some(resolver) = self.resolver.lock().unwrap().clone() else {
            return failed;
        };
        let scan = self.now();
        let due: Vec<Task> = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.status == Status::Active && t.record.scheduled_ms() <= scan)
            .cloned()
            .collect();
        let mut handled = HashSet::new();
        for task in &due {
            if self.stopping.load(Ordering::SeqCst) {
                return failed;
            }
            if handled.contains(&task.record.id) {
                continue;
            }
            let group: Vec<Task> = if task.record.is_recurring() {
                due.iter()
                    .filter(|c| c.session_id == task.session_id && c.record.is_recurring())
                    .cloned()
                    .collect()
            } else {
                vec![task.clone()]
            };
            for m in &group {
                handled.insert(m.record.id.clone());
            }
            let mut committed = HashSet::new();
            let mut admitted = group.clone();
            let result: anyhow::Result<()> = async {
                let agent = resolver.resolve(&task.session_id).await?;
                let now = self.now();
                admitted.retain(|m| m.record.scheduled_ms() <= now);
                if admitted.is_empty() {
                    return Ok(());
                }
                let occurrences: Vec<(Task, record::Occurrence)> = admitted
                    .iter()
                    .filter(|m| m.record.is_recurring())
                    .map(|m| resolve_occurrence(&m.record, now).map(|o| (m.clone(), o)))
                    .collect::<Result<_, _>>()
                    .map_err(anyhow::Error::msg)?;
                let text = if task.record.is_recurring() {
                    let pairs: Vec<(&Record, &str)> = occurrences
                        .iter()
                        .map(|(m, o)| (&m.record, o.occurrence_at.as_str()))
                        .collect();
                    batch_text(&pairs)
                } else {
                    reminder_text(&task.record)
                };
                let message = user_message(text);
                let message_id = message.id.clone();
                agent.followup(message).await?;
                agent.flush().await.map_err(|e| {
                    anyhow::anyhow!("Session persistence did not acknowledge the reminder: {e:#}")
                })?;
                let delivered_at = iso(self.now());
                if !task.record.is_recurring() {
                    let (last, history) = append_delivery(
                        task,
                        Receipt {
                            scheduled_at: task.record.scheduled_at.clone(),
                            delivered_at: delivered_at.clone(),
                            message_id: message_id.clone(),
                        },
                        self.retention,
                    );
                    let mut next = task.clone();
                    next.status = Status::Inactive;
                    next.last_delivery = Some(last);
                    next.history = Some(history);
                    self.commit(next)?;
                    committed.insert(task.record.id.clone());
                }
                for (member, o) in &occurrences {
                    let (last, history) = append_delivery(
                        member,
                        Receipt {
                            scheduled_at: o.occurrence_at.clone(),
                            delivered_at: delivered_at.clone(),
                            message_id: message_id.clone(),
                        },
                        self.retention,
                    );
                    let mut next = member.clone();
                    next.record.scheduled_at = o
                        .next_scheduled_at
                        .clone()
                        .unwrap_or_else(|| o.occurrence_at.clone());
                    next.status = if o.next_scheduled_at.is_some() {
                        Status::Active
                    } else {
                        Status::Inactive
                    };
                    next.last_delivery = Some(last);
                    next.history = Some(history);
                    self.commit(next)?;
                    committed.insert(member.record.id.clone());
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                let failed_at = self.now();
                let pending: Vec<&Task> = admitted
                    .iter()
                    .filter(|m| !committed.contains(&m.record.id))
                    .collect();
                for m in &pending {
                    if m.record.scheduled_ms() <= failed_at {
                        failed.insert(m.record.id.clone());
                    }
                }
                let ids: Vec<&str> = pending.iter().map(|m| m.record.id.as_str()).collect();
                tracing::warn!(
                    "schedule: reminders {} were not acknowledged: {e:#}",
                    serde_json::to_string(&ids).unwrap_or_default()
                );
            }
        }
        failed
    }

    /// Persists one task change after its delivery (status, target, receipt).
    fn commit(&self, task: Task) -> anyhow::Result<()> {
        {
            let mut tasks = self.tasks.lock().unwrap();
            let mut next = tasks.clone();
            match next.iter_mut().find(|t| t.record.id == task.record.id) {
                Some(slot) => *slot = task,
                None => next.push(task),
            }
            self.persist(&next)?;
            *tasks = next;
        }
        self.emit_changed();
        Ok(())
    }

    fn write(&self, f: impl FnOnce(&mut Vec<Task>)) -> anyhow::Result<()> {
        {
            let mut tasks = self.tasks.lock().unwrap();
            let mut next = tasks.clone();
            f(&mut next);
            self.persist(&next)?;
            *tasks = next;
        }
        self.emit_changed();
        self.request_drive();
        Ok(())
    }

    /// Creates a reminder bound to `session_id` (dsh `create`); the record
    /// is built from the clock reading taken before the write queue.
    pub async fn create(&self, session_id: &str, args: &Value) -> Result<Record, InputError> {
        let present = |k: &str| args.get(k).is_some_and(|v| !v.is_null());
        let count = [
            "at",
            "after_seconds",
            "every_seconds",
            "daily",
            "weekly",
            "cron",
        ]
        .iter()
        .filter(|k| present(k))
        .count();
        let selector_error = || {
            InputError::new(
                "invalid_selector",
                "Exactly one reminder selector is required.",
            )
        };
        if count > 1 {
            return Err(selector_error());
        }
        let title = schedule_title(args["title"].as_str().unwrap_or_default())?;
        let id = format!("schedule-{}", uuid::Uuid::new_v4());
        let now = self.now();
        let prompt = args["prompt"].as_str().unwrap_or_default();
        let selector = if present("at") {
            Selector::At(&args["at"])
        } else if present("after_seconds") {
            Selector::After(&args["after_seconds"])
        } else if present("every_seconds") {
            Selector::Every(&args["every_seconds"])
        } else if present("daily") {
            Selector::Daily(&args["daily"])
        } else if present("weekly") {
            Selector::Weekly(&args["weekly"])
        } else if present("cron") {
            Selector::Cron(&args["cron"])
        } else {
            return Err(selector_error());
        };
        let record = create_record(&id, prompt, selector, now, &title)?;
        let _ops = self.ops.lock().await;
        let task = Task {
            session_id: session_id.to_owned(),
            record: record.clone(),
            status: Status::Active,
            last_delivery: None,
            history: Some(History::default()),
        };
        self.write(|tasks| tasks.push(task))
            .map_err(|_| InputError::new("internal_error", "The schedule operation failed."))?;
        Ok(record)
    }

    /// Active reminders of one session, in storage order.
    pub fn list(&self, session_id: &str) -> Vec<Record> {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.session_id == session_id && t.status == Status::Active)
            .map(|t| t.record.clone())
            .collect()
    }

    /// Views of [`ScheduleService::list`] at the current time.
    pub fn list_views(&self, session_id: &str) -> Vec<Value> {
        let now = self.now();
        self.list(session_id).iter().map(|r| r.view(now)).collect()
    }

    /// Every active and inactive task with its session binding, ordered by
    /// `scheduledAt`, then id (dsh `catalog`).
    pub fn catalog(&self) -> Vec<Value> {
        let mut tasks = self.tasks.lock().unwrap().clone();
        tasks.sort_by(|a, b| {
            a.record
                .scheduled_at
                .cmp(&b.record.scheduled_at)
                .then_with(|| a.record.id.cmp(&b.record.id))
        });
        tasks.iter().map(catalog_row).collect()
    }

    /// The session a task belongs to.
    pub fn session_of(&self, id: &str) -> Option<String> {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.record.id == id)
            .map(|t| t.session_id.clone())
    }

    /// One newest-first page of saved deliveries (dsh `history`).
    pub fn history(
        &self,
        session_id: &str,
        id: &str,
        limit: i64,
        before: Option<&str>,
    ) -> Result<Value, InputError> {
        if !(1..=100).contains(&limit) {
            return Err(InputError::new(
                "invalid_rule",
                "Delivery history limit must be a safe integer from 1 through 100.",
            ));
        }
        let tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks
            .iter()
            .find(|t| t.record.id == id && t.session_id == session_id)
        else {
            return Ok(json!({"id": id, "code": "schedule_not_found"}));
        };
        let history = task.history_or_legacy();
        let end = match before {
            None => history.records.len(),
            Some(b) => match history.records.iter().position(|(r, _)| r.message_id == b) {
                Some(i) => i,
                None => return Ok(json!({"id": id, "code": "delivery_cursor_not_found"})),
            },
        };
        let start = end.saturating_sub(limit as usize);
        let records: Vec<Value> = history.records[start..end]
            .iter()
            .rev()
            .map(|(r, prompt)| {
                let mut m = Map::new();
                m.insert("scheduledAt".into(), r.scheduled_at.clone().into());
                m.insert("deliveredAt".into(), r.delivered_at.clone().into());
                m.insert("messageId".into(), r.message_id.clone().into());
                if let Some(p) = prompt {
                    m.insert("prompt".into(), p.clone().into());
                }
                Value::Object(m)
            })
            .collect();
        let mut out = Map::new();
        out.insert("id".into(), id.into());
        let oldest = records.last().map(|r| r["messageId"].clone());
        out.insert("records".into(), Value::Array(records));
        out.insert(
            "earlierRecordsUnavailable".into(),
            history.earlier_unavailable.into(),
        );
        out.insert(
            "earlierRecordsPruned".into(),
            (history.earlier_pruned == Some(true)).into(),
        );
        out.insert(
            "retention".into(),
            json!({"days": self.retention.days, "records": self.retention.records}),
        );
        if let Some(oldest) = oldest.filter(|_| start > 0) {
            out.insert("nextBefore".into(), oldest);
        }
        Ok(Value::Object(out))
    }

    /// Deletes one task of `session_id`, leaving queued messages intact.
    pub async fn delete(&self, session_id: &str, id: &str) -> Result<Value, InputError> {
        let _ops = self.ops.lock().await;
        let owned = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.record.id == id && t.session_id == session_id);
        if !owned {
            return Ok(json!({"id": id, "deleted": false, "code": "schedule_not_found"}));
        }
        self.write(|tasks| tasks.retain(|t| t.record.id != id))
            .map_err(|_| InputError::new("internal_error", "The schedule operation failed."))?;
        Ok(json!({"id": id, "deleted": true}))
    }

    /// Removes every active task of a session (dsh `stopSessionTasks`, on archive/delete).
    pub async fn stop_session_tasks(&self, session_id: &str) -> anyhow::Result<()> {
        let _ops = self.ops.lock().await;
        let any = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.session_id == session_id && t.status == Status::Active);
        if !any {
            return Ok(());
        }
        self.write(|tasks| {
            tasks.retain(|t| !(t.session_id == session_id && t.status == Status::Active))
        })
    }

    /// The tool-shaped update (dsh `schedule_update` after argument
    /// validation): `args` carries `id`, `title?`, `prompt?` and at most one
    /// timing selector; the current record is the expected one.
    pub async fn update_from_args(
        &self,
        session_id: &str,
        args: &Value,
    ) -> Result<Value, InputError> {
        let id = args["id"].as_str().unwrap_or_default();
        let expected = self.list(session_id).into_iter().find(|r| r.id == id);
        let Some(expected) = expected else {
            let ended = self
                .tasks
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.session_id == session_id && t.record.id == id);
            let code = if ended {
                "schedule_ended"
            } else {
                "schedule_not_found"
            };
            return Ok(json!({"id": id, "updated": false, "code": code}));
        };
        let change = ["at", "every_seconds", "daily", "weekly", "cron"]
            .iter()
            .find(|k| args.get(**k).is_some_and(|v| !v.is_null()))
            .map(|k| (*k, &args[*k]));
        let result = self
            .update(
                session_id,
                id,
                &expected.to_json(),
                change,
                args.get("title").and_then(Value::as_str),
                args.get("prompt").and_then(Value::as_str),
            )
            .await?;
        Ok(match result.get("record") {
            Some(r) => Record::from_json(r)
                .map(|r| r.view(self.now()))
                .unwrap_or(result),
            None => result,
        })
    }

    /// dsh `update`: compare the complete expected record, then apply a new
    /// title, prompt and/or timing; an equivalent timing keeps the target.
    pub async fn update(
        &self,
        session_id: &str,
        id: &str,
        expected: &Value,
        change: Option<(&str, &Value)>,
        title: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<Value, InputError> {
        let _ops = self.ops.lock().await;
        let current = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.record.id == id && t.session_id == session_id)
            .cloned();
        let Some(current) = current else {
            return Ok(json!({"id": id, "updated": false, "code": "schedule_not_found"}));
        };
        if current.status == Status::Inactive {
            return Ok(json!({"id": id, "updated": false, "code": "schedule_ended"}));
        }
        let Ok(decoded) = Record::from_json(expected) else {
            return Err(InputError::new(
                "invalid_rule",
                "expected must be a complete valid Schedule record.",
            ));
        };
        if decoded != current.record {
            return Ok(json!({"id": id, "updated": false, "code": "schedule_conflict"}));
        }
        let title = match title {
            Some(t) => schedule_title(t)?,
            None => current.record.title.clone(),
        };
        let prompt = match prompt {
            Some(p) if !p.trim().is_empty() => p.trim().to_owned(),
            Some(_) => {
                return Err(InputError::new(
                    "invalid_prompt",
                    "prompt must be non-empty after trimming.",
                ));
            }
            None => current.record.prompt.clone(),
        };
        let record = changed_record(&current.record, title, prompt, change, self.now())?;
        let updated = record != current.record;
        if updated {
            let next = record.clone();
            self.write(|tasks| {
                if let Some(t) = tasks.iter_mut().find(|t| t.record.id == id) {
                    t.record = next;
                }
            })
            .map_err(|_| InputError::new("internal_error", "The schedule operation failed."))?;
        }
        Ok(json!({"id": id, "updated": updated, "record": record.to_json()}))
    }
}

/// dsh `changedRecord`: an equivalent normalized timing keeps the committed
/// target; any other timing is recomputed exactly like creation.
fn changed_record(
    current: &Record,
    title: String,
    prompt: String,
    change: Option<(&str, &Value)>,
    now: i64,
) -> Result<Record, InputError> {
    let keep = || Record {
        title: title.clone(),
        prompt: prompt.clone(),
        ..current.clone()
    };
    let Some((kind, value)) = change else {
        return Ok(keep());
    };
    use record::Rule;
    let same = match (kind, &current.rule) {
        ("at", Rule::After { .. } | Rule::At) => parse_at(value)? == current.scheduled_ms(),
        ("every_seconds", Rule::Every { every_seconds }) => {
            crate::time::safe(value) == Some(*every_seconds)
        }
        ("daily", Rule::Daily { time, time_zone }) => {
            let (t, z) = parse_daily(value)?;
            t == *time && z.name == *time_zone
        }
        (
            "weekly",
            Rule::Weekly {
                time,
                time_zone,
                weekdays,
            },
        ) => {
            let (t, z, w) = parse_weekly(value)?;
            t == *time && z.name == *time_zone && w == *weekdays
        }
        (
            "cron",
            Rule::Cron {
                expression,
                time_zone,
            },
        ) => {
            let (e, z) = parse_cron_input(value)?;
            e == *expression && z.name == *time_zone
        }
        _ => false,
    };
    if same {
        return Ok(keep());
    }
    let selector = match kind {
        "at" => Selector::At(value),
        "every_seconds" => Selector::Every(value),
        "daily" => Selector::Daily(value),
        "weekly" => Selector::Weekly(value),
        _ => Selector::Cron(value),
    };
    create_record(&current.id, &prompt, selector, now, &title)
}
