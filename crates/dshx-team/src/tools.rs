//! The model-facing Agent Teams tools (dsh `dsh-experimental-tool-agent-team`):
//! `spawn_teammate`, `send_message`, `list_agents`, `wait_agent`,
//! `interrupt_agent` and the `team_task_*` board. They replace the subagent
//! tools and are offered to the Lead and every teammate.
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};

use crate::service::{TaskUpdate, TeamError, TeamService};

const SCHEMAS_JSON: &str = include_str!("../../../docs/rust/upstream/tools-0.2.0-rc.2/team.json");

/// dsh `NO_ACTIVE_PEER_MESSAGE`.
pub(crate) const NO_ACTIVE_PEER_MESSAGE: &str = "No other Team member is running or provisioning. wait_agent cannot make progress or wake inactive teammates. Re-list with list_agents and team_task_list, then use send_message to wake each required inactive teammate before waiting again.";

/// Names of the tools [`team_tools`] returns, in request order.
pub const TOOL_NAMES: [&str; 9] = [
    "interrupt_agent",
    "list_agents",
    "send_message",
    "spawn_teammate",
    "team_task_create",
    "team_task_get",
    "team_task_list",
    "team_task_update",
    "wait_agent",
];

/// The upstream schema of one Team tool.
///
/// # Panics
/// When `name` is not one of the nine Team tools.
pub fn upstream_schema(name: &str) -> ToolSchema {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            let golden: Value = serde_json::from_str(SCHEMAS_JSON).expect("team golden parses");
            golden["tools"]
                .as_array()
                .expect("golden tools")
                .iter()
                .map(|t| ToolSchema {
                    name: t["name"].as_str().unwrap_or_default().to_owned(),
                    description: t["description"].as_str().unwrap_or_default().to_owned(),
                    parameters: t["parameters"].clone(),
                })
                .collect()
        })
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no upstream schema for tool {name}"))
}

/// The nine Team tools over one service, sorted by name.
pub fn team_tools(service: &Arc<TeamService>) -> Vec<Arc<dyn Tool>> {
    TOOL_NAMES
        .iter()
        .map(|name| {
            Arc::new(TeamTool {
                service: Arc::downgrade(service),
                name,
            }) as Arc<dyn Tool>
        })
        .collect()
}

struct TeamTool {
    service: Weak<TeamService>,
    name: &'static str,
}

fn output(value: Value) -> ToolOutput {
    let mut out = ToolOutput::text(serde_json::to_string(&value).unwrap_or_default());
    out.value = Some(value);
    out
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[async_trait]
impl Tool for TeamTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema(self.name)
    }

    fn describe(&self, args: &Value) -> String {
        match self.name {
            "spawn_teammate" => format!(
                "spawn_teammate {}",
                args.get("name").and_then(Value::as_str).unwrap_or("")
            ),
            "send_message" | "interrupt_agent" => format!(
                "{} {}",
                self.name,
                args.get("target").and_then(Value::as_str).unwrap_or("")
            ),
            other => other.to_owned(),
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let service = self.service.upgrade().ok_or_else(|| {
            ToolError::Failed("the Agent Teams service is no longer available".into())
        })?;
        self.run(&service, &args, &ctx)
            .await
            .map(output)
            .map_err(TeamError::into_tool_error)
    }
}

impl TeamTool {
    async fn run(
        &self,
        service: &TeamService,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<Value, TeamError> {
        let caller = service.require(&ctx.session_id)?;
        match self.name {
            "spawn_teammate" => {
                let context = args
                    .get("context")
                    .and_then(Value::as_str)
                    .unwrap_or("fresh");
                service
                    .spawn(
                        &caller,
                        args["name"].as_str().unwrap_or_default(),
                        args["description"].as_str().unwrap_or_default(),
                        args["prompt"].as_str().unwrap_or_default(),
                        context,
                        &ctx.cancel,
                    )
                    .await
            }
            "send_message" => {
                service
                    .send(
                        &caller,
                        args["target"].as_str().unwrap_or_default(),
                        args["message"].as_str().unwrap_or_default(),
                    )
                    .await
            }
            "list_agents" => service.list_members(&caller).map(Value::Array),
            "wait_agent" => {
                let timeout = match args.get("timeout_ms") {
                    None => 30_000,
                    Some(v) => v
                        .as_f64()
                        .filter(|f| f.fract() == 0.0)
                        .map_or(-1, |f| f as i64),
                };
                service.wait(&caller, timeout, &ctx.cancel).await
            }
            "interrupt_agent" => {
                service.interrupt(&caller, args["target"].as_str().unwrap_or_default())
            }
            "team_task_create" => {
                service
                    .create_task(
                        &caller,
                        args["subject"].as_str().unwrap_or_default(),
                        args["description"].as_str().unwrap_or_default(),
                        args.get("blocked_by").and_then(strings),
                        args.get("write_scopes").and_then(strings),
                    )
                    .await
            }
            "team_task_get" => {
                service.get_task(&caller, args["task_id"].as_str().unwrap_or_default())
            }
            "team_task_list" => {
                let status = args.get("status").and_then(Value::as_str);
                let owner = args.get("owner").and_then(Value::as_str);
                let ready = args.get("ready").and_then(Value::as_bool);
                let filtered: Vec<Value> = service
                    .list_tasks(&caller)?
                    .into_iter()
                    .filter(|t| status.is_none_or(|s| t["status"] == s))
                    .filter(|t| match owner {
                        None => true,
                        Some("unowned") => t.get("ownerName").is_none(),
                        Some(o) => t["ownerName"] == o,
                    })
                    .filter(|t| ready.is_none_or(|r| t["ready"] == r))
                    .collect();
                let int = |k: &str, default: i64| match args.get(k) {
                    None => Some(default),
                    Some(v) => v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64),
                };
                let cursor = int("cursor", 0)
                    .filter(|c| *c >= 0)
                    .ok_or_else(|| TeamError::plain("cursor must be a non-negative safe integer"))?
                    as usize;
                let limit = int("limit", 50)
                    .filter(|l| (1..=100).contains(l))
                    .ok_or_else(|| {
                        TeamError::plain("limit must be an integer from 1 through 100")
                    })? as usize;
                let end = cursor.saturating_add(limit).min(filtered.len());
                let page: Vec<Value> = filtered
                    .get(cursor.min(filtered.len())..end)
                    .unwrap_or_default()
                    .to_vec();
                let mut v = json!({"tasks": page});
                if cursor + limit < filtered.len() {
                    v["nextCursor"] = (cursor + limit).into();
                }
                Ok(v)
            }
            _ => {
                service
                    .update_task(
                        &caller,
                        TaskUpdate {
                            task_id: args["task_id"].as_str().unwrap_or_default().to_owned(),
                            expected_revision: args["expected_revision"]
                                .as_f64()
                                .map_or(-1, |f| f as i64),
                            action: args["action"].as_str().unwrap_or_default().to_owned(),
                            subject: opt_str(args, "subject"),
                            description: opt_str(args, "description"),
                            blocked_by: args.get("blocked_by").and_then(strings),
                            write_scopes: args.get("write_scopes").and_then(strings),
                            owner: opt_str(args, "owner"),
                        },
                    )
                    .await
            }
        }
    }
}
