//! The `bash` tool (dsh `tool-bash` over `bash-sandbox`/`bash-local`).
//!
//! Every call runs a fresh `bash -c` in its own process group and registers it
//! as a job while it runs. `run_in_background` returns the job id at once; a
//! foreground call waits until the command finishes or `timeoutMs` passes, and
//! then hands the still-running command over to the background (`promoted`).
//! When the job registry refuses the call (owner limit), the command runs under
//! a plain deadline kill instead.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{SandboxMode, Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::args::{self, fail};
use crate::env::child_env;
use crate::jobs::{
    JobChannel, JobOutcome, JobSpec, JobStatus, JobToolCall, dropped_notice, split_streams,
};
use crate::output::{Collected, MAX_OUTPUT_BYTES, MAX_SPILL_BYTES};
use crate::process::{Channel, DEFAULT_GRACE, Outcome, OutputHook, Process, SpawnSpec};
use crate::sandbox::{
    self, Confined, Runner, approve_escalation, denial_marker, escalation_hint, mode_name,
    runner_failed_marker, validate_escalation_args,
};
use crate::schemas::upstream_schema;
use crate::state::ToolState;
use crate::util::js_number;

/// Default foreground timeout: dsh's shipped base profile sets the bash
/// executor's `timeoutMs` to 60 s (the bare bash-local default is 120 s).
pub const DEFAULT_TIMEOUT_MS: f64 = 60_000.0;
/// Cap on `timeoutMs` (dsh bash-local `maxTimeoutMs`).
pub const MAX_TIMEOUT_MS: f64 = 600_000.0;

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    description: String,
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<f64>,
    workdir: Option<String>,
    run_in_background: Option<bool>,
    sandbox_permissions: Option<String>,
    justification: Option<String>,
}

/// The `bash` tool.
pub struct BashTool {
    runner: Option<Runner>,
    promote_on_timeout: bool,
    grace: Duration,
    default_timeout_ms: f64,
}

impl Default for BashTool {
    fn default() -> Self {
        BashTool::new()
    }
}

impl BashTool {
    /// Confines with the host runner (bubblewrap / sandbox-exec, probed once).
    pub fn new() -> Self {
        BashTool {
            runner: None,
            promote_on_timeout: true,
            grace: DEFAULT_GRACE,
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// Timeout for calls without `timeoutMs` (capped at [`MAX_TIMEOUT_MS`]).
    pub fn default_timeout_ms(mut self, ms: f64) -> Self {
        self.default_timeout_ms = ms;
        self
    }

    /// Uses `runner` instead of the probed host runner.
    pub fn with_runner(runner: Runner) -> Self {
        BashTool {
            runner: Some(runner),
            ..BashTool::new()
        }
    }

    /// When false, a foreground command that reaches its timeout is killed
    /// instead of moving to the background (dsh `promoteOnTimeout`).
    pub fn promote_on_timeout(mut self, promote: bool) -> Self {
        self.promote_on_timeout = promote;
        self
    }

    async fn runner(&self) -> Runner {
        match &self.runner {
            Some(runner) => runner.clone(),
            None => tokio::task::spawn_blocking(sandbox::host_runner)
                .await
                .unwrap_or(Runner::Unconfined),
        }
    }
}

/// Settled confinement facts of one run (dsh `ShellSandboxInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxInfo {
    mode: SandboxMode,
    denied: bool,
    enforcement: Option<&'static str>,
    runner_failed: bool,
}

impl SandboxInfo {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("mode".into(), json!(mode_name(self.mode)));
        map.insert("denied".into(), json!(self.denied));
        if let Some(enforcement) = self.enforcement {
            map.insert("enforcement".into(), json!(enforcement));
        }
        if self.runner_failed {
            map.insert("runnerFailed".into(), json!(true));
        }
        Value::Object(map)
    }
}

/// One prepared execution: the argv and how to classify it afterwards.
struct Prepared {
    argv: Vec<String>,
    mode: SandboxMode,
    confined: Option<Confined>,
    workdir: String,
}

fn prepare(
    runner: &Runner,
    command: &str,
    mode: SandboxMode,
    workspace_root: &Path,
    workdir: &str,
) -> Prepared {
    let confined = sandbox::confine(runner, command, mode, workspace_root);
    let confines = confined.enforcement.is_some();
    Prepared {
        argv: confined.argv.clone(),
        mode,
        confined: confines.then_some(confined),
        workdir: workdir.to_owned(),
    }
}

impl Prepared {
    /// dsh `isRunnerSpawnFailure`: a failed spawn blames the runner only when
    /// the working directory itself is usable.
    fn runner_spawn_failure(&self, outcome: &Outcome) -> bool {
        self.confined.is_some()
            && outcome.spawn_error.is_some()
            && Path::new(&self.workdir).is_dir()
    }
}

/// Sandbox facts once the process settled (dsh `onProcessDone`).
fn settled_sandbox(prepared: &Prepared, outcome: &Outcome, stderr: &str) -> SandboxInfo {
    let Some(confined) = &prepared.confined else {
        return SandboxInfo {
            mode: prepared.mode,
            denied: false,
            enforcement: None,
            runner_failed: false,
        };
    };
    let runner_failed = match &outcome.spawn_error {
        Some(_) => prepared.runner_spawn_failure(outcome),
        None => sandbox::runner_failure(outcome.exit_code, stderr, &confined.failure_signatures)
            .is_some(),
    };
    SandboxInfo {
        mode: prepared.mode,
        denied: !runner_failed
            && sandbox::matches_signature(outcome.exit_code, stderr, &confined.denial_signatures),
        enforcement: confined.enforcement,
        runner_failed,
    }
}

fn sandbox_notes(info: &SandboxInfo) -> Vec<String> {
    if info.runner_failed {
        return vec![runner_failed_marker(info.mode)];
    }
    if info.denied {
        return vec![denial_marker(info.mode), escalation_hint("command")];
    }
    Vec::new()
}

/// dsh `processOutcome`: the job's terminal status and detail.
fn job_outcome(outcome: &Outcome, info: &SandboxInfo) -> JobOutcome {
    let (status, detail) = if outcome.killed {
        let detail = match &outcome.signal {
            Some(signal) => format!("signal: {signal}"),
            None => "killed before exit".to_owned(),
        };
        (JobStatus::Killed, detail)
    } else {
        (
            JobStatus::Completed,
            format!("exit code: {}", outcome.exit_code.unwrap_or(0)),
        )
    };
    let notes = sandbox_notes(info);
    let detail = if notes.is_empty() {
        detail
    } else {
        format!("{detail}; {}", notes.join(" "))
    };
    JobOutcome {
        status,
        detail: Some(detail),
        result: None,
    }
}

fn stream_json(output: &Collected) -> Value {
    let mut map = Map::new();
    map.insert("text".into(), json!(output.text));
    map.insert("truncated".into(), json!(output.truncated));
    if let Some(path) = &output.spill_path {
        map.insert("spillPath".into(), json!(path));
    }
    Value::Object(map)
}

fn stream_text(output: &Collected) -> String {
    if !output.truncated {
        return output.text.clone();
    }
    let path = output.spill_path.as_deref().unwrap_or("(unavailable)");
    format!("{}\n[output truncated; full output: {path}]", output.text)
}

/// A finished foreground run.
struct RunResult {
    outcome: Outcome,
    timed_out: bool,
    aborted: bool,
    timeout_ms: f64,
    stdout: Collected,
    stderr: Collected,
    sandbox: SandboxInfo,
    stopped: Option<String>,
}

impl RunResult {
    fn exit_code_json(&self) -> Value {
        self.outcome
            .exit_code
            .map_or(Value::Null, |code| json!(code))
    }

    fn to_value(&self, cwd: &str) -> Value {
        let mut map = Map::new();
        map.insert("kind".into(), json!("foreground"));
        map.insert("cwd".into(), json!(cwd));
        map.insert("exitCode".into(), self.exit_code_json());
        map.insert("signal".into(), json!(self.outcome.signal));
        map.insert("timedOut".into(), json!(self.timed_out));
        map.insert("aborted".into(), json!(self.aborted));
        map.insert("timeoutMs".into(), json!(self.timeout_ms));
        map.insert("stdout".into(), stream_json(&self.stdout));
        map.insert("stderr".into(), stream_json(&self.stderr));
        map.insert("sandbox".into(), self.sandbox.to_json());
        if let Some(stopped) = &self.stopped {
            map.insert("stopped".into(), json!(stopped));
        }
        Value::Object(map)
    }

    /// dsh `renderResult`.
    fn render(&self) -> String {
        let out = stream_text(&self.stdout);
        let err = stream_text(&self.stderr);
        let mut body = out;
        if !err.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str("[stderr]\n");
            body.push_str(&err);
        }
        if body.is_empty() {
            body = "(no output)".into();
        }
        let mut markers = Vec::new();
        if self.sandbox.denied {
            markers.push(denial_marker(self.sandbox.mode));
            markers.push(escalation_hint("command"));
        }
        if self.timed_out {
            markers.push(format!(
                "[timed out after {}ms]",
                js_number(self.timeout_ms)
            ));
        }
        if let Some(stopped) = &self.stopped {
            markers.push(format!("[stopped: {stopped}]"));
        }
        match (&self.outcome.signal, self.outcome.exit_code) {
            (Some(signal), _) => markers.push(format!("[killed by signal: {signal}]")),
            (None, Some(0)) => {}
            (None, Some(code)) => markers.push(format!("[exit code: {code}]")),
            (None, None) => markers.push("[exit code: null]".into()),
        }
        if markers.is_empty() {
            return body;
        }
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body + &markers.join("\n")
    }
}

/// Runs a closure when dropped unless disarmed (cleanup for abandoned calls).
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    fn new(f: F) -> Self {
        OnDrop(Some(f))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// dsh `renderPromoted`.
fn render_promoted(output: &str, timeout_ms: f64, job_id: &str) -> String {
    let body = if output.is_empty() {
        String::new()
    } else if output.ends_with('\n') {
        output.to_owned()
    } else {
        format!("{output}\n")
    };
    format!(
        "{body}[still running after {}ms; moved to background job {job_id}]\nThe command keeps running in the background. You will be notified when it finishes; read newer output with job_output, stop it with job_kill.",
        js_number(timeout_ms)
    )
}

fn output_with_meta(text: String, value: Value, cwd: &str) -> ToolOutput {
    ToolOutput {
        value: Some(value),
        meta: Some(json!({"cwd": cwd})),
        ..ToolOutput::text(text)
    }
}

fn validate(args: &BashArgs, effective: SandboxMode) -> Result<(), ToolError> {
    if args.command.trim().is_empty() {
        return Err(fail("invalid command: expected a non-empty string"));
    }
    if args.description.trim().is_empty() {
        return Err(fail("invalid description: expected a non-empty string"));
    }
    if let Some(t) = args.timeout_ms
        && (!t.is_finite() || t <= 0.0)
    {
        return Err(fail(format!(
            "invalid timeoutMs: expected a positive number, got {}",
            js_number(t)
        )));
    }
    if args.sandbox_permissions.as_deref() == Some(mode_name(effective)) {
        return Ok(());
    }
    let justification = if args.sandbox_permissions.is_none()
        && args.justification.as_deref().map(str::trim) == Some("")
    {
        None
    } else {
        args.justification.as_deref()
    };
    validate_escalation_args(args.sandbox_permissions.as_deref(), justification)
}

/// dsh `resolveWorkdir`: a relative override joins the session directory.
fn resolve_workdir(workdir: Option<&str>, cwd: &Path) -> String {
    let cwd = cwd.to_string_lossy();
    match workdir {
        None => cwd.into_owned(),
        Some(dir) if Path::new(dir).is_absolute() => dir.to_owned(),
        Some(dir) => format!("{cwd}/{dir}"),
    }
}

fn spawn_spec(argv: Vec<String>, workdir: &str, session_id: &str, grace: Duration) -> SpawnSpec {
    SpawnSpec {
        argv,
        cwd: PathBuf::from(workdir),
        env: child_env(session_id),
        stdout_max: MAX_OUTPUT_BYTES,
        stderr_max: MAX_OUTPUT_BYTES,
        max_spill: Some(MAX_SPILL_BYTES),
        grace,
    }
}

/// The foreground result once the process settled, or the error dsh throws.
fn foreground_result(
    process: &Process,
    outcome: Outcome,
    prepared: &Prepared,
    timing: (bool, bool, f64),
    stopped: Option<String>,
) -> Result<RunResult, ToolError> {
    let stderr = process.stderr();
    if let Some(error) = &outcome.spawn_error {
        if prepared.runner_spawn_failure(&outcome) {
            return Err(fail(sandbox::unavailable_message(
                prepared.mode,
                Some(&format!("Error: {error}")),
            )));
        }
        return Err(fail(error.clone()));
    }
    if let Some(confined) = &prepared.confined
        && let Some(line) = sandbox::runner_failure(
            outcome.exit_code,
            &stderr.text,
            &confined.failure_signatures,
        )
    {
        return Err(fail(sandbox::unavailable_message(
            prepared.mode,
            Some(&line),
        )));
    }
    let sandbox = settled_sandbox(prepared, &outcome, &stderr.text);
    let (timed_out, aborted, timeout_ms) = timing;
    Ok(RunResult {
        outcome,
        timed_out,
        aborted,
        timeout_ms,
        stdout: process.stdout(),
        stderr,
        sandbox,
        stopped,
    })
}

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("bash")
    }

    fn describe(&self, args: &Value) -> String {
        args["command"].as_str().unwrap_or("bash").to_owned()
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = args::parse("bash", args)?;
        let standing = ctx.sandbox;
        validate(&args, standing)?;
        let mode = match (&args.sandbox_permissions, &args.justification) {
            (Some(requested), Some(justification)) => {
                approve_escalation(
                    &ctx,
                    "bash",
                    "command",
                    requested,
                    justification,
                    standing,
                    &args.command,
                )
                .await?
            }
            _ => standing,
        };
        let state = ToolState::get(&ctx)?;
        let workdir = resolve_workdir(args.workdir.as_deref(), &ctx.cwd);
        let timeout_ms = args
            .timeout_ms
            .unwrap_or(self.default_timeout_ms)
            .min(MAX_TIMEOUT_MS);
        let runner = if mode == SandboxMode::DangerFullAccess {
            Runner::Unconfined
        } else {
            self.runner().await
        };
        let prepared = Arc::new(prepare(&runner, &args.command, mode, &ctx.cwd, &workdir));
        let jobs = state.jobs();
        let job_spec = || JobSpec {
            kind: "bash".into(),
            label: args.command.clone(),
            owner: Some(ctx.session_id.clone()),
            tool_call: Some(JobToolCall {
                call_id: ctx.call_id.clone(),
                root_call_id: ctx.call_id.clone(),
            }),
        };

        if args.run_in_background == Some(true) {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let sink = jobs.start(job_spec()).map_err(fail)?;
            self.start_job(
                &sink,
                &prepared,
                &workdir,
                &ctx.session_id,
                Arc::new(Mutex::new(None)),
            );
            let job_id = sink.id().to_owned();
            let value = json!({"kind": "background", "jobId": job_id, "cwd": workdir});
            return Ok(output_with_meta(
                format!("started background job {job_id}"),
                value,
                &workdir,
            ));
        }

        if self.promote_on_timeout {
            match jobs.start(job_spec()) {
                Ok(sink) => {
                    let stopped = Arc::new(Mutex::new(None));
                    let process = self.start_job(
                        &sink,
                        &prepared,
                        &workdir,
                        &ctx.session_id,
                        stopped.clone(),
                    );
                    let id = sink.id().to_owned();
                    let owner = Some(ctx.session_id.as_str());
                    // A dropped call (the agent gave up on it) takes the command along.
                    let guard = OnDrop::new({
                        let (jobs, id, owner) = (jobs.clone(), id.clone(), ctx.session_id.clone());
                        move || {
                            let _ = jobs.kill(&id, Some(&owner), Some("tool call aborted"));
                        }
                    });
                    let waited = jobs.wait(&id, timeout_ms, owner, Some(&ctx.cancel)).await;
                    guard.disarm();
                    let view = match waited {
                        Ok(view) => view,
                        Err(_) => {
                            // The caller's abort ends the call, so the command goes with it.
                            let _ = jobs.kill(&id, owner, Some("tool call aborted"));
                            if let Ok(settled) = jobs.wait(&id, timeout_ms, owner, None).await
                                && settled.status.is_terminal()
                            {
                                let _ = jobs.remove(&id, owner);
                            }
                            return Err(ToolError::Cancelled);
                        }
                    };
                    if !view.status.is_terminal() {
                        let read = jobs.read(&id, owner).map_err(fail)?;
                        let mut output = split_streams(read.chunks.iter());
                        if read.lossy {
                            let separator = if !output.is_empty() && !output.ends_with('\n') {
                                "\n"
                            } else {
                                ""
                            };
                            output = format!(
                                "{output}{separator}{}",
                                dropped_notice(&read.job.spill_paths)
                            );
                        }
                        let text = render_promoted(&output, timeout_ms, &id);
                        let value = json!({
                            "kind": "promoted", "cwd": workdir, "jobId": id, "timeoutMs": timeout_ms, "output": output,
                        });
                        return Ok(output_with_meta(text, value, &workdir));
                    }
                    let _ = jobs.remove(&id, owner);
                    let outcome = process.wait().await;
                    let stopped = stopped.lock().unwrap().clone();
                    let result = foreground_result(
                        &process,
                        outcome,
                        &prepared,
                        (false, false, timeout_ms),
                        stopped,
                    )?;
                    return Ok(output_with_meta(
                        result.render(),
                        result.to_value(&workdir),
                        &workdir,
                    ));
                }
                Err(refusal) => {
                    tracing::warn!(
                        "bash: job registration refused, running in the foreground with the timeout kill instead: {refusal}"
                    );
                }
            }
        }

        // Deadline path: no job; the timeout kills the command.
        let process = Process::spawn(
            spawn_spec(prepared.argv.clone(), &workdir, &ctx.session_id, self.grace),
            None,
        );
        let guard = OnDrop::new({
            let process = process.clone();
            move || {
                process.kill();
            }
        });
        let sleep = tokio::time::sleep(Duration::from_secs_f64(timeout_ms / 1000.0));
        let (timed_out, aborted) = tokio::select! {
            _ = process.wait() => (false, false),
            _ = sleep => { process.kill(); (true, false) }
            _ = ctx.cancel.cancelled() => { process.kill(); (false, true) }
        };
        let outcome = process.wait().await;
        guard.disarm();
        if aborted {
            return Err(ToolError::Cancelled);
        }
        let result = foreground_result(
            &process,
            outcome,
            &prepared,
            (timed_out, aborted, timeout_ms),
            None,
        )?;
        Ok(output_with_meta(
            result.render(),
            result.to_value(&workdir),
            &workdir,
        ))
    }
}

impl BashTool {
    /// Spawns the job's process, wires its output into the ring, and settles
    /// the job when it ends.
    fn start_job(
        &self,
        sink: &crate::jobs::JobSink,
        prepared: &Arc<Prepared>,
        workdir: &str,
        session_id: &str,
        stopped: Arc<Mutex<Option<String>>>,
    ) -> Process {
        let ring = sink.clone();
        let hook: OutputHook = Arc::new(move |channel, text| {
            let channel = match channel {
                Channel::Stdout => JobChannel::Stdout,
                Channel::Stderr => JobChannel::Stderr,
            };
            ring.append(channel, text);
        });
        let process = Process::spawn(
            spawn_spec(prepared.argv.clone(), workdir, session_id, self.grace),
            Some(hook),
        );
        let spill_source = process.clone();
        sink.bind_spill_paths(move || spill_source.spill_paths());
        let killer = process.clone();
        sink.bind_cancel(move |reason| {
            *stopped.lock().unwrap() = reason.map(str::to_owned);
            killer.kill();
        });
        let settle_sink = sink.clone();
        let settle_process = process.clone();
        let prepared = prepared.clone();
        tokio::spawn(async move {
            let outcome = settle_process.wait().await;
            let stderr = match &outcome.spawn_error {
                Some(error) => {
                    format!("subprocess failed before reporting an outcome: Error: {error}")
                }
                None => settle_process.stderr().text,
            };
            if outcome.spawn_error.is_some() {
                settle_sink.append(JobChannel::Stderr, &stderr);
            }
            let info = settled_sandbox(&prepared, &outcome, &stderr);
            settle_sink.settle(job_outcome(&outcome, &info));
        });
        process
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(stdout: &str, stderr: &str, exit: Option<i32>, signal: Option<&str>) -> RunResult {
        RunResult {
            outcome: Outcome {
                exit_code: exit,
                signal: signal.map(String::from),
                killed: signal.is_some(),
                spawn_error: None,
            },
            timed_out: false,
            aborted: false,
            timeout_ms: 120_000.0,
            stdout: Collected {
                text: stdout.into(),
                ..Collected::default()
            },
            stderr: Collected {
                text: stderr.into(),
                ..Collected::default()
            },
            sandbox: SandboxInfo {
                mode: SandboxMode::DangerFullAccess,
                denied: false,
                enforcement: None,
                runner_failed: false,
            },
            stopped: None,
        }
    }

    #[test]
    fn render_matches_dsh() {
        assert_eq!(run("hi\n", "", Some(0), None).render(), "hi\n");
        assert_eq!(run("", "", Some(0), None).render(), "(no output)");
        assert_eq!(
            run("a", "b\n", Some(2), None).render(),
            "a\n[stderr]\nb\n[exit code: 2]"
        );
        assert_eq!(
            run("", "", None, Some("SIGTERM")).render(),
            "(no output)\n[killed by signal: SIGTERM]"
        );
        let mut r = run("x", "", None, None);
        r.timed_out = true;
        r.timeout_ms = 10.0;
        assert_eq!(r.render(), "x\n[timed out after 10ms]\n[exit code: null]");
        let mut r = run("", "touch: Read-only file system\n", Some(1), None);
        r.sandbox = SandboxInfo {
            mode: SandboxMode::ReadOnly,
            denied: true,
            enforcement: Some("full"),
            runner_failed: false,
        };
        assert_eq!(
            r.render(),
            format!(
                "[stderr]\ntouch: Read-only file system\n{}\n{}\n[exit code: 1]",
                denial_marker(SandboxMode::ReadOnly),
                escalation_hint("command")
            )
        );
        let mut r = run("tail", "", Some(0), None);
        r.stdout.truncated = true;
        r.stdout.spill_path = Some("/tmp/x.log".into());
        assert_eq!(
            r.render(),
            "tail\n[output truncated; full output: /tmp/x.log]"
        );
    }

    #[test]
    fn promoted_text() {
        assert_eq!(
            render_promoted("", 1.0, "bash-1"),
            "[still running after 1ms; moved to background job bash-1]\nThe command keeps running in the background. You will be notified when it finishes; read newer output with job_output, stop it with job_kill."
        );
        assert!(render_promoted("partial", 5.0, "bash-1").starts_with("partial\n[still running"));
    }

    #[test]
    fn workdir_resolution() {
        assert_eq!(resolve_workdir(None, Path::new("/w")), "/w");
        assert_eq!(resolve_workdir(Some("sub"), Path::new("/w")), "/w/sub");
        assert_eq!(resolve_workdir(Some("/abs"), Path::new("/w")), "/abs");
    }
}
