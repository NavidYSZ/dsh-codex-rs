//! Validate live mutations as strictly as replay and preserve a run across attach.
use std::sync::Arc;

use dshx_agent::testing::{FixedApprover, MemoryAttachments, ScriptedLlm};
use dshx_agent::{Agent, AgentConfig, AgentDeps};
use dshx_core::{ApprovalDecision, SessionLog, ToolRegistry};
use dshx_orchestrator::{
    Activation, Check, ContractInput, Criterion, Initiator, OrchestratorService,
    OrchestratorSettings, Phase, TaskSpec, stored_orchestration,
};
use dshx_session::MemoryLog;
use dshx_subagent::{MemorySessions, RuntimeDeps, SubagentRuntime};
use serde_json::json;

struct Harness {
    agent: Agent,
    log: Arc<MemoryLog>,
    service: Arc<OrchestratorService>,
    _dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let log = Arc::new(MemoryLog::new(dir.path().to_str().unwrap()));
    let attachments = Arc::new(MemoryAttachments::default());
    let approver = Arc::new(FixedApprover(ApprovalDecision::Rejected));
    let subagents = SubagentRuntime::new(RuntimeDeps {
        llm: llm.clone(),
        sessions: Arc::new(MemorySessions::default()),
        attachments: attachments.clone(),
        approver: approver.clone(),
    });
    let mut config = AgentConfig::new("deepseek-flash", dir.path());
    config.instructions.harness_home = dir.path().join("home/.dsh-codex");
    config.instructions.agents_home = dir.path().join("home/.agents");
    let agent = Agent::new(
        config,
        AgentDeps {
            llm,
            tools: ToolRegistry::new(),
            log: log.clone(),
            attachments,
            approver,
            observer: None,
            tool_state: None,
        },
    );
    let service = OrchestratorService::new(OrchestratorSettings::default(), subagents);
    service.attach(&agent, false);
    Harness {
        agent,
        log,
        service,
        _dir: dir,
    }
}

fn contract() -> ContractInput {
    ContractInput {
        objective: "Produce a verified report".into(),
        criteria: vec![Criterion {
            id: "report".into(),
            criterion: "Report exists".into(),
        }],
        limits: None,
    }
}

#[test]
fn partial_limits_keep_configured_defaults_and_invalid_limits_are_rejected() {
    let settings = OrchestratorSettings {
        default_max_rounds: 20,
        default_max_parallel: 2,
        default_max_workers: 5,
        ..OrchestratorSettings::default()
    };
    assert_eq!(settings.parse_limits(&json!(null)).unwrap(), None);
    let limits = settings
        .parse_limits(&json!({"maxRounds": 1}))
        .unwrap()
        .unwrap();
    assert_eq!(
        (limits.max_rounds, limits.max_parallel, limits.max_workers),
        (1, 2, 5)
    );
    for invalid in [
        json!([]),
        json!(3),
        json!({"maxRound": 2}),
        json!({"maxRounds": 0}),
        json!({"maxRounds": 10001}),
        json!({"maxParallel": 9}),
        json!({"maxWorkers": 65}),
        json!({"maxRounds": 1.5}),
        json!({"maxWorkers": "5"}),
    ] {
        assert!(settings.parse_limits(&invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn rejected_contract_and_completion_leave_the_journal_valid() {
    let h = harness();
    let id = h.agent.session_id();
    let mut invalid = contract();
    invalid.criteria.push(invalid.criteria[0].clone());
    assert!(
        h.service
            .create(&id, invalid, Initiator::Host)
            .await
            .is_err()
    );
    assert!(stored_orchestration(&h.log.events()).unwrap().is_none());
    let view = h
        .service
        .create(&id, contract(), Initiator::Host)
        .await
        .unwrap();
    let before = h.log.events().len();
    assert!(
        h.service
            .complete(&id, &view.reference(), vec![], Initiator::Agent)
            .await
            .is_err()
    );
    assert_eq!(
        h.log.events().len(),
        before,
        "invalid evidence must not be appended"
    );
    let done = h
        .service
        .complete(
            &id,
            &view.reference(),
            vec![Check {
                criterion_id: "report".into(),
                evidence: vec!["report.md".into()],
            }],
            Initiator::Agent,
        )
        .await
        .unwrap();
    assert_eq!(done.state.run.phase, Phase::Complete);
    assert_eq!(
        stored_orchestration(&h.log.events())
            .unwrap()
            .unwrap()
            .state,
        done.state
    );
}

#[tokio::test]
async fn restart_rearms_an_active_run_and_blocked_resume_clears_the_reason() {
    let h = harness();
    let id = h.agent.session_id();
    let created = h
        .service
        .create(&id, contract(), Initiator::Host)
        .await
        .unwrap();
    assert!(
        h.service
            .resume(&id, &created.reference(), Initiator::Host)
            .await
            .is_err()
    );
    h.service.detach(&id).await;
    h.service.attach(&h.agent, false);
    let restored = h.service.get(&id).unwrap().unwrap();
    assert_eq!(restored.activation, Activation::Disarmed);
    assert_eq!(restored.state, created.state);
    let resumed = h
        .service
        .resume(&id, &restored.reference(), Initiator::Host)
        .await
        .unwrap();
    assert_eq!(resumed.activation, Activation::Armed);
    let blocked = h
        .service
        .block(
            &id,
            &resumed.reference(),
            "needs-input",
            "Need the source",
            Initiator::Host,
        )
        .await
        .unwrap();
    let resumed = h
        .service
        .resume(&id, &blocked.reference(), Initiator::Host)
        .await
        .unwrap();
    assert!(resumed.state.run.blocked_reason.is_none());
    assert_eq!(
        stored_orchestration(&h.log.events())
            .unwrap()
            .unwrap()
            .state,
        resumed.state
    );
}

#[tokio::test]
async fn invalid_plan_is_rejected_before_persistence() {
    let h = harness();
    let id = h.agent.session_id();
    let view = h
        .service
        .create(&id, contract(), Initiator::Host)
        .await
        .unwrap();
    let before = h.log.events().len();
    let task = TaskSpec {
        id: "a".into(),
        description: "Do work".into(),
        depends_on: vec![],
    };
    assert!(
        h.service
            .plan(
                &id,
                &view.reference(),
                vec![task.clone(), task],
                Initiator::Agent
            )
            .await
            .is_err()
    );
    assert_eq!(h.log.events().len(), before);
    assert_eq!(h.service.get(&id).unwrap().unwrap().state, view.state);
}
