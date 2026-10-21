//! Reminder delivery end to end: the real DeepSeek client against the
//! scripted mock, a file-backed task store, an injectable clock, and the
//! agent a resolver hands out for the reminder's session.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dshx_agent::testing::{FixedApprover, MemoryAttachments};
use dshx_agent::{Agent, AgentConfig, AgentDeps, AgentStatus};
use dshx_core::{
    ApprovalDecision, ContentBlock, LlmClient, SessionEvent, SessionLog, ToolRegistry,
};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_schedule::{
    Clock, Record, Retention, ScheduleService, SessionResolver, Status, TimeContextHook,
    batch_text, reminder_text, resolve_occurrence, schedule_tools,
};
use dshx_session::MemoryLog;
use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
use serde_json::{Value, json};

/// 2026-10-21T21:12:02Z, a Wednesday.
const NOW: i64 = 1_792_617_122_000;

fn ms(iso: &str) -> i64 {
    // Through the record decoder's canonical profile.
    let r = Record::from_json(
        &json!({"id": "x", "kind": "at", "title": "t", "prompt": "p", "scheduledAt": iso}),
    )
    .unwrap();
    r.scheduled_ms()
}

struct Fixture {
    _mock: MockDeepSeek,
    log: Arc<MemoryLog>,
    agent: Agent,
    clock: Arc<AtomicI64>,
    dir: tempfile::TempDir,
}

async fn llm(
    responder: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
) -> (MockDeepSeek, Arc<dyn LlmClient>) {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(responder);
    let mut config = DeepSeekConfig {
        base_url: mock.base_url(),
        ..DeepSeekConfig::default()
    };
    config.send_session_id = false;
    let llm: Arc<dyn LlmClient> = Arc::new(
        DeepSeekClient::builder(config)
            .api_key(Arc::new(StaticApiKey("sk-mock".into())))
            .build()
            .unwrap(),
    );
    (mock, llm)
}

fn agent(
    llm: Arc<dyn LlmClient>,
    log: Arc<MemoryLog>,
    tools: ToolRegistry,
    dir: &std::path::Path,
) -> Agent {
    let mut c = AgentConfig::new("deepseek-flash", dir);
    c.instructions.harness_home = dir.join("home/.dsh-codex");
    c.instructions.agents_home = dir.join("home/.agents");
    Agent::new(
        c,
        AgentDeps {
            llm,
            tools,
            log: log as Arc<dyn SessionLog>,
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: None,
            tool_state: None,
        },
    )
}

async fn fixture() -> Fixture {
    let (mock, llm) = llm(|_| Reply::text("noted")).await;
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(MemoryLog::new(dir.path().to_str().unwrap()));
    let agent = agent(llm, log.clone(), ToolRegistry::new(), dir.path());
    Fixture {
        _mock: mock,
        log,
        agent,
        clock: Arc::new(AtomicI64::new(NOW)),
        dir,
    }
}

struct Resolver {
    agent: Agent,
    fail: AtomicBool,
}

#[async_trait]
impl SessionResolver for Resolver {
    async fn resolve(&self, session_id: &str) -> anyhow::Result<Agent> {
        anyhow::ensure!(
            !self.fail.load(Ordering::SeqCst),
            "session {session_id} is unavailable"
        );
        anyhow::ensure!(
            session_id == self.agent.session_id(),
            "unknown session {session_id}"
        );
        Ok(self.agent.clone())
    }
}

impl Fixture {
    fn clock(&self) -> Clock {
        let c = self.clock.clone();
        Arc::new(move || c.load(Ordering::SeqCst))
    }

    fn open(&self) -> Arc<ScheduleService> {
        ScheduleService::open(
            &self.dir.path().join("storages"),
            Retention::default(),
            Some(self.clock()),
        )
        .unwrap()
    }

    fn set(&self, iso: &str) {
        self.clock.store(ms(iso), Ordering::SeqCst);
    }

    fn reminders(&self) -> Vec<SessionEvent> {
        self.log
            .events()
            .into_iter()
            .filter(|e| e.kind == "agent/inbox/spliced")
            .flat_map(|e| {
                e.data["inserted"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |m| (e.clone(), m))
            })
            .filter(|(_, m)| m["source"]["kind"] == "schedule")
            .map(|(e, m)| SessionEvent { data: m, ..e })
            .collect()
    }

    async fn settle(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self.agent.status() == AgentStatus::Idle && self.agent.is_idle() {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "agent never idled");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn task(service: &ScheduleService, id: &str) -> Value {
    service
        .catalog()
        .into_iter()
        .find(|t| t["id"] == id)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reminders_are_delivered_once_with_the_latest_missed_occurrence() {
    let f = fixture().await;
    let session = f.agent.session_id();
    let service = f.open();
    let resolver = Arc::new(Resolver {
        agent: f.agent.clone(),
        fail: AtomicBool::new(false),
    });
    service.set_resolver(resolver.clone());
    let create = |args: Value| {
        let service = service.clone();
        let session = session.clone();
        async move { service.create(&session, &args).await.unwrap() }
    };
    let after =
        create(json!({"prompt": "Check the deploy", "title": "Deploy", "after_seconds": 90})).await;
    let every = create(json!({"prompt": "Hourly", "title": "Hourly", "every_seconds": 3600})).await;
    let daily = create(json!({"prompt": "Standup", "title": "Standup", "daily": {"time": "09:00:00", "time_zone": "UTC"}})).await;
    let weekly = create(json!({"prompt": "Report", "title": "Report", "weekly": {"time": "09:00:00", "time_zone": "UTC", "weekdays": [5, 1]}})).await;
    let cron = create(json!({"prompt": "Poll", "title": "Poll", "cron": {"expression": "*/15 9-17 * * 1-5", "time_zone": "UTC"}})).await;
    assert_eq!(after.scheduled_at, "2026-10-21T21:13:32.000Z");
    assert_eq!(cron.scheduled_at, "2026-10-22T09:00:00.000Z");

    // Nothing is due yet.
    assert!(service.drive_now().await.is_empty());
    assert!(f.reminders().is_empty());

    // The one-shot reminder.
    f.set("2026-10-21T21:13:40.000Z");
    service.drive_now().await;
    f.settle().await;
    let delivered = f.reminders();
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0].data["content"][0]["text"],
        reminder_text(&after)
    );
    assert_eq!(delivered[0].data["source"], json!({"kind": "schedule"}));
    let row = task(&service, &after.id);
    assert_eq!(row["status"], "inactive");
    assert_eq!(row["lastDelivery"]["scheduledAt"], after.scheduled_at);
    assert_eq!(row["lastDelivery"]["messageId"], delivered[0].data["id"]);
    // The reminder entered the model's history as a user message.
    assert!(
        f.log
            .events()
            .iter()
            .any(|e| e.kind == "user/message" && e.data["source"]["kind"] == "schedule")
    );

    // Down from Wednesday night to Saturday 10:00: each recurring task
    // delivers only its latest missed occurrence, all in one batch.
    let at = ms("2026-10-24T10:00:00.000Z");
    f.set("2026-10-24T10:00:00.000Z");
    let expected: Vec<(Record, String)> = [&every, &daily, &weekly, &cron]
        .iter()
        .map(|r| {
            (
                (*r).clone(),
                resolve_occurrence(r, at).unwrap().occurrence_at,
            )
        })
        .collect();
    assert_eq!(
        expected.iter().map(|(_, o)| o.as_str()).collect::<Vec<_>>(),
        [
            "2026-10-24T09:12:02.000Z",
            "2026-10-24T09:00:00.000Z",
            "2026-10-23T09:00:00.000Z",
            "2026-10-23T17:45:00.000Z"
        ]
    );
    service.drive_now().await;
    f.settle().await;
    let delivered = f.reminders();
    assert_eq!(delivered.len(), 2);
    let pairs: Vec<(&Record, &str)> = expected.iter().map(|(r, o)| (r, o.as_str())).collect();
    assert_eq!(delivered[1].data["content"][0]["text"], batch_text(&pairs));
    assert_eq!(
        task(&service, &every.id)["scheduledAt"],
        "2026-10-24T10:12:02.000Z"
    );
    assert_eq!(
        task(&service, &daily.id)["scheduledAt"],
        "2026-10-25T09:00:00.000Z"
    );
    assert_eq!(
        task(&service, &weekly.id)["scheduledAt"],
        "2026-10-26T09:00:00.000Z"
    );
    assert_eq!(
        task(&service, &cron.id)["scheduledAt"],
        "2026-10-26T09:00:00.000Z"
    );
    // A second pass at the same instant delivers nothing.
    service.drive_now().await;
    assert_eq!(f.reminders().len(), 2);

    // A failed delivery keeps the tasks due and active.
    f.set("2026-10-26T12:00:00.000Z");
    resolver.fail.store(true, Ordering::SeqCst);
    let failed = service.drive_now().await;
    assert_eq!(failed.len(), 4);
    assert_eq!(
        task(&service, &daily.id)["scheduledAt"],
        "2026-10-25T09:00:00.000Z"
    );
    assert_eq!(f.reminders().len(), 2);

    // Restart: a new service reads storages/schedule.json and catches up.
    drop(service);
    let text = std::fs::read_to_string(f.dir.path().join("storages/schedule.json")).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["unit"], json!({"name": "schedule", "version": 1}));
    assert_eq!(doc["global"], Value::Null);
    let stored = &doc["tables"]["tasks"][&daily.id];
    let keys: Vec<&str> = stored
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "sessionId",
            "record",
            "status",
            "deliveryHistory",
            "lastDelivery"
        ]
    );
    assert!(text.ends_with("}\n") && text.contains("\n  \"unit\": {"));
    let service = f.open();
    resolver.fail.store(false, Ordering::SeqCst);
    service.set_resolver(resolver.clone());
    service.drive_now().await;
    f.settle().await;
    let delivered = f.reminders();
    assert_eq!(delivered.len(), 3);
    let batch = delivered[2].data["content"][0]["text"].as_str().unwrap();
    let json_part = batch.strip_prefix("[SCHEDULE REMINDER BATCH]\nThis is a scheduled message from the user\nreminders_json: ").unwrap();
    let reminders: Value = serde_json::from_str(json_part).unwrap();
    let occurrences: Vec<&str> = reminders
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["occurrence_at"].as_str().unwrap())
        .collect();
    assert_eq!(
        occurrences,
        [
            "2026-10-26T11:12:02.000Z",
            "2026-10-26T09:00:00.000Z",
            "2026-10-26T09:00:00.000Z",
            "2026-10-26T12:00:00.000Z"
        ]
    );
    // Delivery history survives the restart, newest first.
    let history = service.history(&session, &daily.id, 10, None).unwrap();
    let records = history["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["scheduledAt"], "2026-10-26T09:00:00.000Z");
    assert_eq!(records[1]["scheduledAt"], "2026-10-24T09:00:00.000Z");
    assert_eq!(records[0]["prompt"], "Standup");
    assert_eq!(history["retention"], json!({"days": 30, "records": 200}));
    let page = service.history(&session, &daily.id, 1, None).unwrap();
    assert_eq!(page["nextBefore"], records[0]["messageId"]);
    assert!(service.history(&session, &daily.id, 0, None).is_err());
    // Deleting a session's reminders.
    service.stop_session_tasks(&session).await.unwrap();
    assert!(service.list(&session).is_empty());
    assert_eq!(
        service.catalog().len(),
        1,
        "the inactive one-shot row stays"
    );
    let _ = Status::Active;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_schedule_tools_answer_with_dsh_values() {
    let script = |r: &RecordedRequest| -> Reply {
        if r.system().is_some_and(|s| s.contains("concise title")) {
            return Reply::text("Titel");
        }
        let n = r.tool_results();
        let id = format!("toolu_{n:02}");
        let results: Vec<Value> = r
            .messages()
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|b| b["type"] == "tool_result")
            .map(|b| {
                let t = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect(),
                    _ => String::new(),
                };
                serde_json::from_str(&t).unwrap_or(Value::Null)
            })
            .collect();
        match n {
            0 => Reply::tool_call(
                id,
                "schedule_create",
                json!({"prompt": "Ship", "title": "Ship it", "daily": {"time": "23:00:00", "time_zone": "Europe/Berlin"}}),
            ),
            1 => Reply::tool_call(
                id,
                "schedule_create",
                json!({"prompt": "x", "title": "y", "after_seconds": 5, "every_seconds": 60}),
            ),
            2 => Reply::tool_call(id, "schedule_list", json!({})),
            3 => Reply::tool_call(
                id,
                "schedule_update",
                json!({"id": results[0]["id"], "cron": {"expression": "0 9 * * 1-5", "time_zone": "UTC"}}),
            ),
            4 => Reply::tool_call(
                id,
                "schedule_update",
                json!({"id": results[0]["id"], "title": "Ship it"}),
            ),
            5 => Reply::tool_call(id, "schedule_delete", json!({"id": results[0]["id"]})),
            6 => Reply::tool_call(id, "schedule_delete", json!({"id": results[0]["id"]})),
            7 => Reply::tool_call(
                id,
                "schedule_update",
                json!({"id": results[0]["id"], "prompt": "again"}),
            ),
            _ => Reply::text("done"),
        }
    };
    let (mock, llm) = llm(script).await;
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicI64::new(NOW));
    let c = clock.clone();
    let service = ScheduleService::open(
        &dir.path().join("storages"),
        Retention::default(),
        Some(Arc::new(move || c.load(Ordering::SeqCst))),
    )
    .unwrap();
    let mut tools = ToolRegistry::new();
    for t in schedule_tools(&service) {
        tools.register(t);
    }
    let log = Arc::new(MemoryLog::new(dir.path().to_str().unwrap()));
    let agent = agent(llm, log.clone(), tools, dir.path());
    agent
        .run_turn(vec![ContentBlock::text("remind me")])
        .await
        .unwrap();
    let results: Vec<(Value, bool)> = log
        .events()
        .iter()
        .filter(|e| e.kind == "tool/result")
        .map(|e| {
            let text = e.data["message"]["content"][0]["text"].as_str().unwrap();
            (
                serde_json::from_str(text).unwrap(),
                e.data["message"]["isError"] == true,
            )
        })
        .collect();
    assert_eq!(results.len(), 8);
    assert!(
        results.iter().all(|(_, err)| !err),
        "schedule results are values, not errors"
    );
    let created = &results[0].0;
    let keys: Vec<&str> = created
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "kind",
            "title",
            "prompt",
            "time",
            "timeZone",
            "scheduledAt",
            "state",
            "deliveryMode"
        ]
    );
    assert_eq!(created["time"], "23:00:00.000");
    assert_eq!(created["scheduledAt"], "2026-10-22T21:00:00.000Z");
    assert_eq!(created["state"], "scheduled");
    assert_eq!(created["deliveryMode"], "host");
    assert_eq!(
        results[1].0,
        json!({"code": "invalid_selector", "message": "schedule_create accepts exactly one of after_seconds, at, every_seconds, daily, weekly, or cron."})
    );
    assert_eq!(results[2].0.as_array().unwrap().len(), 1);
    assert_eq!(results[3].0["kind"], "cron");
    assert_eq!(results[3].0["scheduledAt"], "2026-10-22T09:00:00.000Z");
    assert_eq!(results[3].0["id"], created["id"]);
    assert_eq!(
        results[4].0["expression"], "0 9 * * 1-5",
        "an unchanged title keeps the record"
    );
    assert_eq!(results[5].0, json!({"id": created["id"], "deleted": true}));
    assert_eq!(
        results[6].0,
        json!({"id": created["id"], "deleted": false, "code": "schedule_not_found"})
    );
    assert_eq!(
        results[7].0,
        json!({"id": created["id"], "updated": false, "code": "schedule_not_found"})
    );
    drop(mock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn time_context_readings_follow_the_refresh_interval() {
    let (mock, llm) = llm(|_| Reply::text("ok")).await;
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(MemoryLog::new(dir.path().to_str().unwrap()));
    let a = agent(llm, log.clone(), ToolRegistry::new(), dir.path());
    let offset = Arc::new(AtomicI64::new(0));
    let o = offset.clone();
    let hook = TimeContextHook::new(
        Some("Europe/Berlin"),
        600_000,
        Some(Arc::new(move || {
            dshx_core::now_ms() as i64 + o.load(Ordering::SeqCst)
        })),
    )
    .unwrap();
    a.add_pre_step_hook(hook);
    let readings = |log: &MemoryLog| -> Vec<SessionEvent> {
        log.events()
            .into_iter()
            .filter(|e| e.kind == "user/message" && e.data["source"]["kind"] == "time-context")
            .collect()
    };
    a.run_turn(vec![ContentBlock::text("one")]).await.unwrap();
    let first = readings(&log);
    assert_eq!(first.len(), 1);
    let text = first[0].data["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Time sampled while preparing turn 1, step 1: "),
        "{text}"
    );
    assert!(text.contains("[Europe/Berlin]\nBrowser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.\nElapsed since the preceding model-visible message: unavailable."), "{text}");
    assert_eq!(first[0].data["source"]["form"], "snapshot");
    assert_eq!(
        first[0].data["source"]["sections"][0]["name"],
        "time-context"
    );
    // The reading is the last message of its step, after the runtime context.
    let users: Vec<SessionEvent> = log
        .events()
        .into_iter()
        .filter(|e| e.kind == "user/message")
        .collect();
    assert_eq!(users.last().unwrap().data["source"]["kind"], "time-context");
    assert_eq!(
        users[users.len() - 2].data["source"]["kind"],
        "runtime-context"
    );
    // Within the refresh interval: no new reading.
    a.run_turn(vec![ContentBlock::text("two")]).await.unwrap();
    assert_eq!(readings(&log).len(), 1);
    // Eleven minutes later: a new reading with the elapsed time.
    offset.store(11 * 60_000, Ordering::SeqCst);
    a.run_turn(vec![ContentBlock::text("three")]).await.unwrap();
    let all = readings(&log);
    assert_eq!(all.len(), 2);
    let text = all[1].data["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Time sampled while preparing turn 3, step 1: "),
        "{text}"
    );
    assert!(
        text.contains("Elapsed since the preceding model-visible message: 11m "),
        "{text}"
    );
    // The model saw it as the final user block.
    let request = mock
        .requests()
        .into_iter()
        .rfind(|r| !r.system().is_some_and(|s| s.contains("concise title")))
        .unwrap();
    let last_user = request
        .messages()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap();
    let blocks = last_user["content"].as_array().unwrap();
    assert_eq!(blocks.last().unwrap()["text"].as_str().unwrap(), text);
}
