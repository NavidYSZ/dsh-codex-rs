//! Goal rounds end to end: the real DeepSeek client against the scripted
//! mock, the goal tools in the agent's registry and the round driver on.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dshx_agent::testing::{FixedApprover, MemoryAttachments};
use dshx_agent::{Agent, AgentConfig, AgentDeps, AgentStatus, CancelCause, TurnEndCompaction};
use dshx_core::{
    ApprovalDecision, ContentBlock, LlmClient, NewEvent, SessionEvent, SessionHeader, SessionLog,
    ToolRegistry,
};
use dshx_goal::{Activation, GoalCommand, GoalService, GoalSettings, Phase, goal_tools};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_session::MemoryLog;
use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
use serde_json::{Value, json};

/// Every content block of the request with its role, in order.
fn blocks(r: &RecordedRequest) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for m in r.messages() {
        let role = m["role"].as_str().unwrap_or_default().to_owned();
        match &m["content"] {
            Value::String(s) => out.push((role.clone(), json!({"type": "text", "text": s}))),
            Value::Array(items) => out.extend(items.iter().map(|b| (role.clone(), b.clone()))),
            _ => {}
        }
    }
    out
}

fn text(b: &Value) -> &str {
    b["text"].as_str().unwrap_or_default()
}

fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect(),
        _ => String::new(),
    }
}

/// The scripted model: a human prompt creates a goal; round `complete_at`
/// reads the goal and completes it; other rounds just report progress; the
/// wrap-up instruction gets the closing message.
fn responder(
    complete_at: u64,
    max_rounds: u64,
) -> impl Fn(&RecordedRequest) -> Reply + Send + Sync {
    move |r| {
        if r.system().is_some_and(|s| s.contains("concise title")) {
            return Reply::text("Titel");
        }
        let all = blocks(r);
        // The newest instruction block (human prompt, round or wrap-up).
        let start = all
            .iter()
            .rposition(|(role, b)| {
                role == "user"
                    && b["type"] == "text"
                    && (text(b).starts_with("<goal_round>")
                        || text(b).starts_with("<goal_complete>")
                        || text(b).starts_with("GOAL "))
            })
            .unwrap_or(0);
        let task = text(&all[start].1).to_owned();
        let after: Vec<&(String, Value)> = all[start..].iter().collect();
        let results: Vec<String> = after
            .iter()
            .filter(|(_, b)| b["type"] == "tool_result")
            .map(|(_, b)| result_text(b))
            .collect();
        let n = r
            .messages()
            .iter()
            .filter(|m| m["role"] == "assistant")
            .count();
        let id = format!("toolu_{n:03}");
        if task.starts_with("<goal_complete>") {
            return Reply::text("Closing: the feature shipped.");
        }
        if task.starts_with("GOAL ") {
            if results.is_empty() {
                return Reply::tool_call(
                    id,
                    "create_goal",
                    json!({"objective": "Ship the feature", "max_goal_rounds": max_rounds}),
                );
            }
            return Reply::text("Goal set.");
        }
        let round: u64 = task
            .lines()
            .find_map(|l| l.strip_prefix("Round: "))
            .and_then(|r| r.split('/').next())
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);
        if task.contains("HANG") || round != complete_at {
            return Reply::text(format!("Progress in round {round}."));
        }
        match results.len() {
            0 => Reply::tool_call(id, "get_goal", json!({})),
            _ => {
                let goal: Value = serde_json::from_str(&results[0]).unwrap_or_default();
                Reply::tool_call(
                    id,
                    "update_goal",
                    json!({"goal_id": goal["goal"]["id"], "revision": goal["goal"]["revision"], "action": "complete"}),
                )
            }
        }
    }
}

struct Harness {
    mock: MockDeepSeek,
    log: Arc<dyn SessionLog>,
    agent: Agent,
    goals: Arc<GoalService>,
    _dir: tempfile::TempDir,
}

/// Holds the driver either in its terminal append or in the durability
/// checkpoint after compaction/end, when no summary token remains installed.
struct GatedEndLog {
    log: MemoryLog,
    terminal_append: bool,
    armed: AtomicBool,
    compaction_finished: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl GatedEndLog {
    fn new(cwd: &str, terminal_append: bool) -> Arc<Self> {
        Arc::new(Self {
            log: MemoryLog::new(cwd),
            terminal_append,
            armed: AtomicBool::new(true),
            compaction_finished: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }

    async fn hold_once(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl SessionLog for GatedEndLog {
    fn id(&self) -> &str {
        self.log.id()
    }
    fn header(&self) -> SessionHeader {
        self.log.header()
    }
    fn events(&self) -> Vec<SessionEvent> {
        self.log.events()
    }
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.log.subscribe()
    }
    async fn append(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        if self.terminal_append && events.iter().any(|e| e.kind == "turn/end") {
            self.hold_once().await;
        }
        let ends_compaction = events.iter().any(|e| e.kind == "compaction/end");
        let stored = self.log.append(events).await?;
        if ends_compaction {
            self.compaction_finished.store(true, Ordering::SeqCst);
        }
        Ok(stored)
    }
    async fn flush(&self) -> anyhow::Result<()> {
        if !self.terminal_append && self.compaction_finished.load(Ordering::SeqCst) {
            self.hold_once().await;
        }
        self.log.flush().await
    }
}

async fn harness_with(
    responder: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    log: Option<Arc<dyn SessionLog>>,
    goals: Option<Arc<GoalService>>,
) -> Harness {
    harness_configured(responder, log, goals, true, |_| {}).await
}

async fn harness_configured(
    responder: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    log: Option<Arc<dyn SessionLog>>,
    goals: Option<Arc<GoalService>>,
    drive: bool,
    configure: impl FnOnce(&mut AgentConfig),
) -> Harness {
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
    let dir = tempfile::tempdir().unwrap();
    let log = log.unwrap_or_else(|| Arc::new(MemoryLog::new(dir.path().to_str().unwrap())));
    let goals = goals.unwrap_or_else(|| GoalService::new(GoalSettings::default()));
    let mut tools = ToolRegistry::new();
    for t in goal_tools(&goals) {
        tools.register(t);
    }
    let mut agent_config = AgentConfig::new("deepseek-flash", dir.path());
    agent_config.instructions.harness_home = dir.path().join("home/.dsh-codex");
    agent_config.instructions.agents_home = dir.path().join("home/.agents");
    configure(&mut agent_config);
    let agent = Agent::new(
        agent_config,
        AgentDeps {
            llm,
            tools,
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: None,
            tool_state: None,
        },
    );
    goals.attach(&agent, drive);
    Harness {
        mock,
        log,
        agent,
        goals,
        _dir: dir,
    }
}

async fn harness(complete_at: u64, max_rounds: u64) -> Harness {
    harness_with(responder(complete_at, max_rounds), None, None).await
}

impl Harness {
    fn events(&self) -> Vec<SessionEvent> {
        self.log.events()
    }

    async fn until(&self, what: &str, f: impl Fn(&Harness) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !f(self) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}: {:#?}",
                kinds(&self.events())
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn idle(&self) -> bool {
        self.agent.status() == AgentStatus::Idle && self.agent.is_idle()
    }
}

fn kinds(events: &[SessionEvent]) -> Vec<String> {
    events.iter().map(|e| e.kind.clone()).collect()
}

fn goal_changes(events: &[SessionEvent]) -> Vec<Value> {
    events
        .iter()
        .filter(|e| e.kind == "goal/change")
        .map(|e| e.data.clone())
        .collect()
}

fn rounds(events: &[SessionEvent]) -> Vec<u64> {
    events
        .iter()
        .filter(|e| e.kind == "user/message" && e.data["source"]["kind"] == "goal")
        .map(|e| e.data["source"]["round"].as_u64().unwrap())
        .collect()
}

fn is_summary(r: &RecordedRequest) -> bool {
    blocks(r)
        .last()
        .is_some_and(|(_, b)| text(b).contains("compaction engine"))
}

fn round_config(config: &mut AgentConfig) {
    config.title.enabled = false;
    config.compaction.turn_end = TurnEndCompaction::EveryTurn;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_goal_rounds_compact_and_restore_canonical_goal_state() {
    let h = harness_configured(
        |r| {
            if is_summary(r) {
                Reply::text("Checkpoint: work has progressed; keep pursuing the objective.")
            } else {
                Reply::text("progress ".repeat(750))
            }
        },
        None,
        None,
        true,
        round_config,
    )
    .await;
    let objective = "Ship the feature, preserve the accepted corrections, and verify the result.";
    let initial = h
        .goals
        .command(
            &h.agent.session_id(),
            GoalCommand::Create(objective.into()),
            Some(&json!(3)),
        )
        .await
        .unwrap()
        .unwrap();
    h.until("round budget reached", |h| {
        h.goals
            .get(&h.agent.session_id())
            .unwrap()
            .is_some_and(|g| g.goal.phase == Phase::Blocked)
            && h.idle()
    })
    .await;
    let events = h.events();
    assert_eq!(rounds(&events), [1, 2, 3]);
    let summary = events
        .iter()
        .find(|e| e.kind == "compaction/summary")
        .expect("6KB work rounds must compact below the standard 8K-token gate");
    let start = events[..summary.seq as usize]
        .iter()
        .rev()
        .find(|e| e.kind == "compaction/start")
        .unwrap();
    assert!(
        start.data["turn"].is_null(),
        "single-step round compacts after turn/end"
    );
    let snapshot = events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "goal-state")
        .expect("canonical state restored");
    assert!(summary.seq < snapshot.seq);
    let state = &snapshot.data["source"]["snapshot"]["goal"];
    assert_eq!(state["id"], initial.goal.id);
    assert_eq!(state["revision"], initial.goal.revision);
    assert_eq!(state["objective"], objective);
    assert_eq!(state["phase"], "active");
    assert_eq!(state["activation"], "armed");
    assert_eq!(
        state["roundsStarted"], 3,
        "snapshot and round admission share one commit"
    );
    assert_eq!(state["maxGoalRounds"], 3);
    let goal = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
    assert_eq!(goal.goal.objective, objective);
    assert_eq!(goal.rounds_started, 3);
    assert_eq!(goal.goal.max_goal_rounds, 3);
    assert_eq!(goal.goal.blocked_reason.unwrap().code, "round-limit");
    dshx_session::validate::validate_log(&h.log.header(), &events).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_summary_disarms_goal_without_a_next_model_request() {
    for (inside_turn, parent) in [(true, false), (false, false), (true, true)] {
        let h = harness_configured(
            move |r| {
                if is_summary(r) {
                    return Reply::stream()
                        .text("unfinished checkpoint")
                        .stall_after(3)
                        .into();
                }
                if inside_turn {
                    Reply::tool_call("read-goal", "get_goal", json!({}))
                } else {
                    Reply::text("Progress made.")
                }
            },
            None,
            None,
            true,
            round_config,
        )
        .await;
        h.goals
            .command(
                &h.agent.session_id(),
                GoalCommand::Create("objective detail ".repeat(400)),
                Some(&json!(5)),
            )
            .await
            .unwrap();
        h.until("blocked summarizer", |h| {
            h.mock.requests().iter().any(is_summary)
        })
        .await;
        let before = h.mock.requests().len();
        if parent {
            h.agent.cancel_with(CancelCause::Parent, true).await;
        } else {
            // Exact public stop entry point used by WebBackend::cancel.
            h.agent.cancel_durable().await.unwrap();
        }
        h.until("stopped and disarmed", |h| {
            h.idle()
                && h.goals
                    .get(&h.agent.session_id())
                    .unwrap()
                    .is_some_and(|g| g.activation == Activation::Disarmed)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            h.mock.requests().len(),
            before,
            "stop must not start another request: inside_turn={inside_turn}, parent={parent}"
        );
        let events = h.events();
        assert_eq!(rounds(&events), [1]);
        assert!(!events.iter().any(|e| e.kind == "compaction/summary"));
        let start = events
            .iter()
            .find(|e| e.kind == "compaction/start")
            .unwrap();
        assert_eq!(
            start.data["turn"],
            if inside_turn { json!(1) } else { Value::Null }
        );
        assert!(
            events
                .iter()
                .any(|e| e.kind == "compaction/end" && e.data.get("error").is_some())
        );
        let goal = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
        assert_eq!(goal.rounds_started, 1);
        assert_eq!(goal.goal.max_goal_rounds, 5);
        assert_eq!(
            goal.goal.phase,
            if inside_turn {
                Phase::Paused
            } else {
                Phase::Active
            }
        );
        dshx_session::validate::validate_log(&h.log.header(), &events).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_during_terminal_commit_or_post_summary_flush_prevents_continuation() {
    for terminal_append in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let log = GatedEndLog::new(dir.path().to_str().unwrap(), terminal_append);
        let h = harness_configured(
            |r| {
                if is_summary(r) {
                    Reply::text("Checkpoint: continue the work.")
                } else {
                    Reply::text("Progress made.")
                }
            },
            Some(log.clone()),
            None,
            true,
            round_config,
        )
        .await;
        let objective = "objective detail ".repeat(400).trim().to_owned();
        h.goals
            .command(
                &h.agent.session_id(),
                GoalCommand::Create(objective.clone()),
                Some(&json!(5)),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), log.entered.notified())
            .await
            .unwrap();
        assert_eq!(h.agent.status(), AgentStatus::Running);
        let events = h.events();
        assert_eq!(rounds(&events), [1]);
        assert_eq!(
            events.iter().any(|e| e.kind == "compaction/end"),
            !terminal_append
        );
        assert_eq!(
            h.goals
                .get(&h.agent.session_id())
                .unwrap()
                .unwrap()
                .activation,
            Activation::Armed
        );
        let before = h.mock.requests().len();
        // Same stop method used by the public web endpoint. Its own flush
        // must complete even while the driver's one-shot flush is held.
        h.agent.cancel_durable().await.unwrap();
        assert_eq!(
            h.goals
                .get(&h.agent.session_id())
                .unwrap()
                .unwrap()
                .activation,
            Activation::Disarmed
        );
        log.release.notify_one();
        h.until("terminal stop released", |h| h.idle()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            h.mock.requests().len(),
            before,
            "no new summary or goal request after stop, terminal_append={terminal_append}"
        );
        assert_eq!(rounds(&h.events()), [1]);
        let goal = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
        assert_eq!(goal.goal.objective, objective);
        assert_eq!(goal.goal.phase, Phase::Active);
        assert_eq!(goal.rounds_started, 1);
        assert_eq!(goal.goal.max_goal_rounds, 5);
        assert_eq!(goal.activation, Activation::Disarmed);
        dshx_session::validate::validate_log(&h.log.header(), &h.events()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rounds_continue_until_the_model_completes_the_goal() {
    let h = harness(2, 5).await;
    h.agent
        .run_turn(vec![ContentBlock::text("GOAL ship the feature")])
        .await
        .unwrap();
    h.until("goal completion", |h| {
        goal_changes(&h.events())
            .iter()
            .any(|c| c["operation"] == "complete")
            && h.idle()
    })
    .await;
    h.until("the closing message", |h| {
        h.events().iter().any(|e| {
            e.kind == "assistant/message"
                && e.data["message"]["content"][0]["text"] == "Closing: the feature shipped."
        })
    })
    .await;
    let events = h.events();
    assert_eq!(rounds(&events), [1, 2]);
    let changes = goal_changes(&events);
    assert_eq!(changes[0]["operation"], "create");
    assert_eq!(changes[0]["goal"]["objective"], "Ship the feature");
    assert_eq!(changes[0]["goal"]["maxGoalRounds"], 5);
    assert_eq!(changes[0]["roundsStarted"], 0);
    let keys: Vec<&str> = changes[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "kind",
            "version",
            "operation",
            "goal",
            "roundsStarted",
            "createdAt",
            "updatedAt"
        ]
    );
    let complete = changes
        .iter()
        .find(|c| c["operation"] == "complete")
        .unwrap();
    assert_eq!(complete["goal"]["phase"], "complete");
    assert_eq!(complete["goal"]["revision"], 2);
    assert_eq!(complete["roundsStarted"], 2);
    // The round prompt is dsh's, attributed to the exact goal revision.
    let round1 = events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["round"] == 1)
        .unwrap();
    assert_eq!(round1.data["source"]["goalId"], changes[0]["goal"]["id"]);
    assert_eq!(round1.data["source"]["revision"], 1);
    assert_eq!(
        round1.data["content"][0]["text"],
        dshx_goal::round_prompt("Ship the feature", 1, 5)
    );
    // Every round step starts a request series.
    let round_seq = round1.seq;
    let series = events.iter().rev().find(|e| {
        e.kind == "request/header" && e.seq < round_seq + 5 && e.seq > round_seq.saturating_sub(5)
    });
    assert!(
        series.is_some_and(|e| e.data["reason"] == "series" || e.data["startsSeries"] == true),
        "{:?}",
        kinds(&events)
    );
    // complete inside the goal's own round defers dsh's wrap-up instruction.
    let wrapup = events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "tool-goal")
        .expect("wrap-up context");
    assert_eq!(wrapup.data["source"]["form"], "notice");
    assert_eq!(
        wrapup.data["source"]["summary"],
        "complete: Ship the feature"
    );
    assert_eq!(
        wrapup.data["content"][0]["text"],
        dshx_goal::wrapup_text("Ship the feature", None)
    );
    // The model saw the goal guidance and the dsh tool result JSON.
    let first = h
        .mock
        .requests()
        .into_iter()
        .find(|r| !r.system().is_some_and(|s| s.contains("concise title")))
        .unwrap();
    assert!(first.system().unwrap().contains("Mark blocked only after the same blocking condition persists for at least 3 consecutive goal rounds"));
    let view = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
    assert_eq!(view.goal.phase, Phase::Complete);
    assert_eq!(view.activation, Activation::Disarmed);
    assert_eq!(view.rounds_started, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_round_limit_blocks_the_goal() {
    let h = harness(99, 2).await;
    h.agent
        .run_turn(vec![ContentBlock::text("GOAL ship the feature")])
        .await
        .unwrap();
    h.until("the round-limit block", |h| {
        goal_changes(&h.events())
            .iter()
            .any(|c| c["operation"] == "block")
            && h.idle()
    })
    .await;
    let events = h.events();
    assert_eq!(rounds(&events), [1, 2]);
    let block = goal_changes(&events)
        .into_iter()
        .find(|c| c["operation"] == "block")
        .unwrap();
    assert_eq!(
        block["goal"]["blockedReason"],
        json!({"code": "round-limit", "message": "Goal reached its configured limit of 2 rounds."})
    );
    assert_eq!(block["goal"]["phase"], "blocked");
    let view = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
    assert_eq!(view.activation, Activation::Disarmed);
    // Nothing else runs once blocked.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rounds(&h.events()), [1, 2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_abort_of_a_round_pauses_the_goal() {
    let base = responder(99, 5);
    let h = harness_with(
        move |r| {
            let last = r
                .messages()
                .last()
                .map(|m| m["content"].to_string())
                .unwrap_or_default();
            if last.contains("Round: 1/5")
                && !r.system().is_some_and(|s| s.contains("concise title"))
            {
                // Round 1 never finishes on its own.
                return Reply::stream()
                    .text("Working on round 1")
                    .stall_after(3)
                    .into();
            }
            base(r)
        },
        None,
        None,
    )
    .await;
    h.agent
        .run_turn(vec![ContentBlock::text("GOAL ship the feature")])
        .await
        .unwrap();
    h.until("round 1 running", |h| {
        rounds(&h.events()) == [1] && h.agent.status() == AgentStatus::Running
    })
    .await;
    h.agent.cancel().await;
    h.until("the pause", |h| {
        goal_changes(&h.events())
            .iter()
            .any(|c| c["operation"] == "pause")
            && h.idle()
    })
    .await;
    let events = h.events();
    let end = events.iter().rev().find(|e| e.kind == "turn/end").unwrap();
    assert_eq!(
        end.data["reason"],
        json!({"kind": "aborted", "reason": {"kind": "user"}})
    );
    let view = h.goals.get(&h.agent.session_id()).unwrap().unwrap();
    assert_eq!(view.goal.phase, Phase::Paused);
    assert_eq!(view.activation, Activation::Disarmed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rounds(&h.events()), [1], "a paused goal starts no round");
    // The user resumes it (dsh `/goal resume`): rounds continue.
    h.goals
        .command(&h.agent.session_id(), GoalCommand::Resume, None)
        .await
        .unwrap();
    h.until("round 2", |h| rounds(&h.events()).contains(&2))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_session_keeps_the_goal_disarmed_until_resumed() {
    let first = harness(99, 3).await;
    // Pause right after creation so no round runs in the first process.
    first
        .agent
        .run_turn(vec![ContentBlock::text("GOAL ship the feature")])
        .await
        .unwrap();
    let id = first.agent.session_id();
    first
        .goals
        .command(&id, GoalCommand::Pause, None)
        .await
        .unwrap();
    first.until("idle", |h| h.idle()).await;
    first.goals.detach(&id).await;
    first.agent.dispose().await;
    let log = first.log.clone();
    let before = rounds(&log.events()).len();
    // "Restart": a fresh service and agent over the same log.
    let second = harness_with(responder(99, 3), Some(log), None).await;
    let view = second.goals.get(&id).unwrap().unwrap();
    assert_eq!(view.goal.phase, Phase::Paused);
    assert_eq!(view.activation, Activation::Disarmed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rounds(&second.events()).len(), before);
    let resumed = second
        .goals
        .command(&id, GoalCommand::Resume, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.activation, Activation::Armed);
    second
        .until("the round limit", |h| {
            goal_changes(&h.events())
                .iter()
                .any(|c| c["operation"] == "block")
        })
        .await;
    assert_eq!(rounds(&second.events()), [1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_tools_enforce_dsh_authority() {
    // create_goal from inside a goal round (no direct human input) is refused,
    // blocked before the threshold is refused.
    let h = harness_with(
        |r| {
            if r.system().is_some_and(|s| s.contains("concise title")) {
                return Reply::text("Titel");
            }
            let all = blocks(r);
            let start = all
                .iter()
                .rposition(|(role, b)| role == "user" && b["type"] == "text" && (text(b).starts_with("<goal_round>") || text(b).starts_with("GOAL")))
                .unwrap_or(0);
            let in_round = text(&all[start].1).starts_with("<goal_round>");
            let results: Vec<String> = all[start..].iter().filter(|(_, b)| b["type"] == "tool_result").map(|(_, b)| result_text(b)).collect();
            let n = r.messages().iter().filter(|m| m["role"] == "assistant").count();
            let id = format!("toolu_{n:03}");
            if !in_round {
                return match results.len() {
                    0 => Reply::tool_call(id, "create_goal", json!({"objective": "  Fix CI  "})),
                    _ => Reply::text("ok"),
                };
            }
            match results.len() {
                0 => Reply::tool_call(id, "create_goal", json!({"objective": "second"})),
                1 => Reply::tool_call(id, "get_goal", json!({})),
                2 => {
                    let goal: Value = serde_json::from_str(&results[1]).unwrap();
                    Reply::tool_call(id, "update_goal", json!({"goal_id": goal["goal"]["id"], "revision": goal["goal"]["revision"], "action": "blocked", "blocked_reason": "CI is down"}))
                }
                3 => Reply::tool_call(id, "update_goal", json!({"goal_id": " x", "revision": 1, "action": "pause"})),
                _ => Reply::text("stop"),
            }
        },
        None,
        None,
    )
    .await;
    h.agent
        .run_turn(vec![ContentBlock::text("GOAL fix ci")])
        .await
        .unwrap();
    h.until("round 1 tool results", |h| {
        h.events()
            .iter()
            .filter(|e| e.kind == "tool/result")
            .count()
            >= 5
    })
    .await;
    let id = h.agent.session_id();
    let _ = h.goals.command(&id, GoalCommand::Pause, None).await;
    let results: Vec<SessionEvent> = h
        .events()
        .into_iter()
        .filter(|e| e.kind == "tool/result")
        .collect();
    let text = |e: &SessionEvent| {
        e.data["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let created: Value = serde_json::from_str(&text(&results[0])).unwrap();
    assert_eq!(created["goal"]["objective"], "Fix CI");
    assert_eq!(created["activation"], "armed");
    let keys: Vec<&str> = created["goal"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "revision",
            "objective",
            "phase",
            "roundsStarted",
            "maxGoalRounds"
        ]
    );
    assert_eq!(
        text(&results[1]),
        "Error: this goal operation requires a direct human turn on a top-level agent"
    );
    assert_eq!(
        results[1].data["error"],
        json!({"name": "HarnessError", "code": "GOAL_TOOL_AUTHORITY_REQUIRED"})
    );
    assert_eq!(
        text(&results[3]),
        "Error: blocked requires at least 3 consecutive goal rounds; current round is 1"
    );
    assert_eq!(
        results[3].data["error"]["code"],
        "GOAL_TOOL_BLOCK_THRESHOLD"
    );
    assert_eq!(
        text(&results[4]),
        "Error: goal_id must be non-empty and revision must be a positive safe integer"
    );
}
