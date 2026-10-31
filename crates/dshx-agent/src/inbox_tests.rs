//! Deterministic inbox interleavings: ownership, durable admission and recovery.
use super::*;
use crate::testing::{FixedApprover, MemoryAttachments, ScriptedLlm, text};
use async_trait::async_trait;
use dshx_core::{ApprovalDecision, SessionLog};
use dshx_session::MemoryLog;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct TestLog {
    log: MemoryLog,
    fail_promotion: AtomicBool,
    fail_commit: AtomicBool,
    fail_flush: AtomicBool,
    gate_flush: AtomicBool,
    flush_entered: tokio::sync::Notify,
    flush_release: tokio::sync::Notify,
    batches: Mutex<Vec<Vec<NewEvent>>>,
}

impl TestLog {
    fn new(path: &std::path::Path) -> Arc<Self> {
        Arc::new(Self {
            log: MemoryLog::new(path.to_str().unwrap()),
            fail_promotion: AtomicBool::new(false),
            fail_commit: AtomicBool::new(false),
            fail_flush: AtomicBool::new(false),
            gate_flush: AtomicBool::new(false),
            flush_entered: tokio::sync::Notify::new(),
            flush_release: tokio::sync::Notify::new(),
            batches: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl SessionLog for TestLog {
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
    async fn flush(&self) -> anyhow::Result<()> {
        if self.gate_flush.swap(false, Ordering::SeqCst) {
            self.flush_entered.notify_one();
            self.flush_release.notified().await;
        }
        anyhow::ensure!(
            !self.fail_flush.swap(false, Ordering::SeqCst),
            "injected flush failure"
        );
        self.log.flush().await
    }
    async fn append(&self, events: Vec<NewEvent>) -> anyhow::Result<Vec<SessionEvent>> {
        let promotion = events.len() == 2
            && events[0].kind == kinds::INBOX_SPLICED
            && events[1].kind == kinds::INBOX_SPLICED
            && events[0].data["target"] == "next-step"
            && events[1].data["target"] == "next-turn";
        if promotion && self.fail_promotion.swap(false, Ordering::SeqCst) {
            anyhow::bail!("injected promotion failure");
        }
        if events.iter().any(|e| e.kind == kinds::USER_MESSAGE)
            && self.fail_commit.swap(false, Ordering::SeqCst)
        {
            anyhow::bail!("injected user-message failure");
        }
        self.batches.lock().unwrap().push(events.clone());
        self.log.append(events).await
    }
}

fn agent(
    log: Arc<dyn SessionLog>,
    path: &std::path::Path,
    answers: usize,
) -> (Agent, Arc<ScriptedLlm>) {
    let llm = ScriptedLlm::new(
        (0..answers)
            .map(|_| text("deepseek-flash", "done"))
            .collect(),
    );
    let mut config = AgentConfig::new("deepseek-flash", path);
    config.instructions.harness_home = path.join("home");
    config.instructions.agents_home = path.join("agents");
    config.compaction.auto = false;
    let agent = Agent::new(
        config,
        AgentDeps {
            llm: llm.clone(),
            tools: ToolRegistry::new(),
            log,
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::AllowedOnce)),
            observer: None,
            tool_state: None,
        },
    );
    (agent, llm)
}

fn users(log: &dyn SessionLog) -> Vec<String> {
    log.events()
        .iter()
        .filter(|e| e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "user")
        .map(|e| e.data["content"][0]["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn reversed_driver_arrival_keeps_fifo_and_returns_each_own_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 2);
    let first = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("first")]),
        )
        .await
        .unwrap();
    let second = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("second")]),
        )
        .await
        .unwrap();
    let later = {
        let a = a.clone();
        tokio::spawn(async move { a.drive_queued(&second).await })
    };
    tokio::task::yield_now().await;
    assert!(
        !later.is_finished(),
        "later caller must wait for the FIFO head"
    );
    let earlier = a.drive_queued(&first).await.unwrap();
    let later = tokio::time::timeout(Duration::from_secs(2), later)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(earlier.turn, Some(1));
    assert_eq!(later.turn, Some(2));
    assert_eq!(users(log.as_ref()), ["first", "second"]);
    assert!(a.inbox_is_empty());
}

#[tokio::test]
async fn promotion_failure_preserves_original_and_success_is_one_batch() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 0);
    let id = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("move me")]),
        )
        .await
        .unwrap();
    a.inner.state.lock().unwrap().running = Some(Running {
        cancel: CancellationToken::new(),
        cause: None,
        ending: false,
    });
    let before = log.events();
    log.fail_promotion.store(true, Ordering::SeqCst);
    assert!(a.inner.promote(&id).await.is_err());
    assert_eq!(log.events(), before);
    assert_eq!(a.inner.state.lock().unwrap().next_turn[0].id, id);
    a.inner.promote(&id).await.unwrap();
    let st = a.inner.state.lock().unwrap();
    assert!(st.next_turn.is_empty());
    assert_eq!(st.next_step[0].id, id);
    drop(st);
    assert_eq!(log.batches.lock().unwrap().last().unwrap().len(), 2);
}

#[tokio::test]
async fn removed_owner_does_not_claim_the_next_callers_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log, dir.path(), 0);
    let first = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("first")]),
        )
        .await
        .unwrap();
    let second = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("second")]),
        )
        .await
        .unwrap();
    a.inner.state.lock().unwrap().own_prompt = Some(first.clone());
    a.update_queue(&first, QueueAction::Remove).await.unwrap();
    assert!(a.inner.claim(Target::NextTurn).await.unwrap().is_empty());
    assert!(a.inbox_contains(&second));
}

#[tokio::test]
async fn failed_user_commit_keeps_input_and_next_submission_retries_it() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, llm) = agent(log.clone(), dir.path(), 2);
    let message = user_message(vec![ContentBlock::text("retained")]);
    let id = message.id.clone();
    log.fail_commit.store(true, Ordering::SeqCst);
    let result = a.run_message(message).await.unwrap();
    assert!(matches!(result.reason, TurnEndReason::Error { .. }));
    assert!(a.inbox_contains(&id));
    assert!(users(log.as_ref()).is_empty());
    assert_eq!(llm.request_count(), 0);
    let done = tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("next")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.turn, Some(3));
    assert_eq!(users(log.as_ref()), ["retained", "next"]);
    assert!(a.inbox_is_empty());
}

#[tokio::test]
async fn restart_resumes_reserved_and_queued_prompts_before_new_input() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 0);
    let first = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("reserved")]),
        )
        .await
        .unwrap();
    a.accept(
        Target::NextTurn,
        user_message(vec![ContentBlock::text("queued")]),
    )
    .await
    .unwrap();
    a.inner.state.lock().unwrap().own_prompt = Some(first.clone());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        a.inner
            .pre_step(Target::NextTurn, 1, 1, &cancel)
            .await
            .is_err()
    );
    assert!(a.inbox_contains(&first));
    assert!(users(log.as_ref()).is_empty());
    let reopened = Arc::new(MemoryLog::with_events(log.header(), log.events()));
    drop(a);
    let (resumed, _) = agent(reopened.clone(), dir.path(), 3);
    tokio::time::timeout(
        Duration::from_secs(2),
        resumed.run_turn(vec![ContentBlock::text("new")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(users(reopened.as_ref()), ["reserved", "queued", "new"]);
    assert!(resumed.inbox_is_empty());
    assert!(
        !reopened
            .events()
            .iter()
            .any(|e| e.kind == kinds::INBOX_SPLICED && e.data["outcome"] == "canceled")
    );
}

#[tokio::test]
async fn failing_admission_flush_does_not_leave_an_ownerless_head() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 2);
    log.fail_flush.store(true, Ordering::SeqCst);
    let error = a
        .followup(user_message(vec![ContentBlock::text("save failed")]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("remains queued"));
    tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("next")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(users(log.as_ref()), ["save failed", "next"]);
    assert!(a.inbox_is_empty());
}

#[tokio::test]
async fn recovery_deduplicates_partial_promotion_and_partial_admission() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 0);
    let promoted = user_message(vec![ContentBlock::text("promoted")]);
    let admitted = user_message(vec![ContentBlock::text("already admitted")]);
    a.accept(Target::NextTurn, promoted.clone()).await.unwrap();
    a.accept(Target::NextTurn, admitted.clone()).await.unwrap();
    // A torn tail retained promotion's insertion but not its removal, and
    // another request's user message but not its corresponding dequeue.
    log.log
        .append(vec![
            inbox_splice_event(Target::NextStep, 0, 0, &[promoted], false),
            surface_append(kinds::USER_MESSAGE, message_json(&admitted), None),
        ])
        .await
        .unwrap();
    let reopened = Arc::new(MemoryLog::with_events(log.header(), log.events()));
    let (resumed, _) = agent(reopened.clone(), dir.path(), 2);
    tokio::time::timeout(
        Duration::from_secs(2),
        resumed.run_turn(vec![ContentBlock::text("new")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        users(reopened.as_ref()),
        ["already admitted", "promoted", "new"]
    );
    assert!(resumed.inbox_is_empty());
}

#[tokio::test]
async fn stable_id_retries_failed_ack_and_completed_input_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, llm) = agent(log.clone(), dir.path(), 2);
    let message = user_message(vec![ContentBlock::text("once")]);
    log.fail_flush.store(true, Ordering::SeqCst);
    assert!(a.followup(message.clone()).await.is_err());
    a.followup(message.clone()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("barrier")]),
    )
    .await
    .unwrap()
    .unwrap();
    a.followup(message.clone()).await.unwrap();
    let mut conflict = message;
    conflict.content = vec![ContentBlock::text("different")];
    assert!(
        a.followup(conflict)
            .await
            .unwrap_err()
            .to_string()
            .contains("different content")
    );
    assert_eq!(llm.request_count(), 2);
    assert_eq!(users(log.as_ref()), ["once", "barrier"]);
}

#[tokio::test]
async fn cancellation_cannot_readmit_a_reserved_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 0);
    let id = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("cancel me")]),
        )
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    {
        let mut st = a.inner.state.lock().unwrap();
        st.own_prompt = Some(id.clone());
        st.running = Some(Running {
            cancel: cancel.clone(),
            cause: None,
            ending: false,
        });
    }
    let claimed = a.inner.claim(Target::NextTurn).await.unwrap();
    a.cancel_durable().await.unwrap();
    assert!(
        a.inner
            .settle_claim(&[id].into_iter().collect(), &claimed, &[], &cancel)
            .await
            .is_err()
    );
    assert!(a.inbox_is_empty());
    assert!(users(log.as_ref()).is_empty());
}

#[tokio::test]
async fn rejected_claim_restores_before_removing_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 0);
    let id = a
        .accept(
            Target::NextTurn,
            user_message(vec![ContentBlock::text("restore me")]),
        )
        .await
        .unwrap();
    a.inner.state.lock().unwrap().own_prompt = Some(id.clone());
    let claimed = a.inner.claim(Target::NextTurn).await.unwrap();
    a.inner
        .settle_claim(
            &[id.clone()].into_iter().collect(),
            &[],
            &claimed,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let batches = log.batches.lock().unwrap();
    let last = batches.last().unwrap();
    assert_eq!(last[0].data["target"], "next-step");
    assert_eq!(last[0].data["inserted"][0]["id"], id);
    assert_eq!(last[1].data["target"], "next-turn");
    drop(batches);
    assert_eq!(a.inner.state.lock().unwrap().next_step[0].id, id);
}

#[tokio::test]
async fn dropped_admission_during_flush_keeps_a_recoverable_queue_owner() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, _) = agent(log.clone(), dir.path(), 2);
    log.gate_flush.store(true, Ordering::SeqCst);
    let message = user_message(vec![ContentBlock::text("interrupted admission")]);
    let id = message.id.clone();
    let admission = {
        let a = a.clone();
        tokio::spawn(async move { a.followup(message).await })
    };
    tokio::time::timeout(Duration::from_secs(2), log.flush_entered.notified())
        .await
        .unwrap();
    admission.abort();
    assert!(admission.await.unwrap_err().is_cancelled());
    assert!(
        log.events()
            .iter()
            .any(|e| e.kind == kinds::INBOX_SPLICED && e.data["inserted"][0]["id"] == id)
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("later")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(users(log.as_ref()), ["interrupted admission", "later"]);
    assert!(a.inbox_is_empty());
}

async fn wait_request(llm: &ScriptedLlm) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while llm.request_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropped_waiting_run_message_does_not_block_later_input() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, llm) = agent(log.clone(), dir.path(), 0);
    llm.push(crate::testing::Response::Hang(vec![]));
    llm.push(text("deepseek-flash", "queued done"));
    llm.push(text("deepseek-flash", "later done"));
    let active = {
        let a = a.clone();
        tokio::spawn(async move { a.run_turn(vec![ContentBlock::text("active")]).await })
    };
    wait_request(&llm).await;
    let message = user_message(vec![ContentBlock::text("queued")]);
    let id = message.id.clone();
    let waiting = {
        let a = a.clone();
        tokio::spawn(async move { a.run_message(message).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !a.inbox_contains(&id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    a.cancel_with(CancelCause::User, true).await;
    active.await.unwrap().unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("later")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(users(log.as_ref()), ["active", "queued", "later"]);
    assert!(a.inbox_is_empty());
}

#[tokio::test]
async fn dropped_active_run_repairs_its_turn_before_new_input() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, llm) = agent(log.clone(), dir.path(), 0);
    llm.push(crate::testing::Response::Hang(vec![]));
    llm.push(text("deepseek-flash", "later done"));
    let active = {
        let a = a.clone();
        tokio::spawn(async move { a.run_turn(vec![ContentBlock::text("active")]).await })
    };
    wait_request(&llm).await;
    active.abort();
    assert!(active.await.unwrap_err().is_cancelled());
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        a.run_turn(vec![ContentBlock::text("later")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.turn, Some(2));
    assert_eq!(users(log.as_ref()), ["active", "later"]);
    let ends: Vec<_> = log
        .events()
        .into_iter()
        .filter(|e| e.kind == kinds::TURN_END)
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].data["reason"]["kind"], "interrupted");
    dshx_session::validate::validate_log(&log.header(), &log.events()).unwrap();
    assert!(a.inbox_is_empty());
}

#[tokio::test]
async fn dropped_admission_transfers_ownership_for_an_already_waiting_follower() {
    let dir = tempfile::tempdir().unwrap();
    let log = TestLog::new(dir.path());
    let (a, llm) = agent(log.clone(), dir.path(), 2);
    log.gate_flush.store(true, Ordering::SeqCst);
    let blocked = {
        let a = a.clone();
        tokio::spawn(async move {
            a.followup(user_message(vec![ContentBlock::text("head")]))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), log.flush_entered.notified())
        .await
        .unwrap();
    let follower = user_message(vec![ContentBlock::text("already accepted")]);
    let follower_id = follower.id.clone();
    let follower = {
        let a = a.clone();
        tokio::spawn(async move { a.run_message(follower).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !a.inbox_contains(&follower_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(llm.request_count(), 0);
    blocked.abort();
    assert!(blocked.await.unwrap_err().is_cancelled());
    // No third send: the follower must progress on the ownership transfer.
    tokio::time::timeout(Duration::from_secs(2), follower)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(users(log.as_ref()), ["head", "already accepted"]);
    assert!(a.inbox_is_empty());
}
