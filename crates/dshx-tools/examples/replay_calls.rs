//! Parity harness: runs a JSON list of `{"tool", "args"}` calls through the
//! default tools in one session and prints one JSON line per result with the
//! text the model would see (`Error: …` for failures, like dsh).
//!
//! `cargo run -p dshx-tools --example replay_calls -- CALLS.json DIR [MODE]`
use std::path::PathBuf;

use dshx_core::{SandboxMode, ToolResultPart};
use serde_json::{Value, json};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let calls_path = args.next().expect("CALLS.json");
    let dir = PathBuf::from(args.next().expect("DIR"));
    let mode = match args.next().as_deref() {
        Some("read-only") => SandboxMode::ReadOnly,
        Some("danger-full-access") => SandboxMode::DangerFullAccess,
        _ => SandboxMode::WorkspaceWrite,
    };
    let calls: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(calls_path).unwrap()).unwrap();
    let tools = dshx_tools::default_tools(&dshx_core::config::Config::default());
    let ctx = dshx_tools::testing::builder(&dir).sandbox(mode).build();
    for call in calls {
        let name = call["tool"].as_str().unwrap();
        let tool = tools
            .iter()
            .find(|t| t.schema().name == name)
            .expect("known tool");
        let ctx_for_spill = dshx_tools::testing::next_call(&ctx);
        let (text, is_error) = match tool
            .execute(call["args"].clone(), ctx_for_spill.clone())
            .await
        {
            Ok(out) => {
                let text: Vec<String> = out
                    .content
                    .iter()
                    .map(|p| match p {
                        ToolResultPart::Text { text } => text.clone(),
                        ToolResultPart::Image { attachment } => {
                            format!("[image {}]", attachment.media_type)
                        }
                    })
                    .collect();
                let text = text.join("");
                // The agent's spill-policy (dsh `maxInlineTokens: 12500`).
                let text_only = out
                    .content
                    .iter()
                    .all(|p| matches!(p, ToolResultPart::Text { .. }));
                let bounded = if name != "read" && text_only {
                    let state = dshx_tools::ToolState::get(&ctx_for_spill).unwrap();
                    dshx_tools::spill::bound_inline_text(
                        state.spill(),
                        &ctx_for_spill.session_id,
                        name,
                        &text,
                        dshx_tools::spill::DEFAULT_MAX_INLINE_TOKENS,
                    )
                } else {
                    None
                };
                (bounded.unwrap_or(text), out.is_error)
            }
            Err(dshx_core::ToolError::Cancelled) => ("Error: tool call aborted".to_owned(), true),
            Err(error) => (format!("Error: {error}"), true),
        };
        println!(
            "{}",
            json!({"tool": name, "text": text, "isError": is_error})
        );
    }
}
