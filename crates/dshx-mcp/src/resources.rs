//! The shared resource tools (dsh `mcp-resources`): `list_mcp_resources`,
//! `list_mcp_resource_templates`, `read_mcp_resource`, plus the prompt
//! section naming the servers they accept.
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Map, Value, json};

use crate::error::McpError;
use crate::protocol;
use crate::server::{Server, list_all};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    List,
    Templates,
    Read,
}

impl Kind {
    fn method(self) -> &'static str {
        match self {
            Kind::List => "resources/list",
            Kind::Templates => "resources/templates/list",
            Kind::Read => "resources/read",
        }
    }
}

/// One of the three resource tools over every configured server.
pub struct ResourceTool {
    kind: Kind,
    servers: Arc<RwLock<Vec<Arc<Server>>>>,
}

pub(crate) fn resource_tools(servers: Arc<RwLock<Vec<Arc<Server>>>>) -> Vec<Arc<dyn Tool>> {
    [Kind::List, Kind::Templates, Kind::Read]
        .into_iter()
        .map(|kind| {
            Arc::new(ResourceTool {
                kind,
                servers: servers.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

/// The `mcp-resource-servers` prompt section text (empty without servers).
pub(crate) fn servers_section(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    format!(
        "## MCP resource servers\n\nUse list_mcp_resources, list_mcp_resource_templates, or read_mcp_resource with one of these names as the server argument: {}.",
        serde_json::to_string(&sorted).unwrap_or_default()
    )
}

fn server_param() -> Value {
    json!({"type": "string", "description": "Configured MCP server name."})
}

#[async_trait]
impl Tool for ResourceTool {
    fn schema(&self) -> ToolSchema {
        let cursor = json!({"type": "string", "description": "Continuation cursor returned by this server."});
        let (name, description, parameters) = match self.kind {
            Kind::List => (
                "list_mcp_resources",
                "List resources available from an MCP server.",
                json!({"type": "object", "properties": {"server": server_param(), "cursor": cursor}, "required": ["server"]}),
            ),
            Kind::Templates => (
                "list_mcp_resource_templates",
                "List parameterized resource URI templates from an MCP server.",
                json!({"type": "object", "properties": {"server": server_param(), "cursor": cursor}, "required": ["server"]}),
            ),
            Kind::Read => (
                "read_mcp_resource",
                "Read an MCP resource by URI from the named server. Use a listed URI or an expanded resource template.",
                json!({"type": "object", "properties": {"server": server_param(), "uri": {"type": "string", "description": "Resource URI to read."}}, "required": ["server", "uri"]}),
            ),
        };
        ToolSchema {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let violations = dshx_tools::validate_args(&self.schema().parameters, &args, "");
        if !violations.is_empty() {
            return Err(ToolError::InvalidArguments(format!(
                "invalid arguments: {}",
                violations.join("; ")
            )));
        }
        let name = args["server"].as_str().unwrap_or_default();
        let server = self
            .servers
            .read()
            .unwrap()
            .iter()
            .find(|s| s.name() == name)
            .cloned()
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "MCP resource server \"{name}\" is unavailable in this agent's scope"
                ))
            })?;
        let value = self
            .request(&server, &args, &ctx)
            .await
            .map_err(|e| match e {
                McpError::Cancelled => ToolError::Cancelled,
                other => ToolError::Failed(other.to_string()),
            })?;
        Ok(ToolOutput {
            value: Some(Value::Object(value.clone())),
            ..ToolOutput::text(render(name, &Value::Object(value)))
        })
    }
}

impl ResourceTool {
    async fn request(
        &self,
        server: &Server,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<Map<String, Value>, McpError> {
        let peer = server.peer().ok_or_else(|| {
            McpError::Transport(format!("{}: server is disconnected", server.spec.label()))
        })?;
        let timeout = server.spec.call_timeout;
        let method = self.kind.method();
        let invalid = |e: String| McpError::Protocol(format!("Invalid result for {method}: {e}"));
        if self.kind == Kind::Read {
            let uri = args["uri"].as_str().unwrap_or_default();
            let result = peer
                .request(
                    method,
                    Some(json!({"uri": uri})),
                    timeout,
                    Some(&ctx.cancel),
                )
                .await?;
            return protocol::read_result(&result).map_err(invalid);
        }
        let key = protocol::list_key(method);
        if !server.has_capability("resources") {
            let mut empty = Map::new();
            empty.insert(key.into(), json!([]));
            return Ok(empty);
        }
        if let Some(cursor) = args.get("cursor").and_then(Value::as_str) {
            // An explicit cursor fetches exactly one raw page.
            let page = peer
                .request(
                    method,
                    Some(json!({"cursor": cursor})),
                    timeout,
                    Some(&ctx.cancel),
                )
                .await?;
            return protocol::resource_list(method, &page).map_err(invalid);
        }
        let mut pages = list_all(&peer, method, timeout, Some(&ctx.cancel))
            .await?
            .into_iter();
        let first = pages.next().unwrap_or_else(|| json!({key: []}));
        let mut aggregate = protocol::resource_list(method, &first).map_err(invalid)?;
        for page in pages {
            let page = protocol::resource_list(method, &page).map_err(invalid)?;
            if let (Some(Value::Array(all)), Some(Value::Array(more))) =
                (aggregate.get_mut(key), page.get(key))
            {
                all.extend(more.iter().cloned());
            }
        }
        aggregate.remove("nextCursor");
        Ok(aggregate)
    }
}

/// `MCP server: <name>\n<JSON>`, with every `blob` string replaced by its size.
pub(crate) fn render(server: &str, value: &Value) -> String {
    format!(
        "MCP server: {server}\n{}",
        serde_json::to_string(&js_json(value, None)).unwrap_or_default()
    )
}

/// Applies dsh's `blob` replacer and prints numbers the way JavaScript does.
fn js_json(value: &Value, key: Option<&str>) -> Value {
    match value {
        Value::String(s) if key == Some("blob") => Value::String(format!(
            "[binary resource: {} base64 characters; available to programmatic callers]",
            s.encode_utf16().count()
        )),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 => {
                json!(f as i64)
            }
            _ => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(|v| js_json(v, None)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), js_json(v, Some(k))))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_replaces_blobs_and_prints_js_numbers() {
        let v = json!({"contents": [{"uri": "u", "blob": "AAECAwQF"}], "n": 1.0, "f": 0.5});
        assert_eq!(
            render("t", &v),
            "MCP server: t\n{\"contents\":[{\"uri\":\"u\",\"blob\":\"[binary resource: 8 base64 characters; available to programmatic callers]\"}],\"n\":1,\"f\":0.5}"
        );
    }

    #[test]
    fn servers_section_lists_sorted_names() {
        assert_eq!(servers_section(&[]), "");
        assert_eq!(
            servers_section(&["web".into(), "t".into()]),
            "## MCP resource servers\n\nUse list_mcp_resources, list_mcp_resource_templates, or read_mcp_resource with one of these names as the server argument: [\"t\",\"web\"]."
        );
    }
}
