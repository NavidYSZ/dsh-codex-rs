//! Bridged MCP tools (dsh `mcp-client/tools.ts`): one [`McpTool`] per server
//! tool under its public name; results become text in block order, images
//! are stored as attachments when the model accepts them, and an MCP
//! `isError` result is a failed call.
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use dshx_core::{NewImage, Tool, ToolContext, ToolError, ToolOutput, ToolResultPart, ToolSchema};
use serde_json::{Map, Value, json};

use crate::error::McpError;
use crate::protocol;
use crate::server::{Server, ToolEntry};

const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// One MCP server tool. Calls go to the server's current connection by the
/// raw tool name, so the object stays valid across reconnects.
pub struct McpTool {
    server: Arc<Server>,
    entry: ToolEntry,
}

impl McpTool {
    pub(crate) fn new(server: Arc<Server>, entry: ToolEntry) -> Self {
        McpTool { server, entry }
    }

    /// The server's own name for this tool (sent in `tools/call`).
    pub fn raw_name(&self) -> &str {
        &self.entry.raw.name
    }

    pub fn server_name(&self) -> &str {
        self.server.name()
    }
}

fn failed(e: McpError) -> ToolError {
    match e {
        McpError::Cancelled => ToolError::Cancelled,
        other => ToolError::Failed(other.to_string()),
    }
}

#[async_trait]
impl Tool for McpTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.entry.public_name.clone(),
            description: self.entry.raw.description.clone(),
            parameters: self.entry.raw.input_schema.clone(),
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let raw_name = self.entry.raw.name.clone();
        // The current generation decides what the tool is now.
        let Some(current) = self
            .server
            .tools()
            .into_iter()
            .find(|t| t.raw.name == raw_name)
        else {
            return Err(ToolError::Failed(format!(
                "unknown tool \"{}\"",
                self.entry.public_name
            )));
        };
        if current.raw.task_required {
            return Err(ToolError::Failed(format!(
                "Tool \"{raw_name}\" requires task-based execution, which this bridge does not support"
            )));
        }
        // Non-object arguments become {} so the server reports what is missing.
        let arguments = if args.is_object() { args } else { json!({}) };
        let peer = self
            .server
            .peer()
            .ok_or_else(|| failed(McpError::NotConnected))?;
        let result = peer
            .request(
                "tools/call",
                Some(json!({"name": raw_name, "arguments": arguments})),
                self.server.spec.call_timeout,
                Some(&ctx.cancel),
            )
            .await
            .map_err(failed)?;
        let result = protocol::call_result(&result)
            .map_err(|e| ToolError::Failed(format!("Invalid result for tools/call: {e}")))?;
        if current.raw.has_output_schema && result.structured.is_none() && !result.is_error {
            return Err(ToolError::Failed(format!(
                "Tool {raw_name} has an output schema but did not return structured content"
            )));
        }
        if result.is_error {
            return Err(ToolError::Failed(extract_text(&result.content, &raw_name)));
        }
        let mut value = Map::new();
        value.insert("content".into(), Value::Array(result.content.clone()));
        if let Some(structured) = &result.structured {
            value.insert("structuredContent".into(), structured.clone());
        }
        let content = if result
            .content
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("image"))
        {
            image_projection(&ctx, &result.content, &raw_name).await
        } else {
            vec![ToolResultPart::Text {
                text: extract_text(&result.content, &raw_name),
            }]
        };
        Ok(ToolOutput {
            content,
            is_error: false,
            value: Some(Value::Object(value)),
            meta: None,
            contexts: Vec::new(),
        })
    }
}

/// Diagnostic for an image that was not admitted to model context.
fn image_diagnostic(block: &Value, reason: &str) -> String {
    let media_type = block
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("unknown media type");
    format!(
        "[image unavailable: {media_type}; {reason}; raw image data remains available to programmatic callers]"
    )
}

/// Ordered projection: text-like runs joined with `\n`, each image a part of
/// its own (`image(block, index)`), never empty.
fn project(
    content: &[Value],
    tool_name: &str,
    mut image: impl FnMut(&Value, usize) -> ToolResultPart,
) -> Vec<ToolResultPart> {
    let mut parts = Vec::new();
    let mut run: Vec<String> = Vec::new();
    let flush = |run: &mut Vec<String>, parts: &mut Vec<ToolResultPart>| {
        if !run.is_empty() {
            parts.push(ToolResultPart::Text {
                text: std::mem::take(run).join("\n"),
            });
        }
    };
    for (index, block) in content.iter().enumerate() {
        let field = |key: &str| block.get(key).and_then(Value::as_str);
        match field("type") {
            Some("text") => {
                if let Some(text) = field("text") {
                    run.push(text.to_owned());
                }
            }
            Some("image") => {
                flush(&mut run, &mut parts);
                parts.push(image(block, index));
            }
            Some("resource_link") => match (field("name"), field("uri")) {
                (Some(name), Some(uri)) => run.push(format!("Resource link: {name} ({uri})")),
                _ => run.push(
                    "[resource link unavailable: the MCP block is missing its name or URI]".into(),
                ),
            },
            Some("audio") => run.push(format!(
                "[audio result unsupported: {}; raw audio data remains available to programmatic callers]",
                field("mimeType").unwrap_or("unknown media type")
            )),
            Some("resource") => run.push(
                "[embedded resource unsupported; raw resource data remains available to programmatic callers]".into(),
            ),
            Some(other) => run.push(format!("[unsupported MCP content type: {other}]")),
            None if !block.is_object() => {
                run.push("[unsupported MCP content block: expected an object]".into())
            }
            None => run.push("[unsupported MCP content type: undefined]".into()),
        }
    }
    flush(&mut run, &mut parts);
    if parts.is_empty() {
        parts.push(ToolResultPart::Text {
            text: format!("({tool_name} returned no model-visible content)"),
        });
    }
    parts
}

/// All content as one text (images as diagnostics), the text dsh shows for
/// plain results and errors.
pub(crate) fn extract_text(content: &[Value], tool_name: &str) -> String {
    project(content, tool_name, |block, _| ToolResultPart::Text {
        text: image_diagnostic(
            block,
            "this result was not admitted to durable model context",
        ),
    })
    .into_iter()
    .map(|part| match part {
        ToolResultPart::Text { text } => text,
        ToolResultPart::Image { .. } => String::new(),
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Canonical RFC 4648 base64 only (no whitespace, URL alphabet or missing padding).
fn decode_canonical(data: &str) -> Option<Vec<u8>> {
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(data).ok()?;
    (engine.encode(&bytes) == data).then_some(bytes)
}

async fn image_projection(
    ctx: &ToolContext,
    content: &[Value],
    tool_name: &str,
) -> Vec<ToolResultPart> {
    let mut decoded: Vec<(usize, Vec<u8>, String)> = Vec::new();
    let mut invalid: Vec<(usize, &'static str)> = Vec::new();
    for (index, block) in content.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let media_type = block
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !IMAGE_MEDIA_TYPES.contains(&media_type) {
            invalid.push((
                index,
                "the declared media type is not PNG, JPEG, WebP, or GIF",
            ));
            continue;
        }
        match block
            .get("data")
            .and_then(Value::as_str)
            .and_then(decode_canonical)
        {
            Some(bytes) => decoded.push((index, bytes, media_type.to_owned())),
            None => invalid.push((index, "the image data is not canonical base64")),
        }
    }
    let refuse = |reason: String| {
        project(content, tool_name, |block, _| ToolResultPart::Text {
            text: image_diagnostic(block, &reason),
        })
    };
    if !invalid.is_empty() {
        return project(content, tool_name, |block, index| {
            let reason = invalid
                .iter()
                .find(|(i, _)| *i == index)
                .map_or("another image in the same result was invalid", |(_, r)| r);
            ToolResultPart::Text {
                text: image_diagnostic(block, reason),
            }
        });
    }
    if !ctx.model_accepts_images {
        let model = ctx
            .state
            .downcast_ref::<dshx_tools::ToolState>()
            .and_then(dshx_tools::ToolState::model);
        return refuse(match model {
            Some(model) => format!("model \"{model}\" does not declare image input"),
            None => "the current model route could not be resolved".into(),
        });
    }
    if ctx.cancel.is_cancelled() {
        return refuse("the tool call was canceled before image storage".into());
    }
    let mut refs = Vec::new();
    for (index, data, media_type) in decoded {
        let saved = ctx
            .attachments
            .save_image(NewImage {
                data,
                media_type,
                name: None,
                width: 0,
                height: 0,
            })
            .await;
        match saved {
            Ok(reference) => refs.push((index, reference)),
            Err(e) => {
                tracing::warn!("MCP image could not be stored: {e:#}");
                return refuse("durable image storage rejected the result".into());
            }
        }
    }
    project(content, tool_name, |_, index| {
        let attachment = refs
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, r)| r.clone())
            .expect("every image was stored");
        ToolResultPart::Image { attachment }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_projection_matches_dsh() {
        let content = vec![
            json!({"type": "text", "text": "first"}),
            json!({"type": "text", "text": "second"}),
            json!({"type": "resource_link", "uri": "test://readme", "name": "readme"}),
            json!({"type": "audio", "data": "AAAA", "mimeType": "audio/wav"}),
            json!({"type": "resource", "resource": {"uri": "test://readme", "text": "inline"}}),
        ];
        assert_eq!(
            extract_text(&content, "mixed"),
            "first\nsecond\nResource link: readme (test://readme)\n[audio result unsupported: audio/wav; raw audio data remains available to programmatic callers]\n[embedded resource unsupported; raw resource data remains available to programmatic callers]"
        );
        assert_eq!(
            extract_text(&[], "empty"),
            "(empty returned no model-visible content)"
        );
        assert_eq!(
            extract_text(
                &[json!({"type": "image", "data": "x", "mimeType": "image/png"})],
                "i"
            ),
            "[image unavailable: image/png; this result was not admitted to durable model context; raw image data remains available to programmatic callers]"
        );
    }

    #[test]
    fn canonical_base64() {
        assert!(decode_canonical("AAEC").is_some());
        assert!(decode_canonical("AAE").is_none());
        assert!(decode_canonical("AA EC").is_none());
        assert!(decode_canonical("AA-_").is_none());
    }
}
