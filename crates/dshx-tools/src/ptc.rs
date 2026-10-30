//! Scoped programmatic tool calls: one TypeScript async body in a fresh Node
//! process. The process is not a VM security boundary. File access is confined
//! by the standing OS sandbox; nested calls always go through [`PtcBindings`].
use std::collections::HashSet;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{SandboxMode, ToolContext, ToolError, ToolOutput, ToolSchema};
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::sandbox::{self, Confined, Runner};

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;
const MAX_CODE_BYTES: usize = 1024 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_CALLS: usize = 128;
const MAX_CALLS: u64 = 4096;
const MAX_LOG_ENTRIES: usize = 4096;
const WORKER: &str = include_str!("ptc_worker.mjs");

/// The agent-owned execution boundary. `call` must enqueue before its first
/// pending await, honor cancellation, and settle only after its audit work.
/// The runtime initially polls calls in wire order, including Promise.all.
#[async_trait]
pub trait PtcBindings: Send + Sync {
    fn schemas(&self) -> Vec<ToolSchema>;
    async fn call(
        &self,
        name: String,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value, ToolError>;
}

/// The model-visible transport. Tool declarations are supplied by the agent
/// in its prompt; they are not exposed as additional native model tools.
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: "run_code".into(),
        description: "Execute a TypeScript program against the available tools. Takes two required arguments: `code`, the BODY of an async function (erasable syntax only; top-level `await` and `return` work), and `description`, a short summary of what the program does. Call tools as `await tools.name(args)` per the declarations in the system prompt. Only what you print or return is program output — curate it. Image-bearing subtool results are attached after the run. Each program runs in a fresh Node.js process in the Session's current directory. Use dynamic import() for Node modules. Tool failures reject with ToolCallError(toolName, message). A sandbox escalation approves this complete program for one execution only. Nested tools retain their own policies and approvals. Request wider access only after evidence of a denial. Earlier effects may already have completed: inspect them before explicitly retrying. Programs are never replayed automatically.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "The program: the body of an async TypeScript function." },
                "description": { "type": "string", "description": "Clear, concise description of what this program does in active voice, 5-10 words (shown in the UI)." },
                "timeoutMs": { "type": "number", "description": "Positive elapsed-time budget in milliseconds, including nested tool and approval waits. Default 120000; capped at 600000. Zero does not disable the deadline." },
                "sandbox_permissions": { "type": "string", "enum": ["workspace-write", "danger-full-access"], "description": "Wider sandbox mode for this complete program execution; requires justification and approval." },
                "justification": { "type": "string", "description": "Reason this complete program needs wider access, shown to the user for approval. Use the language of the user's current request." }
            },
            "required": ["code", "description"],
            "additionalProperties": false
        }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    code: String,
    description: String,
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<f64>,
    sandbox_permissions: Option<String>,
    justification: Option<String>,
}

#[derive(Debug)]
struct Failure {
    kind: &'static str,
    message: String,
}

impl Failure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new("protocol", message)
    }
}

#[derive(Default)]
struct Output {
    logs: Vec<String>,
    bytes: usize,
}

impl Output {
    fn admit(&mut self, text: String) -> Result<(), Failure> {
        let size = serde_json::to_vec(&text).expect("JSON string").len() + 1;
        if self.logs.len() >= MAX_LOG_ENTRIES || self.bytes + size > MAX_OUTPUT_BYTES {
            return Err(Self::limit());
        }
        self.bytes += size;
        self.logs.push(text);
        Ok(())
    }

    fn limit() -> Failure {
        Failure::new(
            "output-limit",
            format!("outer output exceeded {MAX_OUTPUT_BYTES} bytes or {MAX_LOG_ENTRIES} entries"),
        )
    }
}

/// Raw file-descriptor writes may split a UTF-8 character across OS reads.
struct NativeText(encoding_rs::Decoder);

impl NativeText {
    fn new() -> Self {
        Self(encoding_rs::UTF_8.new_decoder_without_bom_handling())
    }

    fn decode(&mut self, bytes: &[u8], eof: bool) -> String {
        let mut text = String::with_capacity(bytes.len() * 3 + 4);
        let (_, read, _) = self.0.decode_to_string(bytes, &mut text, eof);
        debug_assert_eq!(read, bytes.len());
        text
    }
}

/// Keep total indentation bounded, as in the upstream PTC renderer. Deep
/// values must not expand a bounded result into a huge pretty-printed string.
fn render_value(value: &Value, depth: usize, text: &mut String) {
    let (open, close, children): (char, char, Vec<(Option<&str>, &Value)>) = match value {
        Value::Array(values) if depth < 5 && !values.is_empty() => {
            ('[', ']', values.iter().map(|v| (None, v)).collect())
        }
        Value::Object(values) if depth < 5 && !values.is_empty() => (
            '{',
            '}',
            values.iter().map(|(k, v)| (Some(k.as_str()), v)).collect(),
        ),
        _ => {
            text.push_str(&serde_json::to_string(value).expect("JSON value"));
            return;
        }
    };
    text.push(open);
    for (index, (key, child)) in children.into_iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push('\n');
        text.push_str(&"  ".repeat(depth + 1));
        if let Some(key) = key {
            text.push_str(&serde_json::to_string(key).expect("JSON key"));
            text.push_str(": ");
        }
        render_value(child, depth + 1, text);
    }
    text.push('\n');
    text.push_str(&"  ".repeat(depth));
    text.push(close);
}

/// Locate Node before clearing the environment. Explicit overrides fail rather
/// than silently falling back; app bundles put `node` beside the Rust binary.
fn node_executable() -> Result<PathBuf, ToolError> {
    let executable = |path: &Path| {
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let resolve = |name: &Path| {
        if name.components().count() > 1 || name.is_absolute() {
            executable(name).then(|| sandbox::canonical_path(name))
        } else {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|p| p.join(name))
                    .find(|p| executable(p))
                    .map(|p| sandbox::canonical_path(&p))
            })
        }
    };
    if let Some(override_path) = std::env::var_os("DSHX_NODE") {
        return resolve(Path::new(&override_path)).ok_or_else(|| {
            ToolError::Failed("DSHX_NODE must name an executable Node.js 22.18+ binary".into())
        });
    }
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("node")))
        .filter(|p| executable(p))
    {
        return Ok(bundled);
    }
    resolve(Path::new("node")).ok_or_else(|| {
        ToolError::Failed(
            "run_code requires Node.js 22.18 or newer; install Node.js or set DSHX_NODE".into(),
        )
    })
}

/// Kills the entire process group even if the caller drops run_code. Until
/// cleanup we never reap the leader, so its group id cannot be reused.
struct ProcessGuard {
    child: Child,
    pid: Option<i32>,
    cancel: CancellationToken,
}

impl ProcessGuard {
    fn kill(&mut self) {
        self.cancel.cancel();
        if let Some(pid) = self.pid {
            crate::process::signal_group(pid, libc::SIGKILL);
            let _ = self.child.start_kill();
        }
    }

    async fn stop(&mut self) -> io::Result<std::process::ExitStatus> {
        self.kill();
        let status = self.child.wait().await;
        self.pid = None;
        status
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Reader state lives outside the selected future: partial frames survive
/// another select branch winning, unlike a cancellable read_exact future.
struct Frames {
    reader: ReadHalf<UnixStream>,
    header: [u8; 4],
    header_read: usize,
    body: Vec<u8>,
    body_read: usize,
}

impl Frames {
    async fn next(&mut self) -> Result<(Value, usize), Failure> {
        while self.header_read < 4 {
            let n = self
                .reader
                .read(&mut self.header[self.header_read..])
                .await
                .map_err(|e| Failure::protocol(e.to_string()))?;
            if n == 0 {
                return Err(Failure::new(
                    "worker-exit",
                    "Node process closed its control channel before completing",
                ));
            }
            self.header_read += n;
        }
        if self.body.is_empty() {
            let size = u32::from_be_bytes(self.header) as usize;
            if size == 0 || size > MAX_FRAME_BYTES {
                return Err(Failure::protocol("control frame exceeds its byte limit"));
            }
            self.body.resize(size, 0);
        }
        while self.body_read < self.body.len() {
            let n = self
                .reader
                .read(&mut self.body[self.body_read..])
                .await
                .map_err(|e| Failure::protocol(e.to_string()))?;
            if n == 0 {
                return Err(Failure::protocol("incomplete control frame"));
            }
            self.body_read += n;
        }
        let bytes = self.body.len();
        let parsed = serde_json::from_slice(&self.body)
            .map_err(|_| Failure::protocol("invalid control JSON"));
        self.header_read = 0;
        self.body_read = 0;
        self.body.clear();
        parsed.map(|value| (value, bytes))
    }
}

async fn write_frame(writer: &mut WriteHalf<UnixStream>, value: &Value) -> Result<(), Failure> {
    let bytes = serde_json::to_vec(value).map_err(|e| Failure::protocol(e.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Failure::protocol("control reply exceeds its byte limit"));
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|e| Failure::protocol(e.to_string()))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| Failure::protocol(e.to_string()))
}

async fn send(
    writer: &mut WriteHalf<UnixStream>,
    value: &Value,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Failure::new("abort", "tool call aborted")),
        _ = tokio::time::sleep_until(deadline) => Err(Failure::new("timeout", "execution deadline reached")),
        result = write_frame(writer, value) => result,
    }
}

async fn raw_output(
    reader: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8],
) -> Result<usize, Failure> {
    reader
        .read(buffer)
        .await
        .map_err(|e| Failure::new("worker-exit", e.to_string()))
}

type PendingCall = BoxFuture<'static, (u64, usize, Result<Value, ToolError>)>;

/// Execute a program without exposing any direct Tool::execute shortcut.
pub async fn run_code(
    args: Value,
    ctx: ToolContext,
    bindings: Arc<dyn PtcBindings>,
) -> Result<ToolOutput, ToolError> {
    run_with_runner(args, ctx, bindings, None).await
}

async fn run_with_runner(
    args: Value,
    ctx: ToolContext,
    bindings: Arc<dyn PtcBindings>,
    runner: Option<Runner>,
) -> Result<ToolOutput, ToolError> {
    let args: Args =
        serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
    if args.code.len() > MAX_CODE_BYTES {
        return Err(ToolError::InvalidArguments(format!(
            "code exceeds {MAX_CODE_BYTES} bytes"
        )));
    }
    let timeout = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS as f64);
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ToolError::InvalidArguments(
            "timeoutMs must be a positive finite number".into(),
        ));
    }
    let deadline =
        Instant::now() + Duration::from_secs_f64(timeout.min(MAX_TIMEOUT_MS as f64) / 1000.0);
    sandbox::validate_escalation_args(
        args.sandbox_permissions.as_deref(),
        args.justification.as_deref(),
    )?;
    let mode = if let Some(requested) = args.sandbox_permissions.as_deref() {
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            _ = tokio::time::sleep_until(deadline) => return Err(run_failure(Failure::new("timeout", "execution deadline reached during approval"), &Output::default(), ctx.sandbox, None)),
            result = sandbox::approve_escalation(&ctx, "run_code", "program", requested, args.justification.as_deref().unwrap_or_default(), ctx.sandbox, &args.description) => result?,
        }
    } else {
        ctx.sandbox
    };
    let runner = if mode == SandboxMode::DangerFullAccess {
        Runner::Unconfined
    } else {
        match runner {
            Some(runner) => runner,
            None => tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err(run_failure(Failure::new("timeout", "execution deadline reached while checking sandbox"), &Output::default(), mode, None)),
                runner = tokio::task::spawn_blocking(sandbox::host_runner) => runner.unwrap_or(Runner::Unconfined),
            },
        }
    };
    if mode != SandboxMode::DangerFullAccess && runner == Runner::Unconfined {
        return Err(run_failure(
            Failure::new(
                "sandbox-unavailable",
                sandbox::unavailable_message(mode, None),
            ),
            &Output::default(),
            mode,
            None,
        ));
    }
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let node = node_executable()?;
    let confined = sandbox::confine_argv(
        &runner,
        vec![
            node.to_string_lossy().into_owned(),
            "--no-warnings".into(),
            "--max-old-space-size=256".into(),
            "--input-type=module".into(),
            "--eval".into(),
            WORKER.into(),
        ],
        mode,
        &ctx.cwd,
    );
    let (control, inherited) =
        StdUnixStream::pair().map_err(|e| ToolError::Failed(e.to_string()))?;
    control
        .set_nonblocking(true)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let fd = inherited.as_raw_fd();
    let mut command = Command::new(&confined.argv[0]);
    command
        .args(&confined.argv[1..])
        .current_dir(&ctx.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    // SAFETY: only async-signal-safe descriptor operations run after fork. The
    // endpoint stays alive until spawn returns; all other endpoints are CLOEXEC.
    unsafe {
        command.pre_exec(move || {
            if fd != 3 && libc::dup2(fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| {
        run_failure(
            Failure::new(
                if confined.enforcement.is_some() {
                    "sandbox-unavailable"
                } else {
                    "worker-exit"
                },
                format!("could not start Node.js: {e}"),
            ),
            &Output::default(),
            mode,
            Some(&confined),
        )
    })?;
    drop(inherited);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let evaluation_cancel = ctx.cancel.child_token();
    let mut process = ProcessGuard {
        pid: child.id().map(|id| id as i32),
        child,
        cancel: evaluation_cancel.clone(),
    };
    let stream = UnixStream::from_std(control).map_err(|e| ToolError::Failed(e.to_string()))?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut frames = Frames {
        reader,
        header: [0; 4],
        header_read: 0,
        body: Vec::new(),
        body_read: 0,
    };
    let names: HashSet<String> = bindings
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .filter(|name| name != "run_code")
        .collect();
    let mut ordered_names: Vec<_> = names.iter().cloned().collect();
    ordered_names.sort();
    let boot = json!({"type": "boot", "code": args.code, "names": ordered_names});
    let mut calls: FuturesUnordered<PendingCall> = FuturesUnordered::new();
    let mut pending_bytes = 0usize;
    let mut next_id = 1u64;
    let mut ready = false;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_buffer = [0u8; 8192];
    let mut stderr_buffer = [0u8; 8192];
    let mut stdout_text = NativeText::new();
    let mut stderr_text = NativeText::new();
    let mut output = Output::default();
    let mut failure_text = String::new();

    let mut result: Result<Option<Value>, Failure> = async {
        loop {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(Failure::new("abort", "tool call aborted")),
                _ = tokio::time::sleep_until(deadline) => return Err(Failure::new("timeout", "execution deadline reached")),
                bytes = raw_output(&mut stdout, &mut stdout_buffer), if stdout_open => {
                    let size = bytes?;
                    if size == 0 { stdout_open = false; }
                    let text = stdout_text.decode(&stdout_buffer[..size], size == 0);
                    if !text.is_empty() { output.admit(text)?; }
                },
                bytes = raw_output(&mut stderr, &mut stderr_buffer), if stderr_open => {
                    let size = bytes?;
                    if size == 0 { stderr_open = false; }
                    let text = stderr_text.decode(&stderr_buffer[..size], size == 0);
                    if !text.is_empty() {
                        failure_text.push_str(&text);
                        output.admit(text)?;
                    }
                },
                completion = calls.next(), if !calls.is_empty() => {
                    let (id, bytes, value) = completion.expect("nonempty calls");
                    pending_bytes -= bytes;
                    let reply = match value {
                        Ok(value) => json!({"id": id, "ok": true, "value": value}),
                        Err(error) => json!({"id": id, "ok": false, "message": error.to_string()}),
                    };
                    send(&mut writer, &reply, deadline, &ctx.cancel).await?;
                },
                frame = frames.next() => {
                    let (frame, bytes) = frame?;
                    let kind = frame.get("type").and_then(Value::as_str).ok_or_else(|| Failure::protocol("missing control frame type"))?;
                    if !ready {
                        if kind != "ready" { return Err(Failure::protocol("program frame arrived before readiness")); }
                        ready = true;
                        send(&mut writer, &boot, deadline, &ctx.cancel).await?;
                        continue;
                    }
                    match kind {
                        "log" => output.admit(frame.get("text").and_then(Value::as_str).ok_or_else(|| Failure::protocol("invalid log frame"))?.to_owned())?,
                        "done" => {
                            if let Some(error) = frame.get("error") {
                                let kind = match error.get("kind").and_then(Value::as_str) {
                                    Some("exception") => "exception", Some("invalid-output") => "invalid-output", Some("output-limit") => "output-limit", Some("protocol") => "protocol",
                                    _ => return Err(Failure::protocol("invalid terminal error")),
                                };
                                let message = error.get("message").and_then(Value::as_str).ok_or_else(|| Failure::protocol("invalid terminal error message"))?;
                                if output.bytes + serde_json::to_vec(message).expect("JSON string").len() > MAX_OUTPUT_BYTES { return Err(Output::limit()); }
                                return Err(Failure::new(kind, message));
                            }
                            let value = frame.get("value").cloned();
                            if value.as_ref().is_some_and(|v| output.bytes + serde_json::to_vec(v).expect("JSON value").len() > MAX_OUTPUT_BYTES) { return Err(Output::limit()); }
                            return Ok(value);
                        },
                        "call" => {
                            let id = frame.get("id").and_then(Value::as_u64).filter(|id| *id == next_id).ok_or_else(|| Failure::protocol("invalid binding call identity"))?;
                            let name = frame.get("name").and_then(Value::as_str).filter(|name| names.contains(*name)).ok_or_else(|| Failure::protocol("program requested an undeclared binding"))?.to_owned();
                            let args = frame.get("args").cloned().ok_or_else(|| Failure::protocol("binding arguments must be JSON"))?;
                            if id > MAX_CALLS || calls.len() >= MAX_PENDING_CALLS || pending_bytes + bytes > MAX_FRAME_BYTES { return Err(Failure::protocol("pending binding calls exceed configured limits")); }
                            next_id += 1;
                            pending_bytes += bytes;
                            let bindings = bindings.clone();
                            let cancel = evaluation_cancel.clone();
                            let mut call = async move { (id, bytes, bindings.call(name, args, cancel).await) }.boxed();
                            // Enqueue synchronously in source order before allowing any
                            // other call to be polled. A completed call stays ordered too.
                            if let std::task::Poll::Ready(completed) = futures::poll!(&mut call) { calls.push(futures::future::ready(completed).boxed()); }
                            else { calls.push(call); }
                        },
                        _ => return Err(Failure::protocol("unknown control frame")),
                    }
                },
            }
        }
    }.await;

    // Terminal, abort, deadline and protocol failure all share one cleanup
    // path. The leader remains unreaped until its complete group is killed.
    evaluation_cancel.cancel();
    let status = process.stop().await;
    drop(writer);
    drop(frames);
    while calls.next().await.is_some() {}
    // Native fd writes can race the terminal RPC. Drain the now-dead process's
    // bounded pipes so these bytes are not silently discarded on success.
    let drained = tokio::time::timeout(Duration::from_secs(1), async {
        while stdout_open || stderr_open {
            tokio::select! {
                bytes = raw_output(&mut stdout, &mut stdout_buffer), if stdout_open => {
                    let size = bytes?;
                    if size == 0 { stdout_open = false; }
                    let text = stdout_text.decode(&stdout_buffer[..size], size == 0);
                    if !text.is_empty() { output.admit(text)?; }
                },
                bytes = raw_output(&mut stderr, &mut stderr_buffer), if stderr_open => {
                    let size = bytes?;
                    if size == 0 { stderr_open = false; }
                    let text = stderr_text.decode(&stderr_buffer[..size], size == 0);
                    if !text.is_empty() {
                        failure_text.push_str(&text);
                        output.admit(text)?;
                    }
                },
            }
        }
        Ok::<(), Failure>(())
    })
    .await;
    match drained {
        Ok(Err(error)) => result = Err(error),
        Err(_) if result.is_ok() => {
            result = Err(Failure::new(
                "worker-exit",
                "Node process output did not close cleanly",
            ))
        }
        _ => {}
    }
    if let Err(error) = status {
        result = Err(Failure::new(
            "worker-exit",
            format!("Node process cleanup failed: {error}"),
        ));
    }
    if let Err(error) = &mut result
        && error.kind == "worker-exit"
        && confined.enforcement.is_some()
        && sandbox::runner_failure(Some(1), &failure_text, &confined.failure_signatures).is_some()
    {
        error.kind = "sandbox-unavailable";
    }
    match result {
        Err(error) => Err(run_failure(error, &output, mode, Some(&confined))),
        Ok(value) => {
            if value.as_ref().is_some_and(|v| {
                output.bytes + serde_json::to_vec(v).expect("JSON value").len() > MAX_OUTPUT_BYTES
            }) {
                return Err(run_failure(Output::limit(), &output, mode, Some(&confined)));
            }
            let mut text = output.logs.join("\n");
            if let Some(value) = &value {
                if !text.is_empty() {
                    text.push('\n');
                }
                if let Some(value) = value.as_str() {
                    text.push_str(value);
                } else {
                    render_value(value, 0, &mut text);
                }
            }
            let mut result = json!({"logs": output.logs, "sandbox": {"mode": sandbox::mode_name(mode), "denied": false}});
            if let Some(enforcement) = confined.enforcement {
                result["sandbox"]["enforcement"] = json!(enforcement);
            }
            if let Some(value) = value {
                result["result"] = value;
            }
            Ok(ToolOutput {
                value: Some(result),
                ..ToolOutput::text(text)
            })
        }
    }
}

fn run_failure(
    mut error: Failure,
    output: &Output,
    mode: SandboxMode,
    confined: Option<&Confined>,
) -> ToolError {
    if output.bytes
        + serde_json::to_vec(&error.message)
            .expect("JSON string")
            .len()
        > MAX_OUTPUT_BYTES
    {
        error = Output::limit();
    }
    let mut message = format!("code run failed ({}): {}", error.kind, error.message);
    if !output.logs.is_empty() {
        message.push_str(&format!("\nCaptured output:\n{}", output.logs.join("\n")));
    }
    let denied = confined.is_some_and(|c| {
        c.denial_signatures
            .iter()
            .any(|signature| message.to_lowercase().contains(&signature.to_lowercase()))
    });
    message.push_str(&format!("\nFile sandbox: {}", sandbox::mode_name(mode)));
    if let Some(enforcement) = confined.and_then(|c| c.enforcement) {
        message.push_str(&format!("; enforcement: {enforcement}"));
    }
    if denied {
        message.push_str("; operation denied");
    }
    message.push('.');
    if denied {
        message.push_str(&format!(
            "\n{} Earlier effects may already have completed; inspect them before retrying.",
            sandbox::escalation_hint("program")
        ));
    }
    ToolError::coded("CodeRunFailedError", "CODE_RUN_FAILED", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_output_decodes_across_chunks_and_rendering_caps_indentation() {
        let mut decoder = NativeText::new();
        assert_eq!(decoder.decode(&[0xf0, 0x9f], false), "");
        assert_eq!(decoder.decode(&[0x98, 0x80], false), "😀");
        let value = json!([[[[[[[[[["deep"]]]]]]]]]]);
        let mut rendered = String::new();
        render_value(&value, 0, &mut rendered);
        assert_eq!(serde_json::from_str::<Value>(&rendered).unwrap(), value);
        assert!(
            rendered
                .lines()
                .all(|line| line.len() - line.trim_start().len() <= 10)
        );
    }

    struct NoBindings;
    #[async_trait]
    impl PtcBindings for NoBindings {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![]
        }
        async fn call(
            &self,
            _: String,
            _: Value,
            _: CancellationToken,
        ) -> Result<Value, ToolError> {
            panic!("unexpected binding")
        }
    }

    #[tokio::test]
    async fn restricted_run_fails_closed_without_runner() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = crate::testing::builder(temp.path())
            .sandbox(SandboxMode::ReadOnly)
            .build();
        let error = run_with_runner(
            json!({"code": "return 1", "description": "Read one value"}),
            ctx,
            Arc::new(NoBindings),
            Some(Runner::Unconfined),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("sandbox-unavailable"));
        assert!(error.to_string().contains("refusing to run"));
    }
}
