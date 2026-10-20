//! The model-facing tools, with dsh 0.2.0-rc.2's exact names, descriptions
//! and schemas (`docs/rust/upstream/request-header-0.2.0-rc.2.json`):
//! `subagent` (spawn provider, continuable background default),
//! `subagent_fork` (fork provider, one-shot, background as a job),
//! `send_message`, `interrupt_agent` and `list_agents`.
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use dshx_tools::ToolState;
use dshx_tools::jobs::{JobOutcome, JobSpec, JobStatus, JobToolCall};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::SubagentError;
use crate::records;
use crate::runtime::{Live, RunResult, SubagentRuntime};

const SCHEMAS_JSON: &str = include_str!("schemas.json");

/// The upstream schema of one subagent tool.
///
/// # Panics
/// When `name` is not one of the five subagent tools.
pub fn upstream_schema(name: &str) -> ToolSchema {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| serde_json::from_str(SCHEMAS_JSON).expect("embedded schemas parse"))
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no upstream schema for tool {name}"))
}

/// The five tools over one runtime, in dsh's request order (sorted by name).
/// Tools keep a weak reference: the host owns the runtime.
pub fn subagent_tools(runtime: Arc<SubagentRuntime>) -> Vec<Arc<dyn Tool>> {
    let weak = Arc::downgrade(&runtime);
    vec![
        Arc::new(InterruptAgentTool {
            runtime: weak.clone(),
        }),
        Arc::new(ListAgentsTool {
            runtime: weak.clone(),
        }),
        Arc::new(SendMessageTool {
            runtime: weak.clone(),
        }),
        Arc::new(DelegateTool {
            runtime: weak.clone(),
            name: "subagent",
            provider: "spawn",
            continuable: true,
        }),
        Arc::new(DelegateTool {
            runtime: weak,
            name: "subagent_fork",
            provider: "fork",
            continuable: false,
        }),
    ]
}

fn runtime_of(weak: &Weak<SubagentRuntime>) -> Result<Arc<SubagentRuntime>, ToolError> {
    weak.upgrade()
        .ok_or_else(|| ToolError::Failed("the subagent runtime is no longer available".into()))
}

/// dsh resolves `exec.agent`; here the calling session must be attached.
fn caller(runtime: &SubagentRuntime, ctx: &ToolContext, tool: &str) -> Result<Live, ToolError> {
    runtime.live(&ctx.session_id).ok_or_else(|| {
        ToolError::Failed(format!(
            "{tool} requires a calling agent (exec.agent was undefined)"
        ))
    })
}

fn refused(e: SubagentError) -> ToolError {
    ToolError::Failed(e.message)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key}: expected string")))
}

fn output(text: String, value: Value) -> ToolOutput {
    let mut out = ToolOutput::text(text);
    out.value = Some(value);
    out
}

/// dsh `stopReasonError`.
fn stop_reason_error(stop: &str) -> Option<String> {
    match stop {
        "completed" => None,
        "aborted" => Some("subagent run was cancelled".into()),
        "error" => Some("subagent run failed".into()),
        "max-tokens" => Some("subagent run hit its token limit before finishing".into()),
        "refusal" => Some("subagent declined the task".into()),
        other => Some(format!("subagent run ended abnormally ({other})")),
    }
}

/// dsh `settleForegroundRun` → the model-visible result or error.
fn foreground_result(child: &str, result: RunResult) -> Result<ToolOutput, ToolError> {
    if let Some(failure) = result.failure {
        return Err(ToolError::Failed(failure));
    }
    if let Some(headline) = stop_reason_error(result.stop_reason) {
        let text = records::output_text(&result.output);
        let partial = if text.is_empty() {
            String::new()
        } else {
            format!("\nPartial output before the run ended:\n{text}")
        };
        return Err(ToolError::Failed(format!("{headline}{partial}")));
    }
    let text = records::output_text(&result.output);
    Ok(output(
        text,
        json!({"kind": "foreground", "runId": child, "output": result.output}),
    ))
}

/// dsh `settleRun` / `runOutcome`: a background job's terminal outcome.
fn job_outcome(result: &RunResult) -> JobOutcome {
    if let Some(failure) = &result.failure {
        // dsh `String(error)` of the rejected run.
        return JobOutcome {
            status: JobStatus::Failed,
            detail: Some(format!("Error: {failure}")),
            result: None,
        };
    }
    match result.stop_reason {
        "completed" => JobOutcome {
            status: JobStatus::Completed,
            detail: None,
            result: Some(records::output_text(&result.output)),
        },
        "aborted" => JobOutcome {
            status: JobStatus::Killed,
            detail: None,
            result: None,
        },
        other => JobOutcome {
            status: JobStatus::Failed,
            detail: Some(other.to_owned()),
            result: None,
        },
    }
}

/// `subagent` and `subagent_fork` (dsh `tool-subagent`).
struct DelegateTool {
    runtime: Weak<SubagentRuntime>,
    name: &'static str,
    provider: &'static str,
    /// `backgroundMode: continuable` (else one-shot background jobs).
    continuable: bool,
}

#[async_trait]
impl Tool for DelegateTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema(self.name)
    }

    fn concurrency_safe(&self) -> bool {
        true
    }

    fn describe(&self, args: &Value) -> String {
        args.get("description")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| self.name.to_owned())
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let runtime = runtime_of(&self.runtime)?;
        let parent = caller(&runtime, &ctx, "subagent tool")?;
        let label = str_arg(&args, "description")?.to_owned();
        let prompt = str_arg(&args, "prompt")?.to_owned();
        let background = args
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(self.continuable);
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let max_depth = Some(runtime.settings().max_depth);
        if background && self.continuable {
            let (child, _) = runtime
                .start_continuable(&parent, self.provider, &label, &prompt, max_depth)
                .await
                .map_err(refused)?;
            return Ok(output(
                format!("started subagent {child}"),
                json!({"kind": "continuable", "subagentId": child}),
            ));
        }
        if background {
            return self
                .start_job(runtime, parent, &ctx, label, prompt, max_depth)
                .map(|job| {
                    output(
                        format!("started background subagent job {job}"),
                        json!({"kind": "background", "jobId": job}),
                    )
                });
        }
        let cancel = CancellationToken::new();
        let run = runtime
            .start_one_shot(&parent, self.provider, &label, &prompt, max_depth, cancel)
            .await
            .map_err(refused)?;
        let child = run.child_id.clone();
        let mut result = run.result;
        let settled = tokio::select! {
            r = &mut result => r,
            _ = ctx.cancel.cancelled() => {
                run.cancel.cancel();
                result.await
            }
        };
        let result = settled.map_err(|_| ToolError::Failed("subagent run was dropped".into()))?;
        if ctx.cancel.is_cancelled() {
            // dsh: cancellation supersedes the body's outcome once it settled.
            return Err(ToolError::Cancelled);
        }
        foreground_result(&child, result)
    }
}

impl DelegateTool {
    /// One-shot background run as a `subagent` job (dsh `jobs.start`).
    fn start_job(
        &self,
        runtime: Arc<SubagentRuntime>,
        parent: Live,
        ctx: &ToolContext,
        label: String,
        prompt: String,
        max_depth: Option<u32>,
    ) -> Result<String, ToolError> {
        let state = ToolState::get(ctx)?;
        let sink = state
            .jobs()
            .start(JobSpec {
                kind: "subagent".into(),
                label: label.clone(),
                owner: Some(ctx.session_id.clone()),
                tool_call: Some(JobToolCall {
                    call_id: ctx.call_id.clone(),
                    root_call_id: ctx.call_id.clone(),
                }),
            })
            .map_err(ToolError::Failed)?;
        let id = sink.id().to_owned();
        let cancel = CancellationToken::new();
        let kill = cancel.clone();
        sink.bind_cancel(move |_reason| kill.cancel());
        let provider = self.provider;
        tokio::spawn(async move {
            let outcome = match runtime
                .start_one_shot(
                    &parent,
                    provider,
                    &label,
                    &prompt,
                    max_depth,
                    cancel.clone(),
                )
                .await
            {
                Ok(run) => match run.result.await {
                    Ok(result) => job_outcome(&result),
                    Err(_) => JobOutcome {
                        status: JobStatus::Failed,
                        detail: Some("subagent run was dropped".into()),
                        result: None,
                    },
                },
                // dsh `settleStart`: a refused start is `failed` with `String(error)`.
                Err(_) if cancel.is_cancelled() => JobOutcome {
                    status: JobStatus::Killed,
                    detail: None,
                    result: None,
                },
                Err(e) => JobOutcome {
                    status: JobStatus::Failed,
                    detail: Some(e.display_with_name()),
                    result: None,
                },
            };
            sink.settle(outcome);
        });
        Ok(id)
    }
}

/// `send_message` (dsh `tool-subagent-control`).
struct SendMessageTool {
    runtime: Weak<SubagentRuntime>,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("send_message")
    }

    fn describe(&self, args: &Value) -> String {
        format!(
            "send_message {}",
            args.get("agent_id").and_then(Value::as_str).unwrap_or("")
        )
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let runtime = runtime_of(&self.runtime)?;
        let sender = caller(&runtime, &ctx, "send_message")?;
        let target = str_arg(&args, "agent_id")?.to_owned();
        let message = str_arg(&args, "message")?.to_owned();
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let id = runtime
            .send_message(&sender, &target, &message)
            .await
            .map_err(refused)?;
        Ok(output(
            format!("message delivered to agent {target}"),
            json!({"messageId": id}),
        ))
    }
}

/// `interrupt_agent` (dsh `tool-subagent-control`).
struct InterruptAgentTool {
    runtime: Weak<SubagentRuntime>,
}

#[async_trait]
impl Tool for InterruptAgentTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("interrupt_agent")
    }

    fn describe(&self, args: &Value) -> String {
        format!(
            "interrupt_agent {}",
            args.get("agent_id").and_then(Value::as_str).unwrap_or("")
        )
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let runtime = runtime_of(&self.runtime)?;
        let caller = caller(&runtime, &ctx, "interrupt_agent")?;
        let target = str_arg(&args, "agent_id")?.to_owned();
        runtime.interrupt(&target, &caller.id()).map_err(refused)?;
        Ok(output(
            format!("interrupt requested for agent {target}"),
            json!({"accepted": true}),
        ))
    }
}

/// `list_agents` (dsh `tool-subagent-control/list-agents`).
struct ListAgentsTool {
    runtime: Weak<SubagentRuntime>,
}

/// One model-facing row.
enum Row {
    Child {
        id: String,
        label: String,
        status: &'static str,
        at: Option<(String, u32)>,
    },
    Diagnostic {
        id: String,
        reason: &'static str,
        at: Option<(String, u32)>,
    },
}

impl Row {
    fn render(&self, descendants: bool) -> String {
        let at = |at: &Option<(String, u32)>| match at {
            Some((parent, depth)) if descendants => format!(" parent={parent} depth={depth}"),
            _ => String::new(),
        };
        match self {
            Row::Child {
                id,
                label,
                status,
                at: pos,
            } => format!("{id} [{status}]{} — {label}", at(pos)),
            Row::Diagnostic {
                id,
                reason,
                at: pos,
            } => {
                format!("{id} [diagnostic: {reason}]{}", at(pos))
            }
        }
    }

    fn value(&self) -> Value {
        let mut v = match self {
            Row::Child {
                id, label, status, ..
            } => json!({"kind": "child", "id": id, "label": label, "status": status}),
            Row::Diagnostic { id, reason, .. } => {
                json!({"kind": "diagnostic", "id": id, "reason": reason})
            }
        };
        let (Row::Child { at, .. } | Row::Diagnostic { at, .. }) = self;
        if let Some((parent, depth)) = at {
            v["parent"] = json!(parent);
            v["depth"] = json!(depth);
        }
        v
    }
}

fn read_failure(e: &anyhow::Error) -> &'static str {
    match e.downcast_ref::<dshx_session::SessionError>() {
        Some(dshx_session::SessionError::Corrupt(_)) => "corrupt",
        _ => "unavailable",
    }
}

impl ListAgentsTool {
    fn status(runtime: &SubagentRuntime, id: &str) -> &'static str {
        if runtime.is_running(id) {
            "running"
        } else {
            "inactive"
        }
    }

    /// dsh `listDescendants` + `project`: stable pre-order over parent catalogs.
    async fn descendants(runtime: &SubagentRuntime, root: &str) -> anyhow::Result<Vec<Row>> {
        let mut stack: Vec<(records::CatalogEntry, String, u32)> = runtime
            .catalog(root)
            .await?
            .into_iter()
            .map(|e| (e, root.to_owned(), 1))
            .rev()
            .collect();
        let mut visited = std::collections::HashSet::from([root.to_owned()]);
        let mut rows = Vec::new();
        while let Some((entry, parent, depth)) = stack.pop() {
            if !visited.insert(entry.id.clone()) {
                continue;
            }
            let at = Some((parent.clone(), depth));
            let children = match runtime.catalog(&entry.id).await {
                Ok(children) => children,
                Err(e) => {
                    rows.push(Row::Diagnostic {
                        id: entry.id,
                        reason: read_failure(&e),
                        at,
                    });
                    continue;
                }
            };
            if entry.mode == "unknown" {
                rows.push(Row::Diagnostic {
                    id: entry.id.clone(),
                    reason: "unsupported",
                    at,
                });
            } else if entry.mode == "continuable" {
                rows.push(Row::Child {
                    id: entry.id.clone(),
                    label: entry.label.clone().unwrap_or_default(),
                    status: Self::status(runtime, &entry.id),
                    at,
                });
            }
            for child in children.into_iter().rev() {
                stack.push((child, entry.id.clone(), depth + 1));
            }
        }
        Ok(rows)
    }
}

#[async_trait]
impl Tool for ListAgentsTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("list_agents")
    }

    fn describe(&self, _args: &Value) -> String {
        "list_agents".into()
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let runtime = runtime_of(&self.runtime)?;
        let parent = caller(&runtime, &ctx, "list_agents")?;
        let descendants = args.get("scope").and_then(Value::as_str) == Some("descendants");
        let id = parent.id();
        let rows = if descendants {
            Self::descendants(&runtime, &id)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?
        } else {
            runtime
                .catalog(&id)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?
                .into_iter()
                .filter(|e| e.mode == "continuable")
                .map(|e| Row::Child {
                    status: Self::status(&runtime, &e.id),
                    label: e.label.unwrap_or_default(),
                    id: e.id,
                    at: None,
                })
                .collect()
        };
        let text = if rows.is_empty() {
            "(no subagents)".to_owned()
        } else {
            rows.iter()
                .map(|r| r.render(descendants))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(output(
            text,
            Value::Array(rows.iter().map(Row::value).collect()),
        ))
    }
}
