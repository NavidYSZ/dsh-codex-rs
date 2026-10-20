//! bash + job tools end to end (real processes in temp dirs).
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{call, err, ok, text};
use dshx_core::{ApprovalDecision, SandboxMode, Tool};
use dshx_tools::bash::BashTool;
use dshx_tools::jobs::{JobConfig, JobEvent, JobKillTool, JobListTool, JobOutputTool};
use dshx_tools::testing::{FixedApprover, builder, context};
use dshx_tools::{JobRegistry, SpillStore, ToolState};
use serde_json::json;

fn bash() -> BashTool {
    BashTool::new()
}

#[tokio::test]
async fn foreground_output_and_markers() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let out = call(
        &bash(),
        &ctx,
        json!({"command": "echo TERMINAL_OK", "description": "Echo"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&out), "TERMINAL_OK\n");
    assert_eq!(out.meta, Some(json!({"cwd": dir.path().to_string_lossy()})));
    let value = out.value.unwrap();
    assert_eq!(value["kind"], "foreground");
    assert_eq!(value["exitCode"], 0);
    assert_eq!(
        value["sandbox"],
        json!({"mode": "danger-full-access", "denied": false})
    );

    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "echo out; echo err >&2; exit 3", "description": "Fail"}),
    )
    .await;
    assert_eq!(t, "out\n[stderr]\nerr\n[exit code: 3]");
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "true", "description": "Nothing"}),
    )
    .await;
    assert_eq!(t, "(no output)");
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "kill -TERM $$", "description": "Self kill"}),
    )
    .await;
    assert_eq!(t, "(no output)\n[killed by signal: SIGTERM]");
}

#[tokio::test]
async fn environment_and_workdir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let ctx = builder(dir.path()).session("sess-42").build();
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "echo $TERM $NO_COLOR $PAGER $GIT_PAGER $DSH_SHELL $DSH_SESSION_ID; pwd -P", "description": "Env", "workdir": "sub"}),
    )
    .await;
    let real = std::fs::canonicalize(dir.path().join("sub")).unwrap();
    assert_eq!(t, format!("dumb 1 cat cat 1 sess-42\n{}\n", real.display()));
    let t = ok(&bash(), &ctx, json!({"command": "env | cut -d= -f1 | grep -iE 'key|token|secret|password' || true", "description": "Scrub"})).await;
    assert_eq!(t, "(no output)");
}

#[tokio::test]
async fn argument_validation_uses_dsh_wording() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    assert_eq!(
        err(&bash(), &ctx, json!({"command": "ls"})).await,
        "invalid arguments: missing required property \"description\""
    );
    assert_eq!(
        err(&bash(), &ctx, json!({"command": " ", "description": "x"})).await,
        "invalid command: expected a non-empty string"
    );
    assert_eq!(
        err(
            &bash(),
            &ctx,
            json!({"command": "ls", "description": "x", "timeoutMs": -1})
        )
        .await,
        "invalid timeoutMs: expected a positive number, got -1"
    );
    assert_eq!(
        err(
            &bash(),
            &ctx,
            json!({"command": "ls", "description": "x", "sandbox_permissions": "workspace-write"})
        )
        .await,
        "invalid escalation: sandbox_permissions requires a justification"
    );
    // A blank justification without sandbox_permissions is ignored (dsh snapshot bash-tool-turn).
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "echo ok", "description": "x", "justification": " \t\n "}),
    )
    .await;
    assert_eq!(t, "ok\n");
    // Requesting the current mode needs no approval (dsh snapshot bash-same-mode-empty-justification).
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "echo ok", "description": "x", "sandbox_permissions": "danger-full-access", "justification": ""}),
    )
    .await;
    assert_eq!(t, "ok\n");
}

#[tokio::test]
async fn escalation_goes_through_the_approver() {
    let dir = tempfile::tempdir().unwrap();
    let rejecting = FixedApprover::new(ApprovalDecision::Rejected);
    let ctx = builder(dir.path())
        .sandbox(SandboxMode::ReadOnly)
        .approver(rejecting.clone())
        .build();
    let args = json!({"command": "touch x", "description": "Touch", "sandbox_permissions": "danger-full-access", "justification": "needs to write"});
    assert_eq!(
        err(&bash(), &ctx, args.clone()).await,
        "the user rejected escalating this command to \"danger-full-access\"; it stays denied, so stop and explain instead of working around it"
    );
    let asked = rejecting.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        [(
            "bash".into(),
            "escalate sandbox to danger-full-access: touch x".into(),
            Some("needs to write".into())
        )]
    );

    let approving = FixedApprover::new(ApprovalDecision::AllowedOnce);
    let ctx = builder(dir.path())
        .sandbox(SandboxMode::ReadOnly)
        .approver(approving)
        .build();
    let out = call(&bash(), &ctx, args).await.unwrap();
    assert_eq!(out.value.unwrap()["sandbox"]["mode"], "danger-full-access");
    assert!(dir.path().join("x").exists());

    let ctx = builder(dir.path())
        .sandbox(SandboxMode::DangerFullAccess)
        .build();
    assert_eq!(
        err(&bash(), &ctx, json!({"command": "ls", "description": "x", "sandbox_permissions": "workspace-write", "justification": "j"})).await,
        "sandbox escalation to \"workspace-write\" is not strictly wider than this call's current \"danger-full-access\" mode"
    );
}

#[tokio::test]
async fn long_output_keeps_the_tail_and_spills() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let out = call(
        &bash(),
        &ctx,
        json!({"command": "seq 1 30000", "description": "Count"}),
    )
    .await
    .unwrap();
    let t = text(&out);
    let value = out.value.unwrap();
    let spill = value["stdout"]["spillPath"].as_str().unwrap().to_owned();
    assert_eq!(value["stdout"]["truncated"], true);
    assert!(
        t.ends_with(&format!(
            "30000\n\n[output truncated; full output: {spill}]"
        )),
        "{}",
        &t[t.len() - 200..]
    );
    assert!(t.len() < 66_000);
    let full = std::fs::read_to_string(&spill).unwrap();
    assert!(full.starts_with("1\n2\n3\n"));
    assert!(full.ends_with("29999\n30000\n"));
    let _ = std::fs::remove_file(spill);
}

#[tokio::test]
async fn background_jobs_and_job_tools() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    let mut events = state.jobs().subscribe();
    let out = call(
        &bash(),
        &ctx,
        json!({"command": "echo one; sleep 0.3; echo two >&2", "description": "Bg", "run_in_background": true}),
    )
    .await
    .unwrap();
    let id = out.value.as_ref().unwrap()["jobId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("bash-") && id.len() == 21, "{id}");
    assert_eq!(text(&out), format!("started background job {id}"));

    let list = ok(&JobListTool, &ctx, json!({})).await;
    assert_eq!(
        list,
        format!("{id} [bash] running — echo one; sleep 0.3; echo two >&2")
    );

    let t = ok(
        &JobOutputTool,
        &ctx,
        json!({"job_id": id, "wait": true, "timeout_ms": 5000}),
    )
    .await;
    assert_eq!(t, "one\n[stderr]\ntwo\n[status: completed, exit code: 0]");
    let t = ok(&JobOutputTool, &ctx, json!({"job_id": id})).await;
    assert_eq!(t, "(no new output)\n[status: completed, exit code: 0]");
    let t = ok(&JobKillTool, &ctx, json!({"job_id": id})).await;
    assert_eq!(
        t,
        format!("job {id} had already finished [status: completed, exit code: 0]")
    );

    let registered = events.recv().await.unwrap();
    let e = registered.session_event().unwrap();
    assert_eq!(e.kind, "job/registered");
    assert_eq!(e.data["label"], "echo one; sleep 0.3; echo two >&2");
    assert_eq!(e.data["owner"], "test-session");
    let keys: Vec<&String> = e.data.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["jobId", "owner", "kind", "label", "startedAt", "toolCall"]
    );
    let settled = events.recv().await.unwrap();
    let e = settled.session_event().unwrap();
    assert_eq!(e.kind, "job/settled");
    assert_eq!(e.data["status"], "completed");
    assert_eq!(e.data["detail"], "exit code: 0");
    assert_eq!(e.data["cause"], "producer");
    assert_eq!(e.data["producerReported"], true);
    // The wait received the settlement, so no notice is due.
    assert!(settled.completion_notice().is_none());

    assert_eq!(
        err(&JobOutputTool, &ctx, json!({"job_id": "bash-nope"})).await,
        "unknown job bash-nope"
    );
    assert_eq!(
        err(&JobOutputTool, &ctx, json!({"job_id": ""})).await,
        "invalid job_id: expected a non-empty string, got \"\""
    );
    let other = builder(dir.path())
        .session("other")
        .state(Arc::new(ToolState::new()))
        .build();
    assert_eq!(
        ok(&JobListTool, &other, json!({})).await,
        "(no background jobs)"
    );
}

#[tokio::test]
async fn job_kill_reports_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    let mut events = state.jobs().subscribe();
    let out = call(&bash(), &ctx, json!({"command": "while :; do sleep 60; done", "description": "Hold", "run_in_background": true}))
        .await
        .unwrap();
    let id = out.value.unwrap()["jobId"].as_str().unwrap().to_owned();
    let t = ok(
        &JobKillTool,
        &ctx,
        json!({"job_id": id, "reason": "free the bounded task slot"}),
    )
    .await;
    assert_eq!(t, format!("requested cancellation of job {id}"));
    let _ = events.recv().await;
    let settled = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    let e = settled.session_event().unwrap();
    assert_eq!(e.data["status"], "killed");
    assert_eq!(e.data["cause"], "kill");
    assert_eq!(
        e.data["detail"],
        "signal: SIGTERM; free the bounded task slot"
    );
    assert!(
        settled.completion_notice().is_none(),
        "the model killed it itself"
    );
}

#[tokio::test]
async fn timed_out_foreground_moves_to_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    let mut events = state.jobs().subscribe();
    let out = call(
        &bash(),
        &ctx,
        json!({"command": "echo started; sleep 1; echo finished", "description": "Slow", "timeoutMs": 300}),
    )
    .await
    .unwrap();
    let value = out.value.clone().unwrap();
    assert_eq!(value["kind"], "promoted");
    let id = value["jobId"].as_str().unwrap().to_owned();
    assert_eq!(
        text(&out),
        format!(
            "started\n[still running after 300ms; moved to background job {id}]\nThe command keeps running in the background. You will be notified when it finishes; read newer output with job_output, stop it with job_kill."
        )
    );
    let _ = events.recv().await;
    // Nobody waits: the settlement produces a completion notice.
    let settled = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    let notice = settled.completion_notice().unwrap();
    assert_eq!(
        notice.text,
        format!(
            "background job {id} (bash: echo started; sleep 1; echo finished) finished [status: completed, exit code: 0]. Read its output with job_output."
        )
    );
    assert_eq!(
        notice.summary,
        "bash echo started; sleep 1; echo finished [status: completed, exit code: 0]"
    );
    let t = ok(&JobOutputTool, &ctx, json!({"job_id": id})).await;
    assert_eq!(t, "finished\n[status: completed, exit code: 0]");
}

#[tokio::test]
async fn refused_admission_runs_under_the_deadline_kill() {
    let dir = tempfile::tempdir().unwrap();
    let registry = JobRegistry::new(JobConfig {
        max_concurrent_jobs_per_owner: 0,
        ..JobConfig::default()
    });
    let state = Arc::new(ToolState::with_parts(registry, SpillStore::new()));
    let ctx = builder(dir.path()).state(state).build();
    let t = ok(
        &bash(),
        &ctx,
        json!({"command": "echo a; sleep 5", "description": "Slow", "timeoutMs": 200}),
    )
    .await;
    assert_eq!(t, "a\n[timed out after 200ms]\n[killed by signal: SIGTERM]");
    let e = err(
        &bash(),
        &ctx,
        json!({"command": "sleep 5", "description": "Bg", "run_in_background": true}),
    )
    .await;
    assert_eq!(
        e,
        "background job limit reached for this owner (limit: 0); use job_kill to stop an unneeded job, wait for it to finish, then retry"
    );
}

#[tokio::test]
async fn cancellation_kills_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = dshx_tools::testing::next_call(&context(dir.path()));
    let marker = dir.path().join("survived");
    let cancel = ctx.cancel.clone();
    let task = tokio::spawn({
        let ctx = ctx.clone();
        let cmd = format!("sleep 2; touch {}", marker.display());
        async move {
            bash()
                .execute(json!({"command": cmd, "description": "Slow"}), ctx)
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(dshx_core::ToolError::Cancelled)));
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(!marker.exists(), "the process group was killed");
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    assert!(
        state.jobs().list(Some("test-session")).is_empty(),
        "the aborted job left the registry"
    );
}

#[tokio::test]
async fn spawn_failure_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let e = err(
        &bash(),
        &ctx,
        json!({"command": "true", "description": "x", "workdir": "missing"}),
    )
    .await;
    assert_eq!(e, "spawn bash ENOENT");
}

#[tokio::test]
async fn shutdown_tears_down_without_notices() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    let mut events = state.jobs().subscribe();
    call(
        &bash(),
        &ctx,
        json!({"command": "sleep 60", "description": "Bg", "run_in_background": true}),
    )
    .await
    .unwrap();
    state.jobs().shutdown(None);
    let _ = events.recv().await;
    let settled = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(settled, JobEvent::Settled { .. }));
    assert_eq!(settled.session_event().unwrap().data["cause"], "teardown");
    assert!(settled.completion_notice().is_none());
}

#[tokio::test]
async fn a_dropped_call_kills_its_command() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let marker = dir.path().join("survived");
    let cmd = format!("sleep 1; touch {}", marker.display());
    let tool = bash();
    let call = tool.execute(
        json!({"command": cmd, "description": "Slow"}),
        dshx_tools::testing::next_call(&ctx),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), call)
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(!marker.exists(), "the abandoned command was killed");
}
