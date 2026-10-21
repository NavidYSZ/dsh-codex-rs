//! The model-facing `schedule_create`, `schedule_list`, `schedule_delete`
//! and `schedule_update` tools (dsh `registerScheduleTools`, root agents
//! only). Every result, including input errors, is a value the model reads
//! as compact JSON.
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};

use crate::record::{MAX_TITLE_LENGTH, utf16_len};
use crate::service::ScheduleService;
use crate::time::safe;

const SCHEMAS_JSON: &str =
    include_str!("../../../docs/rust/upstream/tools-0.2.0-rc.2/schedule.json");

/// Names of the tools [`schedule_tools`] returns, in request order.
pub const TOOL_NAMES: [&str; 4] = [
    "schedule_create",
    "schedule_delete",
    "schedule_list",
    "schedule_update",
];

/// The upstream schema of one schedule tool.
///
/// # Panics
/// When `name` is not one of the four schedule tools.
pub fn upstream_schema(name: &str) -> ToolSchema {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            let golden: Value = serde_json::from_str(SCHEMAS_JSON).expect("schedule golden parses");
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

/// The four tools over one service, sorted by name.
pub fn schedule_tools(service: &Arc<ScheduleService>) -> Vec<Arc<dyn Tool>> {
    TOOL_NAMES
        .iter()
        .map(|name| {
            Arc::new(ScheduleTool {
                service: Arc::downgrade(service),
                name,
            }) as Arc<dyn Tool>
        })
        .collect()
}

struct ScheduleTool {
    service: Weak<ScheduleService>,
    name: &'static str,
}

fn value(v: Value) -> ToolOutput {
    let mut out = ToolOutput::text(serde_json::to_string(&v).unwrap_or_default());
    out.value = Some(v);
    out
}

fn error(code: &str, message: &str) -> Value {
    json!({"code": code, "message": message})
}

fn internal() -> Value {
    error("internal_error", "The schedule operation failed.")
}

fn present(args: &Value, key: &str) -> bool {
    args.get(key).is_some_and(|v| !v.is_null())
}

/// One supplied fixed-rate interval (dsh `invalidInterval`).
fn invalid_interval(args: &Value) -> Option<Value> {
    if !present(args, "every_seconds") {
        return None;
    }
    match safe(&args["every_seconds"]) {
        None => Some(error(
            "invalid_rule",
            "every_seconds must be a safe integer.",
        )),
        Some(s) if s < 60 => Some(error(
            "frequency_too_high",
            "every_seconds must be at least 60.",
        )),
        Some(_) => None,
    }
}

fn title_error(title: &str) -> Option<Value> {
    if title.trim().is_empty() {
        return Some(error(
            "invalid_prompt",
            "title is required and must be non-empty after trimming.",
        ));
    }
    if utf16_len(title.trim()) > MAX_TITLE_LENGTH {
        return Some(error(
            "invalid_prompt",
            "title must be at most 120 characters.",
        ));
    }
    None
}

/// dsh `validateCreateArgs`.
fn validate_create(args: &Value) -> Option<Value> {
    let allowed = [
        "prompt",
        "title",
        "after_seconds",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ];
    let unknown = args
        .as_object()
        .is_some_and(|m| m.keys().any(|k| !allowed.contains(&k.as_str())));
    let selectors = allowed[2..].iter().filter(|k| present(args, k)).count();
    if unknown || selectors != 1 {
        return Some(error(
            "invalid_selector",
            "schedule_create accepts exactly one of after_seconds, at, every_seconds, daily, weekly, or cron.",
        ));
    }
    if args["prompt"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Some(error(
            "invalid_prompt",
            "prompt must be non-empty after trimming.",
        ));
    }
    if let Some(e) = title_error(args["title"].as_str().unwrap_or_default()) {
        return Some(e);
    }
    if present(args, "after_seconds") && safe(&args["after_seconds"]).is_none_or(|s| s <= 0) {
        return Some(error(
            "invalid_rule",
            "after_seconds must be a positive safe integer.",
        ));
    }
    invalid_interval(args)
}

/// dsh `validateUpdateArgs`.
pub fn validate_update(args: &Value) -> Option<Value> {
    let allowed = [
        "id",
        "title",
        "prompt",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ];
    let unknown = args
        .as_object()
        .is_some_and(|m| m.keys().any(|k| !allowed.contains(&k.as_str())));
    let selectors = allowed[3..].iter().filter(|k| present(args, k)).count();
    if unknown || selectors > 1 {
        return Some(error(
            "invalid_selector",
            "schedule_update accepts at most one of at, every_seconds, daily, weekly, or cron.",
        ));
    }
    let id = args["id"].as_str().unwrap_or_default();
    if id.is_empty() || id.trim() != id {
        return Some(error(
            "invalid_rule",
            "schedule_update id must be non-empty without surrounding whitespace.",
        ));
    }
    let title = args.get("title").and_then(Value::as_str);
    let prompt = args.get("prompt").and_then(Value::as_str);
    if selectors == 0 && title.is_none() && prompt.is_none() {
        return Some(error(
            "invalid_selector",
            "schedule_update needs a new title, prompt, or one of at, every_seconds, daily, weekly, or cron.",
        ));
    }
    if let Some(e) = title.and_then(title_error) {
        return Some(e);
    }
    if prompt.is_some_and(|p| p.trim().is_empty()) {
        return Some(error(
            "invalid_prompt",
            "prompt must be non-empty after trimming.",
        ));
    }
    invalid_interval(args)
}

#[async_trait]
impl Tool for ScheduleTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema(self.name)
    }

    fn describe(&self, _args: &Value) -> String {
        match self.name {
            "schedule_create" => "Create reminder",
            "schedule_list" => "List reminders",
            "schedule_delete" => "Delete reminder",
            _ => "Update reminder",
        }
        .into()
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let Some(service) = self.service.upgrade() else {
            return Ok(value(internal()));
        };
        // Registered for top-level agents only (dsh `agents.roots()`).
        if ctx.caller.as_ref().is_some_and(|c| !c.root) {
            return Ok(value(internal()));
        }
        let session = ctx.session_id.as_str();
        let out = match self.name {
            "schedule_create" => {
                if let Some(e) = validate_create(&args) {
                    return Ok(value(e));
                }
                if ctx.cancel.is_cancelled() {
                    return Ok(value(internal()));
                }
                match service.create(session, &args).await {
                    Ok(record) => record.view(service.now()),
                    Err(e) => e.to_json(),
                }
            }
            "schedule_list" => {
                if ctx.cancel.is_cancelled() {
                    return Ok(value(internal()));
                }
                Value::Array(service.list_views(session))
            }
            "schedule_delete" => {
                let id = args["id"].as_str().unwrap_or_default();
                if id.is_empty() || id.trim() != id {
                    return Ok(value(error(
                        "invalid_rule",
                        "schedule_delete id must be non-empty without surrounding whitespace.",
                    )));
                }
                if ctx.cancel.is_cancelled() {
                    return Ok(value(internal()));
                }
                service
                    .delete(session, id)
                    .await
                    .unwrap_or_else(|e| e.to_json())
            }
            _ => {
                if let Some(e) = validate_update(&args) {
                    return Ok(value(e));
                }
                if ctx.cancel.is_cancelled() {
                    return Ok(value(internal()));
                }
                service
                    .update_from_args(session, &args)
                    .await
                    .unwrap_or_else(|e| e.to_json())
            }
        };
        Ok(value(out))
    }
}
