//! Tool-call scheduling for one assistant step (dsh `agent-loop/src/tool-calls.ts`
//! plus the tool runtime's result shapes): exclusive calls are barriers,
//! concurrency-safe calls share a bounded rolling pool, and results commit in
//! model order. Cancellation drains started calls and records
//! `ABORTED_BEFORE_DISPATCH` pairs for the rest.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{
    ApprovalDecision, Approver, CallerInfo, ContentBlock, Message, MessageSource, Role, Tool,
    ToolContext, ToolError, ToolOutput, ToolRegistry, ToolResultPart, ToolSchema, kinds,
};
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{Inner, Target};
use crate::config::{ApprovalPolicy, Preset};
use crate::events::{event, message_json, surface_append};
use crate::observer::AgentStatus;
use crate::schema;

pub(crate) const TOOL_ABORTED: &str = "ABORTED";
pub(crate) const TOOL_ABORTED_BEFORE_DISPATCH: &str = "ABORTED_BEFORE_DISPATCH";
pub(crate) const TOOL_TIMEOUT: &str = "TOOL_TIMEOUT";

const FILE_TOUCH_TOOLS: [&str; 3] = ["read", "write", "edit"];
/// Bridged MCP tools (`mcp__<server>__<tool>`), whose arguments dsh does not validate.
const MCP_TOOL_PREFIX: &str = "mcp__";

/// One model tool call with parsed arguments.
#[derive(Clone)]
struct Planned {
    id: String,
    name: String,
    raw: String,
    args: Value,
}

/// The canonical outcome of one call before it is committed.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExecResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// `{name, code, reason?}` logged beside the message.
    pub error_info: Option<Value>,
    pub meta: Option<Value>,
    pub value: Option<Value>,
    pub contexts: Vec<Message>,
    /// Whether the tool body was aborted (touch notifications skip it).
    pub aborted: bool,
}

impl ExecResult {
    fn error(message: &str, info: Option<(&str, &str)>) -> Self {
        ExecResult {
            content: vec![ContentBlock::text(format!("Error: {message}"))],
            is_error: true,
            error_info: info.map(|(name, code)| json!({"name": name, "code": code})),
            ..Default::default()
        }
    }

    fn aborted_before_dispatch() -> Self {
        let mut r = ExecResult::error(
            "tool call aborted before dispatch",
            Some(("AbortError", TOOL_ABORTED_BEFORE_DISPATCH)),
        );
        r.aborted = true;
        r
    }

    fn aborted() -> Self {
        let mut r = ExecResult::error("tool call aborted", Some(("AbortError", TOOL_ABORTED)));
        r.aborted = true;
        r
    }

    fn timeout(ms: u128) -> Self {
        ExecResult::error(
            &format!("tool call timed out after {ms}ms"),
            Some(("ToolTimeoutError", TOOL_TIMEOUT)),
        )
    }

    fn from_output(out: ToolOutput) -> Self {
        ExecResult {
            content: out.content.into_iter().map(part_block).collect(),
            is_error: out.is_error,
            error_info: None,
            meta: out.meta,
            value: out.value,
            contexts: out.contexts,
            aborted: false,
        }
    }
}

fn part_block(part: ToolResultPart) -> ContentBlock {
    match part {
        ToolResultPart::Text { text } => ContentBlock::Text { text },
        ToolResultPart::Image { attachment } => ContentBlock::Image {
            attachment,
            offloaded: false,
        },
    }
}

/// Empty input means `{}`; invalid JSON stays the raw string (dsh `parseArguments`).
fn parse_arguments(raw: &str) -> Value {
    if raw.is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

enum Prepared {
    Dispatch(Arc<dyn Tool>),
    /// Result that still receives post-execute policy.
    Post(ExecResult),
    /// Final result (cancellation before dispatch).
    Final(ExecResult),
}

struct Slot {
    result: ExecResult,
    needs_post: bool,
}

struct GroupOutcome {
    consumed: usize,
    aborted: bool,
}

impl Inner {
    /// Whether a call may join the parallel pool (dsh `executionMode`).
    fn parallel(&self, call: &Planned) -> bool {
        if self.config.lock().unwrap().preset == Preset::Ptc {
            return false;
        }
        let tool = self.tools.lock().unwrap().get(&call.name);
        match tool {
            Some(t) => {
                t.concurrency_safe()
                    && schema::validate(&t.schema().parameters, &call.args).is_empty()
            }
            None => false,
        }
    }

    /// Runs one step's calls; returns whether a result concluded the turn.
    pub(crate) async fn execute_tool_calls(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        calls: Vec<(String, String, String)>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<bool> {
        let planned: Vec<Planned> = calls
            .into_iter()
            .map(|(id, name, raw)| {
                let args = parse_arguments(&raw);
                Planned {
                    id,
                    name,
                    raw,
                    args,
                }
            })
            .collect();
        let mut next = 0;
        while next < planned.len() {
            let parallel = self.parallel(&planned[next]);
            let group: &[Planned] = if parallel {
                &planned[next..]
            } else {
                &planned[next..next + 1]
            };
            let outcome = self.run_group(turn, step, group, parallel, cancel).await?;
            next += outcome.consumed;
            if outcome.aborted {
                for call in &planned[next..] {
                    self.append_skipped(turn, step, call).await?;
                }
                return Ok(false);
            }
        }
        Ok(false)
    }

    async fn run_group(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        group: &[Planned],
        parallel: bool,
        cancel: &CancellationToken,
    ) -> anyhow::Result<GroupOutcome> {
        let max = self.config.lock().unwrap().max_parallel_tool_calls.max(1);
        let mut slots: Vec<Option<Slot>> = (0..group.len()).map(|_| None).collect();
        let mut call_seqs: Vec<Option<u64>> = vec![None; group.len()];
        let mut next_to_start = 0;
        let mut committed = 0;
        let mut started = 0;
        let mut aborted = cancel.is_cancelled();
        let mut in_flight = FuturesUnordered::new();

        loop {
            // Fill the pool in model order; later calls are reclassified first.
            while !aborted && next_to_start < group.len() && in_flight.len() < max {
                let call = &group[next_to_start];
                if next_to_start > 0 && parallel && !self.parallel(call) {
                    break;
                }
                let seq = self
                    .append(event(
                        kinds::TOOL_CALL,
                        json!({"turn": turn, "step": step, "callId": call.id, "name": call.name, "arguments": call.raw}),
                    ))
                    .await?
                    .seq;
                call_seqs[next_to_start] = Some(seq);
                started += 1;
                match self.prepare(call, cancel) {
                    Prepared::Dispatch(tool) => {
                        self.journal.flush().await?;
                        let index = next_to_start;
                        let fut = self.clone().dispatch(tool, call.clone(), cancel.clone());
                        in_flight.push(async move { (index, fut.await) });
                    }
                    Prepared::Post(r) => {
                        slots[next_to_start] = Some(Slot {
                            result: r,
                            needs_post: true,
                        })
                    }
                    Prepared::Final(r) => {
                        slots[next_to_start] = Some(Slot {
                            result: r,
                            needs_post: false,
                        })
                    }
                }
                next_to_start += 1;
                committed = self
                    .commit_ready(turn, step, group, &mut slots, &call_seqs, committed)
                    .await?;
                if cancel.is_cancelled() {
                    aborted = true;
                }
            }
            let Some((index, result)) = in_flight.next().await else {
                break;
            };
            slots[index] = Some(Slot {
                result,
                needs_post: true,
            });
            committed = self
                .commit_ready(turn, step, group, &mut slots, &call_seqs, committed)
                .await?;
            if cancel.is_cancelled() {
                aborted = true;
            }
        }
        if aborted {
            for call in &group[started..] {
                self.append_skipped(turn, step, call).await?;
            }
            return Ok(GroupOutcome {
                consumed: group.len(),
                aborted: true,
            });
        }
        Ok(GroupOutcome {
            consumed: started,
            aborted: false,
        })
    }

    /// Ordered pre-dispatch gate: cancellation, unknown tool, argument validation.
    fn prepare(self: &Arc<Self>, call: &Planned, cancel: &CancellationToken) -> Prepared {
        let tool = if self.config.lock().unwrap().preset == Preset::Ptc {
            (call.name == "run_code")
                .then(|| Arc::new(crate::ptc::RunCodeTool::new(self)) as Arc<dyn Tool>)
        } else {
            self.tools.lock().unwrap().get(&call.name)
        };
        prepare_tool(tool, call, cancel)
    }

    /// Runs one tool body with its cooperative deadline and cancellation.
    async fn dispatch(
        self: Arc<Self>,
        tool: Arc<dyn Tool>,
        call: Planned,
        cancel: CancellationToken,
    ) -> ExecResult {
        let (cwd, sandbox, grace, policy) = {
            let c = self.config.lock().unwrap();
            // A transport must drain child tool cleanup and its worker before settling.
            let grace = if call.name == "run_code" && c.preset == Preset::Ptc {
                c.tool_grace.saturating_mul(3)
            } else {
                c.tool_grace
            };
            (c.cwd.clone(), c.sandbox, grace, c.approval)
        };
        let token = cancel.child_token();
        let approver: Arc<dyn Approver> = Arc::new(LoggingApprover {
            inner: self.clone(),
            policy,
            cancel: token.clone(),
        });
        let ctx = ToolContext {
            session_id: self.journal.id(),
            call_id: call.id.clone(),
            cwd,
            sandbox,
            cancel: token.clone(),
            attachments: self.attachments.clone(),
            model_accepts_images: self.model_accepts_images(),
            approver,
            state: self.tool_state.clone(),
            caller: self.caller_info(),
        };
        let timeout = tool.timeout();
        let fut = tool.execute(call.args.clone(), ctx);
        tokio::pin!(fut);
        let mut timed_out = false;
        let settled = tokio::select! {
            r = &mut fut => Some(r),
            _ = sleep_opt(timeout) => {
                timed_out = true;
                token.cancel();
                tokio::time::timeout(grace, &mut fut).await.ok()
            }
            _ = cancel.cancelled() => tokio::time::timeout(grace, &mut fut).await.ok(),
        };
        if timed_out {
            return ExecResult::timeout(timeout.unwrap_or_default().as_millis());
        }
        match settled {
            None => ExecResult::aborted(),
            Some(Ok(out)) => {
                let r = ExecResult::from_output(out);
                if cancel.is_cancelled() && !r.is_error {
                    ExecResult::aborted()
                } else {
                    r
                }
            }
            Some(Err(ToolError::Cancelled)) => ExecResult::aborted(),
            Some(Err(ToolError::Timeout(d))) => ExecResult::timeout(d.as_millis()),
            Some(Err(ToolError::InvalidArguments(m))) => {
                ExecResult::error(&m, Some(("ToolArgsError", "INVALID_ARGS")))
            }
            Some(Err(ToolError::Failed(m))) => ExecResult::error(&m, None),
            Some(Err(ToolError::Coded {
                name,
                code,
                message,
            })) => ExecResult::error(&message, Some((&name, &code))),
        }
    }

    /// The calling agent as tools see it (`ToolContext::caller`).
    fn caller_info(&self) -> Option<CallerInfo> {
        let (turn, turn_start_seq) = self.state.lock().unwrap().open_turn?;
        // dsh `agents.roots()`: lineage alone does not make a child, so a
        // branch (parentSession without origin "subagent") is still a root.
        let header = self.journal.header();
        let root = header.delegation_depth == 0
            && header.extra.get("origin").and_then(|o| o.as_str()) != Some("subagent");
        Some(CallerInfo {
            root,
            turn,
            turn_start_seq,
            session: Arc::new(self.journal.clone()),
        })
    }

    /// Commits contiguous settled slots in model order.
    async fn commit_ready(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        group: &[Planned],
        slots: &mut [Option<Slot>],
        call_seqs: &[Option<u64>],
        mut committed: usize,
    ) -> anyhow::Result<usize> {
        while committed < group.len() {
            let Some(slot) = slots[committed].take() else {
                break;
            };
            let call = &group[committed];
            let mut result = slot.result;
            if slot.needs_post {
                self.post_execute(call, &mut result);
                self.bound_inline(call, &mut result);
            }
            self.notify_result(call, &result);
            self.commit(
                turn,
                step,
                call,
                result,
                call_seqs[committed].unwrap_or_default(),
            )
            .await?;
            committed += 1;
        }
        Ok(committed)
    }

    /// Ordered post-execute policy: the repeat-tool reminder.
    fn post_execute(&self, call: &Planned, result: &mut ExecResult) {
        let (enabled, thresholds, preview) = {
            let c = self.config.lock().unwrap();
            (
                c.repeat_reminder.enabled,
                c.repeat_reminder.thresholds.clone(),
                c.repeat_reminder.arguments_preview_chars,
            )
        };
        if !enabled || thresholds.is_empty() {
            return;
        }
        let mut sorted = thresholds;
        sorted.sort_unstable();
        let canonical = serde_json::to_string(&sort_json(&call.args)).unwrap_or_default();
        let key = serde_json::to_string(&json!([call.name, canonical])).unwrap_or_default();
        let count = {
            let mut st = self.state.lock().unwrap();
            let count = match &st.repeat_chain {
                Some((k, c)) if *k == key => c + 1,
                _ => 1,
            };
            st.repeat_chain = Some((key, count));
            count
        };
        if !sorted.contains(&count) {
            return;
        }
        let text = if count == sorted[0] {
            "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.".to_owned()
        } else {
            format!(
                "Repeated tool call detected:\n- tool: {}\n- consecutive_calls: {count}\n- arguments: {}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered.",
                call.name,
                preview_arguments(&canonical, preview)
            )
        };
        let reminder = Message {
            id: dshx_core::new_id(),
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            source: MessageSource::Other(json!({
                "kind": "repeat-tool-reminder",
                "form": "notice",
                "summary": format!("{} × {count}", call.name),
            })),
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        };
        result.contexts.insert(0, reminder);
    }

    /// dsh `spill-policy`: an oversized text result keeps its head and tail
    /// inline and the full text goes to a spill file (not for `read`).
    fn bound_inline(&self, call: &Planned, result: &mut ExecResult) {
        let Some(state) = &self.tools_state else {
            return;
        };
        if call.name == "read" {
            return;
        }
        if let [ContentBlock::Text { text }] = result.content.as_slice()
            && let Some(bounded) = dshx_tools::spill::bound_inline_text(
                state.spill(),
                &self.journal.id(),
                &call.name,
                text,
                dshx_tools::spill::DEFAULT_MAX_INLINE_TOKENS,
            )
        {
            result.content = vec![ContentBlock::Text { text: bounded }];
        }
    }

    /// `tools/result` observers: workspace-instruction touch tracking.
    fn notify_result(&self, call: &Planned, result: &ExecResult) {
        if result.is_error || result.aborted || !FILE_TOUCH_TOOLS.contains(&call.name.as_str()) {
            return;
        }
        if let Some(path) = call
            .args
            .get("file_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            self.state.lock().unwrap().touches.push(path.to_owned());
        }
    }

    async fn commit(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        call: &Planned,
        result: ExecResult,
        call_seq: u64,
    ) -> anyhow::Result<()> {
        self.commit_tool_effects(call, &result).await?;
        let message = Message {
            id: dshx_core::new_id(),
            role: Role::Tool,
            content: result.content,
            source: MessageSource::Tool {
                call_id: call.id.clone(),
            },
            tool_call_id: Some(call.id.clone()),
            is_error: Some(result.is_error),
            extra: Default::default(),
        };
        let mut data = Map::new();
        data.insert("turn".into(), turn.into());
        data.insert("step".into(), step.into());
        data.insert("message".into(), message_json(&message));
        if let Some(info) = result.error_info {
            data.insert("error".into(), info);
        }
        if let Some(meta) = result.meta {
            data.insert("meta".into(), meta);
        }
        self.append(surface_append(
            kinds::TOOL_RESULT,
            Value::Object(data),
            Some(vec![call_seq]),
        ))
        .await?;
        for context in result.contexts {
            self.splice(Target::NextStep, usize::MAX, 0, vec![context], false)
                .await?;
        }
        Ok(())
    }

    /// Domain events belong to the underlying capability, including nested PTC calls.
    async fn commit_tool_effects(
        self: &Arc<Self>,
        call: &Planned,
        result: &ExecResult,
    ) -> anyhow::Result<()> {
        if call.name == "todo_write" && !result.is_error {
            let todos = result
                .value
                .as_ref()
                .and_then(|v| v.get("todos").or(if v.is_array() { Some(v) } else { None }))
                .or_else(|| call.args.get("todos"))
                .cloned();
            if let Some(todos) = todos {
                self.append(event(kinds::TODO_WRITE, json!({"todos": todos})))
                    .await?;
            }
        }
        // Job lifecycle events of this call (e.g. a backgrounded bash) precede its result.
        crate::jobs::drain(self).await;
        Ok(())
    }

    /// The durable call/result pair for a call skipped by cancellation.
    async fn append_skipped(
        self: &Arc<Self>,
        turn: u64,
        step: u64,
        call: &Planned,
    ) -> anyhow::Result<()> {
        let seq = self
            .append(event(
                kinds::TOOL_CALL,
                json!({"turn": turn, "step": step, "callId": call.id, "name": call.name, "arguments": call.raw}),
            ))
            .await?
            .seq;
        self.commit(turn, step, call, ExecResult::aborted_before_dispatch(), seq)
            .await
    }
}

fn prepare_tool(
    tool: Option<Arc<dyn Tool>>,
    call: &Planned,
    cancel: &CancellationToken,
) -> Prepared {
    if cancel.is_cancelled() {
        return Prepared::Final(ExecResult::aborted_before_dispatch());
    }
    let Some(tool) = tool else {
        return Prepared::Post(ExecResult::error(
            &format!("unknown tool \"{}\"", call.name),
            Some(("ToolNotFoundError", "UNKNOWN_TOOL")),
        ));
    };
    let violations = if call.name.starts_with(MCP_TOOL_PREFIX) {
        Vec::new()
    } else {
        schema::validate(&tool.schema().parameters, &call.args)
    };
    if !violations.is_empty() {
        return Prepared::Post(ExecResult::error(
            &format!("invalid arguments: {}", violations.join("; ")),
            Some(("ToolArgsError", "INVALID_ARGS")),
        ));
    }
    Prepared::Dispatch(tool)
}

struct PtcRequest {
    call: Planned,
    cancel: CancellationToken,
    reply: oneshot::Sender<Result<Value, ToolError>>,
}

struct PtcSlot {
    request: PtcRequest,
    result: Option<ExecResult>,
    exclusive: bool,
}

/// A fresh FIFO scheduler for each program. The worker only receives JSON values.
pub(crate) struct PtcDispatcher {
    schemas: Vec<ToolSchema>,
    tx: mpsc::UnboundedSender<PtcRequest>,
    cancel: CancellationToken,
    root: String,
    next: AtomicU64,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), ToolError>>>>,
}

impl PtcDispatcher {
    pub(crate) fn new(inner: Arc<Inner>, ctx: &ToolContext) -> Arc<Self> {
        let tools = inner.tools.lock().unwrap().clone();
        let schemas = tools
            .schemas()
            .into_iter()
            .filter(|s| s.name != "run_code")
            .collect();
        let max = inner.config.lock().unwrap().max_parallel_tool_calls.max(1);
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = ctx.cancel.clone();
        let root = ctx.call_id.clone();
        let task = tokio::spawn(drive_ptc(
            inner,
            tools,
            rx,
            root.clone(),
            cancel.clone(),
            max,
        ));
        Arc::new(Self {
            schemas,
            tx,
            cancel,
            root,
            next: AtomicU64::new(1),
            task: tokio::sync::Mutex::new(Some(task)),
        })
    }

    pub(crate) async fn finish(&self) -> Result<(), ToolError> {
        self.cancel.cancel();
        if let Some(task) = self.task.lock().await.take() {
            task.await
                .map_err(|e| ToolError::Failed(format!("PTC dispatch cleanup failed: {e}")))??;
        }
        Ok(())
    }
}

impl Drop for PtcDispatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[async_trait]
impl dshx_tools::ptc::PtcBindings for PtcDispatcher {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }

    async fn call(
        &self,
        name: String,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value, ToolError> {
        if self.cancel.is_cancelled() || cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, result) = oneshot::channel();
        let call = Planned {
            id: format!("{}:ptc:{n}", self.root),
            name,
            raw: serde_json::to_string(&args)
                .map_err(|e| ToolError::InvalidArguments(e.to_string()))?,
            args,
        };
        // No await before sending: polling binding calls once in frame order is sufficient.
        self.tx
            .send(PtcRequest {
                call,
                cancel,
                reply,
            })
            .map_err(|_| ToolError::Cancelled)?;
        result.await.unwrap_or(Err(ToolError::Cancelled))
    }
}

async fn drive_ptc(
    inner: Arc<Inner>,
    tools: ToolRegistry,
    mut rx: mpsc::UnboundedReceiver<PtcRequest>,
    root: String,
    cancel: CancellationToken,
    max: usize,
) -> Result<(), ToolError> {
    let mut queue: VecDeque<PtcRequest> = VecDeque::new();
    let mut slots: VecDeque<PtcSlot> = VecDeque::new();
    let mut flights: FuturesUnordered<BoxFuture<'static, (String, ExecResult)>> =
        FuturesUnordered::new();
    let mut exclusive = false;
    let mut closed = false;
    let mut failure = None;
    loop {
        while slots.front().is_some_and(|s| s.result.is_some()) {
            let slot = slots.pop_front().unwrap();
            let outcome = inner
                .commit_ptc(&root, &slot.request.call, slot.result.unwrap())
                .await;
            if let Err(error) = &outcome {
                // A normal tool error is data; only an audit/commit failure stops the program.
                if matches!(error, ToolError::Coded { code, .. } if code == "PTC_COMMIT_FAILED") {
                    failure = Some(error.to_string());
                    cancel.cancel();
                }
            }
            let _ = slot.request.reply.send(outcome);
        }
        if cancel.is_cancelled() {
            rx.close();
            closed = true;
            while let Ok(request) = rx.try_recv() {
                queue.push_back(request);
            }
            while let Some(request) = queue.pop_front() {
                let _ = request.reply.send(Err(ToolError::Cancelled));
            }
        }
        while !cancel.is_cancelled() && flights.len() < max {
            let Some(head) = queue.front() else {
                break;
            };
            let tool = (head.call.name != "run_code")
                .then(|| tools.get(&head.call.name))
                .flatten();
            let safe = tool.as_ref().is_some_and(|t| {
                t.concurrency_safe()
                    && schema::validate(&t.schema().parameters, &head.call.args).is_empty()
            });
            if exclusive || (!safe && !flights.is_empty()) {
                break;
            }
            let request = queue.pop_front().unwrap();
            if request.cancel.is_cancelled() {
                let _ = request.reply.send(Err(ToolError::Cancelled));
                continue;
            }
            let start = inner
                .append(event(
                    "tool/ptc-dispatch-start",
                    ptc_pair(&root, &request.call),
                ))
                .await;
            if let Err(error) = start {
                failure = Some(format!("PTC dispatch audit failed: {error}"));
                let _ = request
                    .reply
                    .send(Err(ToolError::Failed(failure.clone().unwrap())));
                cancel.cancel();
                break;
            }
            let prepared = prepare_tool(tool, &request.call, &cancel);
            let result = match prepared {
                Prepared::Dispatch(tool) => {
                    if let Err(error) = inner.journal.flush().await {
                        failure = Some(format!("PTC dispatch flush failed: {error}"));
                        cancel.cancel();
                        Some(ExecResult::aborted_before_dispatch())
                    } else {
                        let exec = inner.clone();
                        let call = request.call.clone();
                        let token = cancel.child_token();
                        let requested_cancel = request.cancel.clone();
                        flights.push(Box::pin(async move {
                            let id = call.id.clone();
                            let run = exec.dispatch(tool, call, token.clone());
                            tokio::pin!(run);
                            let result = tokio::select! {
                                result = &mut run => result,
                                _ = requested_cancel.cancelled() => { token.cancel(); run.await }
                            };
                            (id, result)
                        }));
                        exclusive = !safe;
                        None
                    }
                }
                Prepared::Post(result) | Prepared::Final(result) => Some(result),
            };
            slots.push_back(PtcSlot {
                request,
                result,
                exclusive: !safe,
            });
            if !safe || slots.front().is_some_and(|s| s.result.is_some()) {
                break;
            }
        }
        if slots.front().is_some_and(|s| s.result.is_some()) {
            continue;
        }
        if closed && flights.is_empty() && queue.is_empty() {
            break;
        }
        tokio::select! {
            request = rx.recv(), if !closed => match request {
                Some(request) => queue.push_back(request),
                None => closed = true,
            },
            result = flights.next(), if !flights.is_empty() => {
                if let Some((id, result)) = result
                    && let Some(slot) = slots.iter_mut().find(|s| s.request.call.id == id) {
                    if slot.exclusive { exclusive = false; }
                    slot.result = Some(result);
                }
            },
            _ = cancel.cancelled(), if !closed => {},
        }
    }
    match failure {
        Some(error) => Err(ToolError::Failed(error)),
        None => Ok(()),
    }
}

fn ptc_pair(root: &str, call: &Planned) -> Value {
    json!({"rootCallId":root,"parentCallId":root,"subCallId":call.id,"name":call.name,"arguments":call.args})
}

impl Inner {
    async fn commit_ptc(
        self: &Arc<Self>,
        root: &str,
        call: &Planned,
        mut result: ExecResult,
    ) -> Result<Value, ToolError> {
        self.post_execute(call, &mut result);
        self.notify_result(call, &result);
        let value = result
            .value
            .clone()
            .unwrap_or_else(|| json!({"content":result.content}));
        self.bound_inline(call, &mut result);
        let commit = async {
            self.commit_tool_effects(call, &result).await?;
            let mut data = ptc_pair(root, call);
            data["isError"] = result.is_error.into();
            data["content"] = serde_json::to_value(&result.content)?;
            if let Some(info) = &result.error_info {
                data["error"] = info.clone();
            }
            if let Some(meta) = &result.meta {
                data["meta"] = meta.clone();
            }
            self.append(event("tool/ptc-dispatch", data)).await?;
            if !result.is_error
                && result
                    .content
                    .iter()
                    .any(|c| matches!(c, ContentBlock::Image { .. }))
            {
                result.contexts.push(Message {
                    id: dshx_core::new_id(),
                    role: Role::User,
                    content: result.content.clone(),
                    source: MessageSource::Other(json!({"kind":"ptc-mode"})),
                    tool_call_id: None,
                    is_error: None,
                    extra: Default::default(),
                });
            }
            for context in std::mem::take(&mut result.contexts) {
                self.splice(Target::NextStep, usize::MAX, 0, vec![context], false)
                    .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        commit
            .map_err(|e| ToolError::coded("PtcCommitError", "PTC_COMMIT_FAILED", e.to_string()))?;
        if result.is_error {
            let text = result
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Err(ToolError::Failed(
                text.strip_prefix("Error: ").unwrap_or(&text).to_owned(),
            ))
        } else {
            Ok(value)
        }
    }
}

async fn sleep_opt(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Deep key sort (repeat-tool-reminder canonical arguments).
fn sort_json(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(sort_json).collect()),
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let mut m = Map::new();
            for k in keys {
                m.insert(k.clone(), sort_json(&o[k]));
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

fn preview_arguments(canonical: &str, cap: usize) -> String {
    let units: Vec<u16> = canonical.encode_utf16().collect();
    if units.len() <= cap {
        return canonical.to_owned();
    }
    format!(
        "{}… (+{} more chars)",
        String::from_utf16_lossy(&units[..cap]),
        units.len() - cap
    )
}

/// Wraps the front end's approver with dsh's audit events and policy.
struct LoggingApprover {
    inner: Arc<Inner>,
    policy: ApprovalPolicy,
    cancel: CancellationToken,
}

#[async_trait]
impl Approver for LoggingApprover {
    async fn approve(
        &self,
        call_id: &str,
        tool: &str,
        summary: &str,
        justification: Option<&str>,
    ) -> ApprovalDecision {
        let id = dshx_core::new_id();
        let mut asked = Map::new();
        asked.insert("id".into(), id.clone().into());
        asked.insert("toolName".into(), tool.into());
        asked.insert("callId".into(), call_id.into());
        let reason = justification.filter(|j| !j.is_empty()).unwrap_or(summary);
        if !reason.is_empty() {
            asked.insert("reason".into(), reason.into());
        }
        if self
            .inner
            .append(event(kinds::APPROVAL_ASKED, Value::Object(asked)))
            .await
            .is_err()
        {
            return ApprovalDecision::Unavailable;
        }
        let policy = self.inner.config.lock().unwrap().approval;
        let outcome = if self.cancel.is_cancelled() {
            ApprovalDecision::Cancelled
        } else if policy == ApprovalPolicy::Never || self.policy == ApprovalPolicy::Never {
            ApprovalDecision::Rejected
        } else {
            self.waiting(1);
            let d = tokio::select! {
                d = self.inner.approver.approve(call_id, tool, summary, justification) => d,
                _ = self.cancel.cancelled() => ApprovalDecision::Cancelled,
            };
            self.waiting(-1);
            d
        };
        let decided = json!({"id": id, "outcome": outcome});
        if self
            .inner
            .append(event(kinds::APPROVAL_DECIDED, decided))
            .await
            .is_err()
        {
            return ApprovalDecision::Unavailable;
        }
        outcome
    }
}

impl LoggingApprover {
    fn waiting(&self, delta: i32) {
        let status = {
            let mut st = self.inner.state.lock().unwrap();
            st.pending_approvals = (st.pending_approvals as i32 + delta).max(0) as u32;
            if st.running.is_none() {
                return;
            }
            if st.pending_approvals > 0 {
                AgentStatus::WaitingApproval
            } else {
                AgentStatus::Running
            }
        };
        self.inner.set_status(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse_like_dsh() {
        assert_eq!(parse_arguments(""), json!({}));
        assert_eq!(parse_arguments("{\"a\":1}"), json!({"a": 1}));
        assert_eq!(parse_arguments("{\"a\":"), json!("{\"a\":"));
    }

    #[test]
    fn canonical_arguments_sort_keys() {
        let v = json!({"b": 1, "a": {"d": 2, "c": [3]}});
        assert_eq!(
            serde_json::to_string(&sort_json(&v)).unwrap(),
            "{\"a\":{\"c\":[3],\"d\":2},\"b\":1}"
        );
        assert_eq!(preview_arguments("abcdef", 3), "abc… (+3 more chars)");
    }
}
