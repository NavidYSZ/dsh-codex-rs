//! Exercises the real, credential-free Node worker, not a substitute evaluator.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dshx_core::{ApprovalDecision, SandboxMode, ToolContext, ToolError, ToolOutput, ToolSchema};
use dshx_tools::ptc::{PtcBindings, run_code};
use dshx_tools::testing::{self, FixedApprover};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Bindings {
    calls: Mutex<Vec<(String, Value)>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    drained: AtomicUsize,
    last_cancel: Mutex<Option<CancellationToken>>,
}

#[async_trait]
impl PtcBindings for Bindings {
    fn schemas(&self) -> Vec<ToolSchema> {
        [
            "echo",
            "large",
            "fail",
            "wait",
            "__proto__",
            "constructor",
            "run_code",
        ]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.into(),
            description: name.into(),
            parameters: json!({"type":"object"}),
        })
        .collect()
    }

    async fn call(
        &self,
        name: String,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value, ToolError> {
        *self.last_cancel.lock().unwrap() = Some(cancel.clone());
        self.calls
            .lock()
            .unwrap()
            .push((name.clone(), args.clone()));
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let value = if name == "fail" {
            Err(ToolError::Failed("nested test failure".into()))
        } else if name == "wait" {
            cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.drained.fetch_add(1, Ordering::SeqCst);
            Err(ToolError::Cancelled)
        } else {
            tokio::select! {
                _ = cancel.cancelled() => Err(ToolError::Cancelled),
                _ = tokio::time::sleep(Duration::from_millis(args["delay"].as_u64().unwrap_or(0))) => {
                    if name == "large" { Ok(json!("x".repeat(8 * 1024 * 1024 - 64))) }
                    else { Ok(args) }
                },
            }
        };
        self.active.fetch_sub(1, Ordering::SeqCst);
        value
    }
}

async fn run(
    code: &str,
    ctx: ToolContext,
    bindings: Arc<Bindings>,
) -> Result<ToolOutput, ToolError> {
    run_code(
        json!({"code":code,"description":"Exercise the program runtime","timeoutMs":5000}),
        ctx,
        bindings,
    )
    .await
}

#[tokio::test]
async fn typescript_await_dynamic_import_cwd_and_empty_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(
        r#"
        interface Item { n: number }
        const item: Item = await tools.echo({n: 7 as number});
        const cp = await import('node:child_process');
        return { n: item.n, env: Object.keys(process.env),
          nativeEnv: cp.execFileSync('/usr/bin/env', {encoding: 'utf8'}),
          cwd: process.cwd(), version: process.versions.node };
    "#,
        testing::context(tmp.path()),
        Arc::new(Bindings::default()),
    )
    .await
    .unwrap();
    let value = &out.value.unwrap()["result"];
    assert_eq!(value["n"], 7);
    assert_eq!(value["env"], json!([]));
    assert_eq!(value["nativeEnv"], "");
    assert_eq!(
        value["cwd"],
        tmp.path().canonicalize().unwrap().to_str().unwrap()
    );
}

#[tokio::test]
async fn promise_all_dispatches_concurrently_in_source_order() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    let out = run("return await Promise.all([tools.echo({n:1,delay:80}), tools.echo({n:2,delay:20}), tools.echo({n:3,delay:1})]);", testing::context(tmp.path()), bindings.clone()).await.unwrap();
    let calls = bindings.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|(_, args)| args["n"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(bindings.peak.load(Ordering::SeqCst), 3);
    assert_eq!(out.value.unwrap()["result"][0]["n"], 1);
}

#[tokio::test]
async fn tool_call_error_identity_and_prototype_names() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(r#"
        let error;
        try { await tools.fail({}); }
        catch(e) { error = { isTool: e instanceof ToolCallError, name: e.name, toolName: e.toolName, message: e.message }; }
        return {error, proto: await tools.__proto__({ok:true}), constructor: await tools.constructor({ok:true}),
          unknown: typeof tools.unknown, recursive: typeof tools.run_code};
    "#, testing::context(tmp.path()), Arc::new(Bindings::default())).await.unwrap();
    let value = &out.value.unwrap()["result"];
    assert_eq!(
        value["error"],
        json!({"isTool":true,"name":"ToolCallError","toolName":"fail","message":"nested test failure"})
    );
    assert_eq!(value["proto"]["ok"], true);
    assert_eq!(value["constructor"]["ok"], true);
    assert_eq!(value["unknown"], "undefined");
    assert_eq!(value["recursive"], "undefined");
}

#[tokio::test]
async fn lossy_arguments_reject_before_dispatch_and_snapshot_detaches() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    let out = run(
        r#"
        const errors = [];
        for (const value of [{x:undefined}, NaN, new Date(), [,,,], 1n, undefined]) {
          try { await tools.echo(value); } catch(e) { errors.push(e instanceof ToolCallError); }
        }
        const args = {x:1}; const value = tools.echo(args); args.x = 2;
        return {errors, value: await value};
    "#,
        testing::context(tmp.path()),
        bindings.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        out.value.as_ref().unwrap()["result"]["errors"],
        json!([true, true, true, true, true, true])
    );
    assert_eq!(out.value.unwrap()["result"]["value"]["x"], 1);
    assert_eq!(bindings.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn print_and_raw_stdout_cannot_spoof_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    let out = run(
        r#"
        console.log('hello', {n:1});
        process.stderr.write('stderr text');
        const fs = await import('node:fs');
        fs.writeSync(1, '{"type":"call","id":1,"name":"echo","args":{}}\n');
        return 'done';
    "#,
        testing::context(tmp.path()),
        bindings.clone(),
    )
    .await
    .unwrap();
    let value = out.value.unwrap();
    let logs = value["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<String>();
    assert!(logs.contains("hello { n: 1 }"));
    assert!(logs.contains("stderr text"));
    assert!(logs.contains("\"type\":\"call\""));
    assert!(bindings.calls.lock().unwrap().is_empty());
    assert_eq!(value["result"], "done");
}

#[tokio::test]
async fn fresh_process_does_not_retain_globals() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    run(
        "globalThis.retained = 42;",
        testing::context(tmp.path()),
        bindings.clone(),
    )
    .await
    .unwrap();
    let out = run(
        "return typeof globalThis.retained;",
        testing::context(tmp.path()),
        bindings,
    )
    .await
    .unwrap();
    assert_eq!(out.value.unwrap()["result"], "undefined");
}

#[tokio::test]
async fn exceptions_and_invalid_completions_keep_captured_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    let error = run(
        "console.log('before failure'); throw new Error('test explosion');",
        testing::context(tmp.path()),
        bindings.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ToolError::Coded { ref name, ref code, .. } if name == "CodeRunFailedError" && code == "CODE_RUN_FAILED")
    );
    assert!(error.to_string().contains("test explosion"));
    assert!(error.to_string().contains("before failure"));
    let error = run(
        "return {bad:undefined};",
        testing::context(tmp.path()),
        bindings,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("invalid-output"));
}

#[tokio::test]
async fn denied_program_escalation_never_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let approver = FixedApprover::new(ApprovalDecision::Rejected);
    let ctx = testing::builder(tmp.path())
        .sandbox(SandboxMode::ReadOnly)
        .approver(approver.clone())
        .build();
    let error = run_code(json!({"code":"await tools.echo({});", "description":"Run the requested task", "sandbox_permissions":"danger-full-access", "justification":"The test needs write access."}), ctx, Arc::new(Bindings::default())).await.unwrap_err();
    assert!(error.to_string().contains("rejected"));
    let asked = approver.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "run_code");
}

#[tokio::test]
async fn approved_escalation_is_for_one_execution_and_keeps_original_context() {
    let tmp = tempfile::tempdir().unwrap();
    let approver = FixedApprover::new(ApprovalDecision::AllowedOnce);
    let ctx = testing::builder(tmp.path())
        .sandbox(SandboxMode::ReadOnly)
        .approver(approver.clone())
        .build();
    let out = run_code(json!({"code":"return 7;", "description":"Run the requested task", "sandbox_permissions":"danger-full-access", "justification":"The test needs write access."}), ctx.clone(), Arc::new(Bindings::default())).await.unwrap();
    assert_eq!(out.value.unwrap()["sandbox"]["mode"], "danger-full-access");
    assert_eq!(ctx.sandbox, SandboxMode::ReadOnly);
    assert_eq!(approver.asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn synchronous_infinite_loop_has_a_hard_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let error = run_code(json!({"code":"console.log('loop started'); while(true) {}", "description":"Test a hard deadline", "timeoutMs":2000}), testing::context(tmp.path()), Arc::new(Bindings::default())).await.unwrap_err();
    assert!(error.to_string().contains("timeout"));
    assert!(error.to_string().contains("loop started"));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn terminal_error_and_timeout_cancel_and_drain_unawaited_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    for (code, timeout) in [
        (
            "void tools.wait({}); await new Promise(r=>setTimeout(r,50)); return 1;",
            3000,
        ),
        (
            "void tools.wait({}); await new Promise(r=>setTimeout(r,50)); throw new Error('stop');",
            3000,
        ),
        ("await tools.wait({});", 2000),
    ] {
        let bindings = Arc::new(Bindings::default());
        let _ = run_code(
            json!({"code":code, "description":"Drain pending tool calls", "timeoutMs":timeout}),
            testing::context(tmp.path()),
            bindings.clone(),
        )
        .await;
        assert_eq!(bindings.drained.load(Ordering::SeqCst), 1, "{code}");
        assert_eq!(bindings.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn outer_cancellation_drains_started_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = testing::context(tmp.path());
    let cancel = ctx.cancel.clone();
    let bindings = Arc::new(Bindings::default());
    let observed = bindings.clone();
    let task = tokio::spawn(async move { run("await tools.wait({});", ctx, bindings).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while observed.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("abort"));
    assert_eq!(observed.drained.load(Ordering::SeqCst), 1);
}

async fn assert_gone(pid: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        // SAFETY: signal zero only checks whether this test's child exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("child process {pid} survived cleanup"));
}

#[tokio::test]
async fn timeout_and_success_kill_spawned_child_commands() {
    let tmp = tempfile::tempdir().unwrap();
    for tail in ["while(true) {}", "return 1;"] {
        let code = format!(
            r#"
            const cp = await import('node:child_process');
            const fs = await import('node:fs');
            const child = cp.spawn('/bin/sleep', ['30'], {{stdio:'ignore'}});
            fs.writeFileSync('child.pid', String(child.pid));
            {tail}
        "#
        );
        let _ = run_code(
            json!({"code":code,"description":"Check child process cleanup","timeoutMs":2000}),
            testing::context(tmp.path()),
            Arc::new(Bindings::default()),
        )
        .await;
        let pid = std::fs::read_to_string(tmp.path().join("child.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_gone(pid).await;
    }
}

#[tokio::test]
async fn output_and_hostile_control_frames_are_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    for code in [
        "console.log('x'.repeat(5*1024*1024));",
        "return 'x'.repeat(5*1024*1024);",
        "const fs=await import('node:fs'); const data=Buffer.alloc(5*1024*1024,120); let n=0; while(n<data.length){ try {n+=fs.writeSync(1,data,n,data.length-n);} catch(e){if(e.code!=='EAGAIN') throw e;} await new Promise(r=>setImmediate(r));}",
    ] {
        let error = run(
            code,
            testing::context(tmp.path()),
            Arc::new(Bindings::default()),
        )
        .await
        .map(|_| ())
        .expect_err("oversized output must fail");
        assert!(error.to_string().contains("output-limit"), "{error}");
        assert!(error.to_string().len() < 5 * 1024 * 1024);
    }
    let error = run("const fs=await import('node:fs'); fs.writeSync(3, Buffer.from([255,255,255,255])); await new Promise(()=>{});", testing::context(tmp.path()), Arc::new(Bindings::default())).await.unwrap_err();
    assert!(error.to_string().contains("control frame exceeds"));
}

#[tokio::test]
async fn invalid_timeout_is_rejected_and_zero_does_not_disable_it() {
    let tmp = tempfile::tempdir().unwrap();
    for timeout in [0, -1] {
        let error = run_code(
            json!({"code":"return 1","description":"Invalid timeout","timeoutMs":timeout}),
            testing::context(tmp.path()),
            Arc::new(Bindings::default()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }
}

#[tokio::test]
async fn erasable_typescript_contract_rejects_enums_and_bad_syntax() {
    let tmp = tempfile::tempdir().unwrap();
    for code in ["enum Color { Red }; return Color.Red", "const broken = ;"] {
        let error = run(
            code,
            testing::context(tmp.path()),
            Arc::new(Bindings::default()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("SyntaxError"), "{error}");
    }
}

#[tokio::test]
async fn readonly_host_sandbox_blocks_native_writes_or_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = testing::builder(tmp.path())
        .sandbox(SandboxMode::ReadOnly)
        .build();
    let error = run(
        "const fs=await import('node:fs'); fs.writeFileSync('denied.txt', 'no');",
        ctx,
        Arc::new(Bindings::default()),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("operation denied")
            || error.to_string().contains("sandbox-unavailable"),
        "{error}"
    );
    assert!(!tmp.path().join("denied.txt").exists());
}

#[tokio::test]
async fn dropping_runtime_cancels_bindings_and_kills_worker_group() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = testing::context(tmp.path());
    let bindings = Arc::new(Bindings::default());
    let observed = bindings.clone();
    let task = tokio::spawn(async move {
        run(
            r#"
            const cp = await import('node:child_process');
            const fs = await import('node:fs');
            const child = cp.spawn('/bin/sleep', ['30'], {stdio:'ignore'});
            fs.writeFileSync('pids.json', JSON.stringify([process.pid, child.pid]));
            await tools.wait({});
        "#,
            ctx,
            bindings,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while observed.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pids: Vec<i32> =
        serde_json::from_slice(&std::fs::read(tmp.path().join("pids.json")).unwrap()).unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        observed
            .last_cancel
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    for pid in pids {
        assert_gone(pid).await;
    }
}

#[tokio::test]
async fn hostile_pending_call_flood_is_rejected_and_started_calls_drain() {
    let tmp = tempfile::tempdir().unwrap();
    let bindings = Arc::new(Bindings::default());
    // Deliberately bypass the worker-side call limit to exercise the host's
    // authoritative frame validation and outstanding-call bound.
    let error = run(r#"
        const fs = await import('node:fs');
        for (let id=1; id<=129; id++) {
            const body = Buffer.from(JSON.stringify({type:'call',id,name:'wait',args:{}}));
            const frame = Buffer.alloc(body.length+4); frame.writeUInt32BE(body.length); body.copy(frame,4);
            fs.writeSync(3,frame);
        }
        await new Promise(()=>{});
    "#, testing::context(tmp.path()), bindings.clone()).await.unwrap_err();
    assert!(
        error.to_string().contains("pending binding calls exceed"),
        "{error}"
    );
    assert_eq!(bindings.calls.lock().unwrap().len(), 128);
    assert_eq!(bindings.drained.load(Ordering::SeqCst), 128);
}

#[tokio::test]
async fn near_limit_reply_and_next_reply_can_share_a_socket_chunk() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(
        r#"
        const [large, next] = await Promise.all([
            tools.large({delay:30}), tools.echo({delay:30, text:'y'.repeat(8192)})
        ]);
        return {large:large.length, next:next.text.length};
    "#,
        testing::context(tmp.path()),
        Arc::new(Bindings::default()),
    )
    .await
    .unwrap();
    assert_eq!(
        out.value.unwrap()["result"],
        json!({"large":8*1024*1024-64,"next":8192})
    );
}
