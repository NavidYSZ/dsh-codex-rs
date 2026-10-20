//! Confinement wiring. Real bubblewrap tests skip when bwrap is unusable on
//! the host; the plumbing is exercised with a fake bwrap-compatible runner.
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::{call, err, has_bwrap, ok, text};
use dshx_core::SandboxMode;
use dshx_tools::bash::BashTool;
use dshx_tools::sandbox::{Runner, bwrap_profile_args, denial_marker, escalation_hint};
use dshx_tools::testing::builder;
use serde_json::json;

/// A runner that records its profile arguments and runs the command unconfined.
fn fake_runner(dir: &Path) -> Runner {
    let script = dir.join("fakerun");
    std::fs::write(
        &script,
        "#!/bin/bash\nlog=\"$FAKERUN_LOG\"\nwhile [ \"$1\" != \"--\" ]; do printf '%s\\n' \"$1\" >> \"$(dirname \"$0\")/profile.txt\"; shift; done\nshift\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Runner::Command {
        argv: vec![script.to_string_lossy().into_owned()],
        failure_signatures: vec!["fakerun: ".into()],
    }
}

#[tokio::test]
async fn confined_commands_get_the_bwrap_profile_and_denial_markers() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let bash = BashTool::with_runner(fake_runner(tools.path()));
    let ctx = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .build();
    let out = call(
        &bash,
        &ctx,
        json!({"command": "echo inside", "description": "Run"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&out), "inside\n");
    assert_eq!(
        out.value.unwrap()["sandbox"],
        json!({"mode": "workspace-write", "denied": false, "enforcement": "full"})
    );
    let profile = std::fs::read_to_string(tools.path().join("profile.txt")).unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let expected: Vec<String> = bwrap_profile_args(SandboxMode::WorkspaceWrite, &root);
    assert_eq!(profile.lines().collect::<Vec<_>>(), expected);

    let t = ok(
        &bash,
        &ctx,
        json!({"command": "echo 'touch: cannot touch /x: Read-only file system' >&2; exit 1", "description": "Deny"}),
    )
    .await;
    assert_eq!(
        t,
        format!(
            "[stderr]\ntouch: cannot touch /x: Read-only file system\n{}\n{}\n[exit code: 1]",
            denial_marker(SandboxMode::WorkspaceWrite),
            escalation_hint("command")
        )
    );
}

#[tokio::test]
async fn runner_failures_are_sandbox_errors() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let ctx = builder(work.path()).sandbox(SandboxMode::ReadOnly).build();
    let bash = BashTool::with_runner(fake_runner(tools.path()));
    let e = err(&bash, &ctx, json!({"command": "echo 'fakerun: cannot set up namespace' >&2; exit 1", "description": "x"})).await;
    assert_eq!(
        e,
        dshx_tools::sandbox::unavailable_message(
            SandboxMode::ReadOnly,
            Some("fakerun: cannot set up namespace")
        )
    );

    // A missing runner binary (dsh snapshot missing-sandbox-runner).
    let missing = work
        .path()
        .join(".dsh-missing-sandbox-runner")
        .to_string_lossy()
        .into_owned();
    let bash = BashTool::with_runner(Runner::Command {
        argv: vec![missing.clone()],
        failure_signatures: vec!["x: ".into()],
    });
    let state = dshx_tools::ToolState::get(&ctx).unwrap();
    let mut events = state.jobs().subscribe();
    let e = err(
        &bash,
        &ctx,
        json!({"command": "true", "description": "Exercise missing sandbox runner"}),
    )
    .await;
    assert_eq!(
        e,
        format!(
            "sandbox mode \"read-only\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access. Runner failure: Error: spawn {missing} ENOENT"
        )
    );
    let _ = events.recv().await;
    let settled = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        settled.session_event().unwrap().data["detail"],
        "killed before exit; [sandbox: the sandbox runner itself failed under read-only mode — the command did not run; this is a sandbox problem, not a command failure]"
    );
}

#[tokio::test]
async fn full_access_runs_unwrapped() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let bash = BashTool::with_runner(fake_runner(tools.path()));
    let ctx = builder(work.path())
        .sandbox(SandboxMode::DangerFullAccess)
        .build();
    ok(&bash, &ctx, json!({"command": "true", "description": "x"})).await;
    assert!(!tools.path().join("profile.txt").exists());
}

#[tokio::test]
async fn unconfined_fallback_still_runs() {
    let work = tempfile::tempdir().unwrap();
    let bash = BashTool::with_runner(Runner::Unconfined);
    let ctx = builder(work.path()).sandbox(SandboxMode::ReadOnly).build();
    let out = call(
        &bash,
        &ctx,
        json!({"command": "echo hi", "description": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&out), "hi\n");
    assert_eq!(
        out.value.unwrap()["sandbox"],
        json!({"mode": "read-only", "denied": false})
    );
}

#[tokio::test]
async fn real_bubblewrap_confines_writes() {
    if !has_bwrap() {
        eprintln!("skipping: bubblewrap is not usable on this host");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let outside =
        tempfile::tempdir_in(std::env::var("HOME").unwrap_or_else(|_| "/var/tmp".into())).unwrap();
    let bash = BashTool::with_runner(Runner::Bwrap);
    let ro = builder(work.path()).sandbox(SandboxMode::ReadOnly).build();
    let t = ok(
        &bash,
        &ro,
        json!({"command": "touch inside", "description": "x"}),
    )
    .await;
    assert!(t.contains(&denial_marker(SandboxMode::ReadOnly)), "{t}");
    let ww = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .build();
    ok(
        &bash,
        &ww,
        json!({"command": "touch inside", "description": "x"}),
    )
    .await;
    assert!(work.path().join("inside").exists());
    let cmd = format!("touch {}/x", outside.path().display());
    let t = ok(&bash, &ww, json!({"command": cmd, "description": "x"})).await;
    assert!(
        t.contains(&denial_marker(SandboxMode::WorkspaceWrite)),
        "{t}"
    );
}
