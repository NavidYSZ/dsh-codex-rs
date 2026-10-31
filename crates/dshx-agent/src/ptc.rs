//! PTC is a presentation of an agent's capabilities, not a second tool registry.
//! The Node worker owns program execution; the agent scheduler owns every nested call.
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent::Inner;
use crate::config::ExtraSection;
use crate::scheduler::PtcDispatcher;

pub(crate) struct RunCodeTool {
    inner: Weak<Inner>,
}

impl RunCodeTool {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self {
            inner: Arc::downgrade(inner),
        }
    }
}

/// Dropping an interrupted transport cannot leave its nested work authorized.
struct ActiveCall {
    inner: Arc<Inner>,
    id: String,
    cancel: CancellationToken,
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.inner.ptc_calls.lock().unwrap().remove(&self.id);
    }
}

#[async_trait]
impl Tool for RunCodeTool {
    fn schema(&self) -> ToolSchema {
        dshx_tools::ptc::schema()
    }

    fn describe(&self, args: &Value) -> String {
        args.get("description")
            .and_then(Value::as_str)
            .unwrap_or("Run TypeScript program")
            .to_owned()
    }

    async fn execute(&self, args: Value, mut ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let inner = self.inner.upgrade().ok_or(ToolError::Cancelled)?;
        let cancel = ctx.cancel.child_token();
        {
            // Same lock order as set_permissions: no gap between policy capture and registration.
            let config = inner.config.lock().unwrap();
            if config.sandbox != ctx.sandbox || cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            inner
                .ptc_calls
                .lock()
                .unwrap()
                .insert(ctx.call_id.clone(), cancel.clone());
        }
        let _active = ActiveCall {
            inner: inner.clone(),
            id: ctx.call_id.clone(),
            cancel: cancel.clone(),
        };
        ctx.cancel = cancel.clone();
        let dispatcher = PtcDispatcher::new(inner, &ctx);
        let result = dshx_tools::ptc::run_code(args, ctx, dispatcher.clone()).await;
        cancel.cancel();
        // Await settlement and audit commits even when code returned without awaiting a call.
        dispatcher.finish().await?;
        result
    }
}

pub(crate) fn sdk_sections(schemas: &[ToolSchema]) -> Vec<ExtraSection> {
    vec![
        ExtraSection {
            name: "tools:ptc-only".into(),
            order: 500,
            literal: true,
            text: "Only run_code is directly callable in this session. All other tool names in the instructions are program-only bindings: invoke them inside run_code as await tools.name(args). Never emit a direct call to a program-only binding.".into(),
        },
        ExtraSection {
            name: "tools:sdk".into(),
            order: 5000,
            literal: true,
            text: sdk(schemas),
        },
    ]
}

fn sdk(schemas: &[ToolSchema]) -> String {
    let mut sorted: Vec<_> = schemas.iter().filter(|s| s.name != "run_code").collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::from(
        r#"## Writing code for run_code

run_code takes code (the body of an async TypeScript function) and description (a short summary). Only erasable TypeScript is accepted: no enum or namespaces. Top-level await and return work. Each call starts a fresh Node process; variables do not persist.

Call tools with await tools.name(args), or tools["my-tool"](args) for exotic names. Arguments must be lossless JSON. Calls return their canonical JSON value; legacy tools without a structured value return {content: [...]}. A failed call throws ToolCallError with toolName and a human-readable message; use try/catch to handle it. Independent read-only calls may overlap with Promise.all; mutating calls run alone, in submission order. Sequence dependent work with await, and await every tool call before returning.

The tools bindings are fixed for one run_code execution. Newly connected or discovered MCP tools and updated schemas become available in the next model round and a new run_code call. Use the declarations in the latest SDK; do not invent tool names or replace an available binding with a shell script or another MCP client.

The optional timeoutMs covers the whole program, including approvals, queued calls and all sequential tool calls. It defaults to 120000 and is capped at 600000. For a known long operation choose a sufficient, bounded budget; split longer workflows into separate runs. A nested tool still has its own timeout. After a timeout inspect completed effects before retrying; programs are never replayed automatically.

Only console output and the returned JSON value enter the conversation. Extract the relevant fields from large results. Successful image results are attached after the run, and deferred tool guidance is preserved. Direct Node operations follow this execution's file sandbox, while nested tools retain their own standing permissions and approval checks.

"#,
    );
    if sorted.iter().any(|schema| schema.name == "browser") {
        out.push_str(
            r#"Browser results are objects with a summary string and optional cursor, place, target, screen and image fields. Read page text or the current element list from result.summary, for example:
```ts
const page = await tools.browser({ action: "read" });
console.log(page.summary);
```
Images are attached automatically; do not print their data.

"#,
        );
    }
    if sorted.iter().any(|schema| schema.name.starts_with("mcp__")) {
        out.push_str(
            r#"MCP bindings return {content: [...], structuredContent?: ...}. Use structuredContent when present; otherwise inspect the content blocks. Text blocks contain strings, which may be ordinary prose or JSON according to that tool's contract. Parse a text block only when its format is known to be JSON; do not assume every text result is JSON or that it has a summary field. For a result already returned by an MCP binding:
```ts
if (result.structuredContent !== undefined) {
  console.log(result.structuredContent);
} else {
  for (const block of result.content ?? []) {
    if (block.type === "text") console.log(block.text);
  }
}
```
MCP failures throw ToolCallError, including a server result with isError: true.

"#,
        );
    }
    out.push_str(
        "Program-only SDK bindings:\n\n```ts\ntype JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };\ndeclare class ToolCallError extends Error { readonly name: \"ToolCallError\"; readonly toolName: string; }\ndeclare const tools: {\n",
    );
    for schema in sorted {
        for line in schema.description.replace("*/", "* /").lines() {
            out.push_str("  // ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("  ");
        out.push_str(&quoted(&schema.name));
        out.push_str(": (args: ");
        out.push_str(&schema_type(&schema.parameters, 0));
        out.push_str(") => Promise<JsonValue>;\n");
    }
    out.push_str("};\n```");
    out
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// Schemas remain the authority. Unsupported JSON Schema constructs render as unknown.
fn schema_type(schema: &Value, depth: usize) -> String {
    if depth > 24 {
        return "unknown".into();
    }
    if schema == &Value::Bool(false) {
        return "never".into();
    }
    if let Some(value) = schema.get("const") {
        return literal(value);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return if values.is_empty() {
            "never".into()
        } else {
            values.iter().map(literal).collect::<Vec<_>>().join(" | ")
        };
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(values) = schema.get(key).and_then(Value::as_array) {
            return values
                .iter()
                .map(|v| format!("({})", schema_type(v, depth + 1)))
                .collect::<Vec<_>>()
                .join(" | ");
        }
    }
    if let Some(types) = schema.get("type").and_then(Value::as_array) {
        return types
            .iter()
            .map(|ty| {
                let mut branch = schema.clone();
                branch["type"] = ty.clone();
                schema_type(&branch, depth + 1)
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("integer" | "number") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => "null".into(),
        Some("array") => format!(
            "Array<{}>",
            schema
                .get("items")
                .map(|s| schema_type(s, depth + 1))
                .unwrap_or_else(|| "unknown".into())
        ),
        Some("object") => {
            let required = schema.get("required").and_then(Value::as_array);
            let mut fields = Vec::new();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                let mut names: Vec<_> = properties.keys().collect();
                names.sort();
                for name in names {
                    let optional =
                        !required.is_some_and(|r| r.iter().any(|v| v.as_str() == Some(name)));
                    let description = properties[name]
                        .get("description")
                        .and_then(Value::as_str)
                        .map(|text| format!("/* {} */ ", text.replace("*/", "* /")))
                        .unwrap_or_default();
                    fields.push(format!(
                        "{}{}{}: {}",
                        description,
                        quoted(name),
                        if optional { "?" } else { "" },
                        schema_type(&properties[name], depth + 1)
                    ));
                }
            }
            if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
                fields.push("[key: string]: unknown".into());
            }
            format!("{{ {} }}", fields.join("; "))
        }
        _ => "unknown".into(),
    }
}

fn literal(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
        _ => "JsonValue".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sdk_is_stable_literal_and_preserves_argument_contracts() {
        let make = |name: &str| ToolSchema {
            name: name.into(),
            description: "Use {{literal}} safely".into(),
            parameters: json!({"type":"object","properties":{"choice":{"enum":["one","two"]},"count":{"type":"integer"}},"required":["choice"],"additionalProperties":false}),
        };
        let a = make("z-tool");
        let b = make("a");
        assert_eq!(sdk(&[a.clone(), b.clone()]), sdk(&[b, a]));
        let text = sdk(&[make("mcp__server__odd-name")]);
        assert!(text.contains("\"mcp__server__odd-name\""));
        assert!(text.contains("\"choice\": \"one\" | \"two\""));
        assert!(text.contains("\"count\"?: number"));
        assert!(text.contains("{{literal}}"));
        assert!(sdk_sections(&[]).iter().all(|s| s.literal));
    }

    #[test]
    fn sdk_explains_available_result_shapes_and_execution_boundaries() {
        let tool = |name: &str| ToolSchema {
            name: name.into(),
            description: String::new(),
            parameters: json!({"type": "object"}),
        };
        let text = sdk(&[tool("browser"), tool("mcp__fixture__read")]);
        for guidance in [
            "page.summary",
            "result.structuredContent",
            "block.type === \"text\"",
            "Parse a text block only when its format is known to be JSON",
            "fixed for one run_code execution",
            "next model round and a new run_code call",
            "including approvals, queued calls and all sequential tool calls",
            "defaults to 120000 and is capped at 600000",
            "inspect completed effects before retrying",
        ] {
            assert!(text.contains(guidance), "missing {guidance}");
        }
        let without_browser = sdk(&[tool("read")]);
        assert!(!without_browser.contains("tools.browser("));
        assert!(!without_browser.contains("MCP bindings return"));
    }
}
