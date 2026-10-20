//! The `todo_write` tool (dsh `tool-todo`, `allowParallelInProgress: true`).
//!
//! The tool only validates and counts; the agent logs `todo/write` with
//! `{todos}` taken from the result's `value`.
use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::util::json_string;

#[derive(Deserialize)]
struct TodoItem {
    content: String,
    status: String,
}

#[derive(Deserialize)]
struct TodoArgs {
    todos: Vec<TodoItem>,
}

/// The `todo_write` tool.
pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("todo_write")
    }

    fn describe(&self, _args: &Value) -> String {
        "Update todo list".into()
    }

    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: TodoArgs = args::parse("todo_write", args)?;
        let mut todos = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in &args.todos {
            let content = item.content.trim();
            if content.is_empty() {
                return Err(fail("invalid todo: `content` must be a non-empty string"));
            }
            if !seen.insert(content.to_owned()) {
                return Err(fail(format!(
                    "invalid todos: duplicate content {}",
                    json_string(content)
                )));
            }
            todos.push(json!({"content": content, "status": item.status}));
        }
        let count = |status: &str| args.todos.iter().filter(|t| t.status == status).count();
        let (pending, in_progress, completed) =
            (count("pending"), count("in_progress"), count("completed"));
        let text = format!(
            "Updated todo list: {pending} pending, {in_progress} in progress, {completed} completed."
        );
        Ok(ToolOutput {
            value: Some(json!({
                "todos": todos,
                "counts": {"pending": pending, "inProgress": in_progress, "completed": completed},
            })),
            ..ToolOutput::text(text)
        })
    }
}
