//! The persistent `bash` of dsh's Minimal preset: schema bytes, state that
//! survives between calls, dsh's result texts (exit status, shell exit,
//! timeout, truncation), cancellation, sandboxing and cleanup.
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::{call, ok};
use dshx_core::{SandboxMode, Tool, ToolError};
use dshx_tools::ToolState;
use dshx_tools::persistent_bash::{PersistentBashTool, SHELL_RESET_MESSAGE, schema};
use dshx_tools::sandbox::{Runner, bwrap_profile_args};
use dshx_tools::testing::{builder, context, next_call};
use serde_json::{Value, json};

fn finished(output: &str, code: i32) -> String {
    let marker = format!("[Command finished with exit code {code}]");
    if output.is_empty() {
        marker
    } else {
        format!("{output}\n{marker}")
    }
}

fn ws_path(dir: &Path) -> String {
    dir.canonicalize().unwrap().display().to_string()
}

#[test]
fn schema_matches_the_dsh_minimal_header() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/request-header-minimal-0.2.0-rc.2.json"
    );
    let header: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let tools = header["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let ours = serde_json::to_value(schema()).unwrap();
    assert_eq!(
        serde_json::to_string(&ours).unwrap(),
        serde_json::to_string(&tools[0]).unwrap()
    );
    assert_eq!(PersistentBashTool::new().schema(), schema());
    assert!(!PersistentBashTool::new().concurrency_safe());
}

#[tokio::test]
async fn directory_variables_and_functions_persist() {
    let work = tempfile::tempdir().unwrap();
    let ws = ws_path(work.path());
    let bash = PersistentBashTool::new();
    let ctx = context(work.path());
    let out = ok(
        &bash,
        &ctx,
        json!({"command": "mkdir sub && cd sub && export FOO=bar && greet() { echo \"hi $1\"; }"}),
    )
    .await;
    assert_eq!(out, finished("", 0));
    let out = ok(&bash, &ctx, json!({"command": "pwd; echo $FOO; greet you"})).await;
    assert_eq!(out, finished(&format!("{ws}/sub\nbar\nhi you"), 0));
    // Multi-line commands, stderr, missing trailing newline, exit status.
    let out = ok(
        &bash,
        &ctx,
        json!({"command": "echo a\necho 'b' >&2\nprintf c; (exit 4)"}),
    )
    .await;
    assert_eq!(out, finished("a\nb\nc", 4));
    // Quotes and backslashes reach bash unchanged.
    let out = ok(
        &bash,
        &ctx,
        json!({"command": r#"printf '%s\n' "it's" 'a\b'"#}),
    )
    .await;
    assert_eq!(out, finished("it's\na\\b", 0));
    // stdin is /dev/null: a reader ends at once instead of eating commands.
    let out = ok(&bash, &ctx, json!({"command": "cat; read x || echo eof"})).await;
    assert_eq!(out, finished("eof", 0));
    // The managed environment of dsh's shells.
    let out = ok(
        &bash,
        &ctx,
        json!({"command": "echo $DSH_SHELL $TERM $PAGER"}),
    )
    .await;
    assert_eq!(out, finished("1 dumb cat", 0));
}

#[tokio::test]
async fn invalid_arguments_use_dsh_wording() {
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new();
    let ctx = context(work.path());
    let e = call(&bash, &ctx, json!({})).await.unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid arguments: missing required property \"command\""
    );
    let e = call(&bash, &ctx, json!({"command": "  "}))
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "command must be a non-empty string");
}

#[tokio::test]
async fn an_exiting_shell_is_reset() {
    let work = tempfile::tempdir().unwrap();
    let ws = ws_path(work.path());
    let bash = PersistentBashTool::new();
    let ctx = context(work.path());
    ok(&bash, &ctx, json!({"command": "cd / && export KEEP=1"})).await;
    let out = ok(&bash, &ctx, json!({"command": "echo bye; exit 3"})).await;
    assert_eq!(
        out,
        format!("bye\n[shell exited: code 3]\n{SHELL_RESET_MESSAGE}")
    );
    let out = ok(&bash, &ctx, json!({"command": "pwd; echo \"[$KEEP]\""})).await;
    assert_eq!(out, finished(&format!("{ws}\n[]"), 0));
}

#[tokio::test]
async fn a_timeout_returns_partial_output_and_resets() {
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new().timeout(Duration::from_millis(1500));
    let ctx = context(work.path());
    ok(&bash, &ctx, json!({"command": "export KEEP=1"})).await;
    let started = std::time::Instant::now();
    let out = ok(&bash, &ctx, json!({"command": "echo partial; sleep 30"})).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        out,
        format!(
            "Your command timed out after 2 seconds or experienced an OOM error. Below is partial output:\npartial\n[Command timed out or OOM]\n{SHELL_RESET_MESSAGE}"
        )
    );
    let out = ok(&bash, &ctx, json!({"command": "echo \"[$KEEP]\""})).await;
    assert_eq!(out, finished("[]", 0));
}

#[tokio::test]
async fn long_output_keeps_its_head() {
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new().max_output_chars(10);
    let ctx = context(work.path());
    let out = ok(&bash, &ctx, json!({"command": "printf '0123456789abcdef'"})).await;
    assert!(
        out.starts_with("0123456789<response clipped><NOTE>To save on context"),
        "{out}"
    );
    assert!(
        out.ends_with("</NOTE>\n[Command finished with exit code 0]"),
        "{out}"
    );
    // Many megabytes still end at the marker and keep the head.
    let bash = PersistentBashTool::new();
    let out = ok(
        &bash,
        &ctx,
        json!({"command": "echo first; head -c 6000000 /dev/zero | tr '\\0' x; echo; echo last"}),
    )
    .await;
    assert!(out.starts_with("first\nxxxx"), "{}", &out[..40]);
    assert!(out.ends_with("[Command finished with exit code 0]"));
    assert!(out.chars().count() < 17_000);
}

#[tokio::test]
async fn cancelling_aborts_and_resets() {
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new();
    let ctx = context(work.path());
    ok(&bash, &ctx, json!({"command": "export KEEP=1"})).await;
    let call_ctx = next_call(&ctx);
    let cancel = call_ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let e = bash
        .execute(json!({"command": "sleep 30"}), call_ctx)
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::Cancelled), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let out = ok(&bash, &ctx, json!({"command": "echo \"[$KEEP]\""})).await;
    assert_eq!(out, finished("[]", 0));
}

#[tokio::test]
async fn calls_of_one_session_run_in_order() {
    let work = tempfile::tempdir().unwrap();
    let bash = std::sync::Arc::new(PersistentBashTool::new());
    let ctx = context(work.path());
    let first = {
        let (bash, ctx) = (bash.clone(), next_call(&ctx));
        tokio::spawn(async move {
            bash.execute(json!({"command": "sleep 0.3; echo one > order.txt"}), ctx)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let out = ok(
        &*bash,
        &ctx,
        json!({"command": "echo two >> order.txt; cat order.txt"}),
    )
    .await;
    first.await.unwrap().unwrap();
    assert_eq!(out, finished("one\ntwo", 0));
}

#[tokio::test]
async fn closing_kills_the_shell_and_its_children() {
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new();
    let state = std::sync::Arc::new(ToolState::new());
    let ctx = builder(work.path()).state(state.clone()).build();
    let out = ok(&bash, &ctx, json!({"command": "sleep 300 & echo $!"})).await;
    let pid: i32 = out.lines().next().unwrap().parse().unwrap();
    assert!(state.shells().is_open("test-session"));
    state.shells().close(Some("test-session"));
    assert!(!state.shells().is_open("test-session"));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // SAFETY: probing a pid with signal 0 sends nothing.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "background job survived"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The next call simply starts a new shell.
    let out = ok(&bash, &ctx, json!({"command": "echo again"})).await;
    assert_eq!(out, finished("again", 0));
}

/// A runner that records its profile arguments and runs the shell unconfined.
fn fake_runner(dir: &Path) -> Runner {
    let script = dir.join("fakerun");
    std::fs::write(
        &script,
        "#!/bin/bash\nwhile [ \"$1\" != \"--\" ]; do printf '%s\\n' \"$1\" >> \"$(dirname \"$0\")/profile.txt\"; shift; done\nshift\nprintf 'argv: %s\\n' \"$*\" >> \"$(dirname \"$0\")/profile.txt\"\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Runner::Command {
        argv: vec![script.to_string_lossy().into_owned()],
        failure_signatures: vec!["fakerun: ".into()],
    }
}

#[tokio::test]
async fn the_shell_runs_confined_and_restarts_when_the_mode_changes() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::with_runner(fake_runner(tools.path()));
    let state = std::sync::Arc::new(ToolState::new());
    let ctx = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .state(state.clone())
        .build();
    ok(&bash, &ctx, json!({"command": "export KEEP=1"})).await;
    let out = ok(&bash, &ctx, json!({"command": "echo \"[$KEEP]\""})).await;
    assert_eq!(out, finished("[1]", 0));
    let root = work.path().canonicalize().unwrap();
    let mut expected = bwrap_profile_args(SandboxMode::WorkspaceWrite, &root);
    expected.push("argv: bash --noprofile --norc".into());
    let profile = std::fs::read_to_string(tools.path().join("profile.txt")).unwrap();
    assert_eq!(
        profile.lines().collect::<Vec<_>>(),
        expected,
        "one shell for both calls"
    );

    // Read only from now on: a fresh shell under the narrower profile.
    let ro = builder(work.path())
        .sandbox(SandboxMode::ReadOnly)
        .state(state)
        .build();
    let out = ok(&bash, &ro, json!({"command": "echo \"[$KEEP]\""})).await;
    assert_eq!(
        out,
        format!(
            "[The persistent bash shell was restarted for the read-only sandbox mode; this command ran in a fresh shell from the workspace.]\n{}",
            finished("[]", 0)
        )
    );
    let profile = std::fs::read_to_string(tools.path().join("profile.txt")).unwrap();
    let mut both = expected.clone();
    both.extend(bwrap_profile_args(SandboxMode::ReadOnly, &root));
    both.push("argv: bash --noprofile --norc".into());
    assert_eq!(profile.lines().collect::<Vec<_>>(), both);
}

#[tokio::test]
async fn a_failing_runner_is_reported() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let script = tools.path().join("brokenrun");
    std::fs::write(
        &script,
        "#!/bin/bash\necho 'fakerun: cannot set up the sandbox' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bash = PersistentBashTool::with_runner(Runner::Command {
        argv: vec![script.to_string_lossy().into_owned()],
        failure_signatures: vec!["fakerun: ".into()],
    });
    let ctx = builder(work.path()).sandbox(SandboxMode::ReadOnly).build();
    let e = call(&bash, &ctx, json!({"command": "true"}))
        .await
        .unwrap_err();
    let text = e.to_string();
    assert!(
        text.starts_with(
            "sandbox mode \"read-only\" is requested but no sandbox backend is usable"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("Runner failure: fakerun: cannot set up the sandbox"),
        "{text}"
    );
}

/// The real host runner (sandbox-exec on macOS, bubblewrap on Linux) keeps a
/// read-only shell from writing into the workspace.
#[tokio::test]
async fn read_only_mode_is_enforced_by_the_host_runner() {
    let runner = tokio::task::spawn_blocking(dshx_tools::sandbox::host_runner)
        .await
        .unwrap();
    if runner == Runner::Unconfined {
        eprintln!("skipping: no usable sandbox runner on this host");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let bash = PersistentBashTool::new();
    let state = std::sync::Arc::new(ToolState::new());
    let ctx = builder(work.path())
        .sandbox(SandboxMode::ReadOnly)
        .state(state.clone())
        .build();
    let out = ok(&bash, &ctx, json!({"command": "touch denied.txt"})).await;
    assert!(
        out.ends_with("[Command finished with exit code 1]"),
        "{out}"
    );
    assert!(!work.path().join("denied.txt").exists());
    let ctx = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .state(state)
        .build();
    let out = ok(
        &bash,
        &ctx,
        json!({"command": "touch allowed.txt && echo ok"}),
    )
    .await;
    assert!(out.ends_with(&finished("ok", 0)), "{out}");
    assert!(work.path().join("allowed.txt").exists());
}
