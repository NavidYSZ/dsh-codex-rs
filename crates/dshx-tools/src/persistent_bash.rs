//! The persistent `bash` tool of dsh's Minimal agent preset
//! (`dsh-tool-bash-persistent` over `dsh-terminal-bash`): one long-lived
//! shell per session, so the current directory, exported variables and
//! functions survive from one call to the next.
//!
//! dsh drives an interactive bash on a PTY. Here the shell reads its
//! commands from a pipe, folds stderr into stdout and gives every command
//! `/dev/null` as stdin, so a command waiting for input ends instead of
//! swallowing the next command. Each command is wrapped between unique start
//! and end markers that carry its exit status (dsh `wrapCommand`); the result
//! is the text between them, rendered like dsh. Anything uncertain (the shell
//! exited, a timeout, a cancelled call) resets the shell, and the next call
//! starts from the workspace again.
//!
//! The shell runs under the session's sandbox mode for its whole life, like
//! dsh's terminal backend. When the mode changes between calls the shell is
//! restarted under the new one. The schema has no escalation arguments, so
//! there is nothing to approve.
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{SandboxMode, Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::args::fail;
use crate::env::child_env;
use crate::process::{signal_group, signal_name, spawn_error_text};
use crate::sandbox::{self, Confined, Runner, mode_name};
use crate::state::ToolState;

/// The description dsh's Minimal preset configures for its shell
/// (`dsh-web-app/presets/minimal.patch.yml`, also `dsh-sdk-minimal`).
pub const DESCRIPTION: &str = "Run commands in a bash shell\n* When invoking this tool, the contents of the \"command\" parameter does NOT need to be XML-escaped.\n* Network access depends on the task environment. Prefer configured mirrors/proxies when they are available.\n* State is persistent across command calls and discussions with the user.\n* To inspect a particular line range of a file, e.g. lines 10-25, try 'sed -n 10,25p /path/to/the/file'.\n* Please avoid commands that may produce a very large amount of output.\n* Please run long lived commands in the background, e.g. 'sleep 10 &' or start a server in the background.";
const COMMAND_DESCRIPTION: &str =
    "The bash command to run. Relative path is preferred in the command.";

/// Wall-clock limit of one command (the preset's `timeoutMs: 300000`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// Retained output characters (UTF-16 units, dsh `maxOutputChars`).
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 16_000;

const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";
/// Told to the model whenever its shell went away.
pub const SHELL_RESET_MESSAGE: &str = "The persistent bash shell was reset; the next bash call starts from the workspace with a fresh current directory and environment.";
const TIMEOUT_STATUS_MARKER: &str = "[Command timed out or OOM]";

/// Output kept per command (dsh's terminal scrollback holds 4 MiB).
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
/// Bytes kept while looking for a marker in output we do not keep.
const SCAN_WINDOW: usize = 64 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGTERM → SIGKILL grace for the shell's process group.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// After the shell exited, how long its pipes may still deliver output.
const EXIT_DRAIN: Duration = Duration::from_millis(100);

/// The model-facing schema, byte-identical to dsh's (`defineTool` output).
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: "bash".into(),
        description: DESCRIPTION.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": COMMAND_DESCRIPTION}
            },
            "required": ["command"]
        }),
    }
}

/// The persistent `bash` tool.
pub struct PersistentBashTool {
    runner: Option<Runner>,
    timeout: Duration,
    max_output_chars: usize,
}

impl Default for PersistentBashTool {
    fn default() -> Self {
        PersistentBashTool::new()
    }
}

impl PersistentBashTool {
    /// The Minimal preset's configuration; confines with the host runner.
    pub fn new() -> Self {
        PersistentBashTool {
            runner: None,
            timeout: DEFAULT_TIMEOUT,
            max_output_chars: DEFAULT_MAX_OUTPUT_CHARS,
        }
    }

    /// Uses `runner` instead of the probed host runner.
    pub fn with_runner(runner: Runner) -> Self {
        PersistentBashTool {
            runner: Some(runner),
            ..PersistentBashTool::new()
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_output_chars(mut self, chars: usize) -> Self {
        self.max_output_chars = chars.max(1);
        self
    }

    async fn runner(&self, mode: SandboxMode) -> Runner {
        match &self.runner {
            _ if mode == SandboxMode::DangerFullAccess => Runner::Unconfined,
            Some(runner) => runner.clone(),
            None => tokio::task::spawn_blocking(sandbox::host_runner)
                .await
                .unwrap_or(Runner::Unconfined),
        }
    }
}

#[async_trait]
impl Tool for PersistentBashTool {
    fn schema(&self) -> ToolSchema {
        schema()
    }

    fn describe(&self, args: &Value) -> String {
        args["command"].as_str().unwrap_or("bash").to_owned()
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let violations = crate::args::validate(&schema().parameters, &args, "");
        if !violations.is_empty() {
            return Err(ToolError::InvalidArguments(format!(
                "invalid arguments: {}",
                violations.join("; ")
            )));
        }
        let command = args["command"].as_str().unwrap_or_default();
        if command.trim().is_empty() {
            return Err(fail("command must be a non-empty string"));
        }
        let state = ToolState::get(&ctx)?;
        let slot = state.shells().slot(&ctx.session_id);
        // One command at a time per session (dsh's per-owner queue).
        let mut guard = slot.shell.lock().await;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let deadline = Instant::now() + self.timeout;
        let mut note = None;
        if let Some(shell) = guard.as_ref()
            && (shell.mode != ctx.sandbox || shell.exited())
        {
            if shell.mode != ctx.sandbox && !shell.exited() {
                note = Some(format!(
                    "[The persistent bash shell was restarted for the {} sandbox mode; this command ran in a fresh shell from the workspace.]",
                    mode_name(ctx.sandbox)
                ));
            }
            slot.forget(guard.take());
        }
        if guard.is_none() {
            let runner = self.runner(ctx.sandbox).await;
            let shell = Shell::spawn(&runner, ctx.sandbox, &ctx, deadline).await?;
            *slot.pid.lock().unwrap() = shell.pid;
            *guard = Some(shell);
        }
        let shell = guard.as_mut().expect("shell present");
        let ending = shell.run(command, deadline, &ctx.cancel).await;
        let max = self.max_output_chars;
        let text = match ending {
            Ok(Ending::Finished { output, status }) => append_status_marker(
                &maybe_truncate(&output, max),
                Some(&format!("[Command finished with exit code {status}]")),
            ),
            Ok(Ending::Exited { output, exit }) => {
                slot.forget(guard.take());
                let rendered =
                    append_status_marker(&maybe_truncate(&output, max), Some(&exit.marker()));
                [rendered, SHELL_RESET_MESSAGE.to_owned()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Ok(Ending::TimedOut { output }) => {
                slot.forget(guard.take());
                [
                    format!(
                        "Your command timed out after {} seconds or experienced an OOM error. Below is partial output:",
                        (self.timeout.as_millis() as f64 / 1000.0).round()
                    ),
                    append_status_marker(
                        &maybe_truncate(&output, max),
                        Some(TIMEOUT_STATUS_MARKER),
                    ),
                    SHELL_RESET_MESSAGE.to_owned(),
                ]
                .join("\n")
            }
            Ok(Ending::Cancelled) => {
                slot.forget(guard.take());
                return Err(ToolError::Cancelled);
            }
            Err(e) => {
                slot.forget(guard.take());
                return Err(e);
            }
        };
        let text = match note {
            Some(note) => format!("{note}\n{text}"),
            None => text,
        };
        Ok(ToolOutput {
            value: Some(Value::String(text.clone())),
            ..ToolOutput::text(text)
        })
    }
}

/// The persistent shells of one session's tools, by owner (session id).
#[derive(Default)]
pub struct Shells {
    slots: Mutex<HashMap<String, Arc<Slot>>>,
}

pub(crate) struct Slot {
    shell: tokio::sync::Mutex<Option<Shell>>,
    /// The live shell's process group, readable while a call holds `shell`.
    pid: Mutex<Option<i32>>,
}

impl Slot {
    /// Drops a shell that is no longer usable (which kills its group).
    fn forget(&self, shell: Option<Shell>) {
        *self.pid.lock().unwrap() = None;
        drop(shell);
    }
}

impl Shells {
    fn slot(&self, owner: &str) -> Arc<Slot> {
        self.slots
            .lock()
            .unwrap()
            .entry(owner.to_owned())
            .or_insert_with(|| {
                Arc::new(Slot {
                    shell: tokio::sync::Mutex::new(None),
                    pid: Mutex::new(None),
                })
            })
            .clone()
    }

    /// Whether `owner` has a live shell.
    pub fn is_open(&self, owner: &str) -> bool {
        let slot = self.slots.lock().unwrap().get(owner).cloned();
        slot.is_some_and(|s| s.pid.lock().unwrap().is_some())
    }

    /// Kills the shells of `owner` (all with `None`), e.g. when the agent is
    /// disposed. A call that is still running sees its shell exit.
    pub fn close(&self, owner: Option<&str>) {
        let slots: Vec<Arc<Slot>> = {
            let mut map = self.slots.lock().unwrap();
            match owner {
                Some(owner) => map.remove(owner).into_iter().collect(),
                None => map.drain().map(|(_, s)| s).collect(),
            }
        };
        for slot in slots {
            if let Some(pid) = slot.pid.lock().unwrap().take() {
                terminate_group(pid);
            }
            if let Ok(mut shell) = slot.shell.try_lock() {
                shell.take();
            }
        }
    }
}

/// SIGTERM to the whole group now, SIGKILL after the grace if it survived.
fn terminate_group(pid: i32) {
    signal_group(pid, libc::SIGTERM);
    let escalate = move || {
        // Signal 0 probes whether any member of the group is still alive.
        if signal_group(pid, 0) {
            signal_group(pid, libc::SIGKILL);
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::time::sleep(KILL_GRACE).await;
                escalate();
            });
        }
        Err(_) => {
            std::thread::spawn(move || {
                std::thread::sleep(KILL_GRACE);
                escalate();
            });
        }
    }
}

/// How the shell process ended.
#[derive(Debug, Clone)]
struct ShellExit {
    code: Option<i32>,
    signal: Option<String>,
}

impl ShellExit {
    /// dsh `renderShellExitStatus`.
    fn marker(&self) -> String {
        match (&self.signal, self.code) {
            (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
            (None, Some(code)) => format!("[shell exited: code {code}]"),
            (None, None) => "[shell exited]".to_owned(),
        }
    }
}

/// How one command ended; `output` is the command's text without markers.
enum Ending {
    Finished { output: String, status: i32 },
    Exited { output: String, exit: ShellExit },
    TimedOut { output: String },
    Cancelled,
}

/// One running `bash` reading commands from its stdin.
struct Shell {
    pid: Option<i32>,
    stdin: tokio::process::ChildStdin,
    output: mpsc::UnboundedReceiver<Vec<u8>>,
    exit: watch::Receiver<Option<ShellExit>>,
    mode: SandboxMode,
}

impl Drop for Shell {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            terminate_group(pid);
        }
    }
}

fn shell_env(session_id: &str) -> Vec<(OsString, OsString)> {
    let mut env = child_env(session_id);
    // A non-interactive bash would source these; keep the start clean (--norc).
    env.retain(|(k, _)| k != "BASH_ENV" && k != "ENV");
    env.push(("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()));
    env
}

fn nonce() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// dsh `quoteForBash`: one physical line, so embedded newlines stay inside.
fn quote_for_bash(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    format!("$'{escaped}'")
}

/// Pipes a reader's chunks into the shared output channel.
fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    tx: mpsc::UnboundedSender<Vec<u8>>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

impl Shell {
    /// Starts `bash` in the workspace under `mode` and waits until it reads.
    async fn spawn(
        runner: &Runner,
        mode: SandboxMode,
        ctx: &ToolContext,
        deadline: Instant,
    ) -> Result<Shell, ToolError> {
        let inner = vec!["bash".to_owned(), "--noprofile".into(), "--norc".into()];
        let confined: Confined = sandbox::confine_argv(runner, inner, mode, &ctx.cwd);
        let program = confined.argv[0].clone();
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&confined.argv[1..])
            .current_dir(&ctx.cwd)
            .env_clear()
            .envs(shell_env(&ctx.session_id))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(false);
        let mut child = command
            .spawn()
            .map_err(|e| fail(spawn_error_text(&program, &e)))?;
        let pid = child.id().map(|id| id as i32);
        let (tx, output) = mpsc::unbounded_channel();
        if let Some(stdout) = child.stdout.take() {
            pump(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            pump(stderr, tx);
        }
        let stdin = child.stdin.take().expect("piped stdin");
        let (exit_tx, exit) = watch::channel(None);
        tokio::spawn(async move {
            let status = child.wait().await;
            let exit = match status {
                Ok(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    ShellExit {
                        code: status.code(),
                        signal: status.signal().map(signal_name),
                    }
                }
                Err(_) => ShellExit {
                    code: None,
                    signal: None,
                },
            };
            exit_tx.send_replace(Some(exit));
        });
        let mut shell = Shell {
            pid,
            stdin,
            output,
            exit,
            mode,
        };
        let ready = format!("__DSH_PERSISTENT_BASH_READY_{}__", nonce());
        let init = format!("exec 2>&1\nprintf '%s\\n' {}\n", quote_for_bash(&ready));
        let startup = deadline.min(Instant::now() + STARTUP_TIMEOUT);
        let mut seen = Vec::new();
        let written = shell.stdin.write_all(init.as_bytes()).await;
        loop {
            if written.is_ok() && find(&seen, ready.as_bytes(), 0).is_some() {
                return Ok(shell);
            }
            let exited = shell.exit.borrow().is_some();
            if exited || written.is_err() {
                // Whatever the runner printed before giving up.
                shell.drain(&mut seen).await;
                let text = String::from_utf8_lossy(&seen).into_owned();
                let code = shell.exit.borrow().as_ref().and_then(|e| e.code);
                if let Some(line) =
                    sandbox::runner_failure(code.or(Some(1)), &text, &confined.failure_signatures)
                {
                    return Err(fail(sandbox::unavailable_message(mode, Some(&line))));
                }
                return Err(fail("persistent bash shell did not accept initialization"));
            }
            tokio::select! {
                chunk = shell.output.recv() => match chunk {
                    Some(chunk) => {
                        seen.extend_from_slice(&chunk);
                        keep_tail(&mut seen, SCAN_WINDOW);
                    }
                    None => { let _ = shell.exit.changed().await; }
                },
                _ = shell.exit.changed() => {}
                _ = tokio::time::sleep_until(startup) => {
                    return Err(fail("persistent bash shell did not accept initialization"));
                }
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }
    }

    fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Collects what the pipes still deliver shortly after the exit.
    async fn drain(&mut self, into: &mut Vec<u8>) {
        let until = Instant::now() + EXIT_DRAIN;
        while let Ok(Some(chunk)) = tokio::time::timeout_at(until, self.output.recv()).await {
            into.extend_from_slice(&chunk);
        }
    }

    /// Runs one wrapped command until its end marker, the shell's exit, the
    /// deadline or a cancellation.
    async fn run(
        &mut self,
        command: &str,
        deadline: Instant,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Ending, ToolError> {
        // Output of background jobs between calls belongs to no command.
        while self.output.try_recv().is_ok() {}
        let id = nonce();
        let start = format!("__DSH_PERSISTENT_BASH_START_{id}__");
        let end = format!("__DSH_PERSISTENT_BASH_END_{id}:");
        let wrapped = format!(
            "printf '%s\\n' {}; eval -- {} </dev/null; __dsh_persistent_bash_status=$?; printf '%s%s\\n' {} \"$__dsh_persistent_bash_status\"\n",
            quote_for_bash(&start),
            quote_for_bash(command),
            quote_for_bash(&end),
        );
        let mut capture = Capture::new(start, end);
        if let Err(e) = self.stdin.write_all(wrapped.as_bytes()).await {
            if self.exited() {
                return Ok(self.exited_with(capture).await);
            }
            return Err(fail(format!("persistent bash send failed: {e}")));
        }
        let mut pipes_open = true;
        loop {
            if let Some(status) = capture.status {
                return Ok(Ending::Finished {
                    output: capture.text(),
                    status,
                });
            }
            if self.exited() {
                return Ok(self.exited_with(capture).await);
            }
            tokio::select! {
                chunk = self.output.recv(), if pipes_open => match chunk {
                    Some(chunk) => capture.push(&chunk),
                    None => pipes_open = false,
                },
                _ = self.exit.changed() => {}
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(Ending::TimedOut { output: capture.text() });
                }
                _ = cancel.cancelled() => return Ok(Ending::Cancelled),
            }
        }
    }

    async fn exited_with(&mut self, mut capture: Capture) -> Ending {
        let mut rest = Vec::new();
        self.drain(&mut rest).await;
        capture.push(&rest);
        let exit = self.exit.borrow().clone().unwrap_or(ShellExit {
            code: None,
            signal: None,
        });
        // dsh checks the exit before the end marker: an `exit` inside the
        // command never prints one anyway.
        Ending::Exited {
            output: capture.text(),
            exit,
        }
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| haystack[i..].starts_with(needle))
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| haystack[i..].starts_with(needle))
}

fn keep_tail(buf: &mut Vec<u8>, keep: usize) {
    if buf.len() > keep {
        buf.drain(..buf.len() - keep);
    }
}

/// The output of one command between its markers.
struct Capture {
    start: String,
    end: String,
    started: bool,
    /// Bytes before the start marker (stray output, bounded).
    pre: Vec<u8>,
    /// Bytes after the start marker, up to [`MAX_CAPTURE_BYTES`].
    body: Vec<u8>,
    /// Recent bytes once `body` is full, to find the end marker.
    overflow: Vec<u8>,
    full: bool,
    status: Option<i32>,
}

impl Capture {
    fn new(start: String, end: String) -> Capture {
        Capture {
            start,
            end,
            started: false,
            pre: Vec::new(),
            body: Vec::new(),
            overflow: Vec::new(),
            full: false,
            status: None,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.status.is_some() || chunk.is_empty() {
            return;
        }
        if !self.started {
            self.pre.extend_from_slice(chunk);
            let Some(at) = find(&self.pre, self.start.as_bytes(), 0) else {
                keep_tail(&mut self.pre, SCAN_WINDOW);
                return;
            };
            self.started = true;
            let rest = self.pre.split_off(at + self.start.len());
            self.pre.clear();
            return self.push(&rest);
        }
        if self.full {
            let from = self.overflow.len().saturating_sub(self.end.len() + 24);
            self.overflow.extend_from_slice(chunk);
            if let Some(status) =
                end_status(&self.overflow, self.end.as_bytes(), from).map(|(_, s)| s)
            {
                self.status = Some(status);
            } else {
                keep_tail(&mut self.overflow, SCAN_WINDOW);
            }
            return;
        }
        let from = self.body.len().saturating_sub(self.end.len() + 24);
        self.body.extend_from_slice(chunk);
        if let Some((at, status)) = end_status(&self.body, self.end.as_bytes(), from) {
            self.body.truncate(at);
            self.status = Some(status);
        } else if self.body.len() > MAX_CAPTURE_BYTES {
            // Keep the head; the end marker is looked for in what follows,
            // overlapping the cut so a marker across it is still found.
            let overlap = MAX_CAPTURE_BYTES.saturating_sub(self.end.len() + 24);
            self.overflow = self.body[overlap..].to_vec();
            self.body.truncate(MAX_CAPTURE_BYTES);
            self.full = true;
        }
    }

    /// dsh `commandOutput`/`partialOutput`: without the start line's newline
    /// and trailing newlines.
    fn text(&self) -> String {
        if !self.started {
            return String::new();
        }
        let mut body: &[u8] = &self.body;
        if self.status.is_none()
            && let Some(at) = find(body, self.end.as_bytes(), 0)
        {
            body = &body[..at];
        }
        // The real start line is the last one (`set -x` traces the wrapper).
        if let Some(at) = rfind(body, self.start.as_bytes()) {
            body = &body[at + self.start.len()..];
        }
        let text = String::from_utf8_lossy(body);
        let text = text
            .strip_prefix("\r\n")
            .or_else(|| text.strip_prefix('\n'))
            .unwrap_or(&text);
        trim_trailing_newlines(text).to_owned()
    }
}

/// The end marker with its `<digits>\n` status, searching from `from`.
fn end_status(buf: &[u8], end: &[u8], from: usize) -> Option<(usize, i32)> {
    let mut search = from;
    while let Some(at) = find(buf, end, search) {
        let rest = &buf[at + end.len()..];
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let after = &rest[digits..];
        let terminated = after.starts_with(b"\n") || after.starts_with(b"\r\n");
        if digits > 0 && terminated {
            let status = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
            return Some((at, status));
        }
        if digits == rest.len() || (after == b"\r") {
            // The status line is still arriving.
            return None;
        }
        search = at + 1;
    }
    None
}

fn trim_trailing_newlines(text: &str) -> &str {
    let mut out = text;
    loop {
        if let Some(rest) = out.strip_suffix("\r\n") {
            out = rest;
        } else if let Some(rest) = out.strip_suffix('\n') {
            out = rest;
        } else {
            return out;
        }
    }
}

/// dsh `maybeTruncate` with JavaScript string lengths (UTF-16 units).
fn maybe_truncate(content: &str, max: usize) -> String {
    let mut units = 0;
    for (i, c) in content.char_indices() {
        if units + c.len_utf16() > max {
            // `truncateWithoutSplittingSurrogatePair`: whole characters only.
            return format!("{}{TRUNCATED_MESSAGE}", &content[..i]);
        }
        units += c.len_utf16();
    }
    content.to_owned()
}

/// dsh `appendStatusMarker`.
fn append_status_marker(content: &str, marker: Option<&str>) -> String {
    match marker {
        None => content.to_owned(),
        Some(marker) if content.is_empty() => marker.to_owned(),
        Some(marker) => format!("{content}\n{marker}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_matches_dsh() {
        assert_eq!(quote_for_bash("a'b\\c\nd\re"), "$'a\\'b\\\\c\\nd\\re'");
    }

    #[test]
    fn capture_finds_markers_across_chunks() {
        let mut c = Capture::new("<S>".into(), "<E:".into());
        c.push(b"stray\n<S");
        c.push(b">\nhello\nwor");
        c.push(b"ld<E");
        assert_eq!(c.status, None);
        c.push(b":3");
        assert_eq!(c.status, None);
        c.push(b"\nlater");
        assert_eq!(c.status, Some(3));
        assert_eq!(c.text(), "hello\nworld");
    }

    #[test]
    fn capture_keeps_the_head_of_huge_output() {
        let mut c = Capture::new("<S>".into(), "<E:".into());
        c.push(b"<S>\n");
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..6 {
            c.push(&chunk);
        }
        c.push(b"\n<E:0\n");
        assert_eq!(c.status, Some(0));
        assert_eq!(c.body.len(), MAX_CAPTURE_BYTES);
    }

    #[test]
    fn truncation_counts_utf16_units() {
        assert_eq!(maybe_truncate("abc", 3), "abc");
        assert_eq!(maybe_truncate("abcd", 3), format!("abc{TRUNCATED_MESSAGE}"));
        // 😀 is two UTF-16 units; it is never split.
        assert_eq!(maybe_truncate("a😀b", 2), format!("a{TRUNCATED_MESSAGE}"));
        assert_eq!(maybe_truncate("a😀b", 3), format!("a😀{TRUNCATED_MESSAGE}"));
    }

    #[test]
    fn status_markers() {
        assert_eq!(append_status_marker("", Some("[x]")), "[x]");
        assert_eq!(append_status_marker("out", Some("[x]")), "out\n[x]");
        let exit = ShellExit {
            code: Some(3),
            signal: None,
        };
        assert_eq!(exit.marker(), "[shell exited: code 3]");
    }
}
