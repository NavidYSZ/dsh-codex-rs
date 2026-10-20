#![allow(dead_code)]
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolResultPart};
use serde_json::Value;

pub fn text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .filter_map(|p| match p {
            ToolResultPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Runs one call with a fresh call id (same session state).
pub async fn call(
    tool: &dyn Tool,
    ctx: &ToolContext,
    args: Value,
) -> Result<ToolOutput, ToolError> {
    tool.execute(args, dshx_tools::testing::next_call(ctx))
        .await
}

pub async fn ok(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> String {
    match call(tool, ctx, args).await {
        Ok(out) => {
            assert!(!out.is_error);
            text(&out)
        }
        Err(e) => panic!("tool failed: {e}"),
    }
}

pub async fn err(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> String {
    match call(tool, ctx, args).await {
        Ok(out) => panic!("expected an error, got {:?}", text(&out)),
        Err(e) => e.to_string(),
    }
}

pub fn has_bwrap() -> bool {
    std::process::Command::new("bwrap")
        .args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--die-with-parent",
            "--",
            "true",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
