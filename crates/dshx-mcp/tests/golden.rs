//! dshx-mcp against what the official dsh 0.2.0-rc.2 sent to the model for
//! the same fixture server (`tests/golden/dsh-0.2.0-rc.2.json`, captured
//! with `dsh --profile headless --patch <mcp-client row>` and a scripted mock
//! model): tool declarations, prompt sections and every tool result.
mod common;

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use common::*;
use dshx_core::{AttachmentStore, ToolResultPart};
use serde_json::Value;

fn golden() -> Value {
    serde_json::from_str(include_str!("golden/dsh-0.2.0-rc.2.json")).unwrap()
}

#[tokio::test]
async fn declarations_prompt_and_results_match_dsh() {
    let dir = tempfile::tempdir().unwrap();
    let events = dir.path().join("events.jsonl");
    let config = config(vec![(
        "t",
        stdio_server(&[
            (
                "MCP_TEST_INSTRUCTIONS",
                "Use the fixture tools carefully.\n  ",
            ),
            ("MCP_TEST_VAR", "from-config"),
            ("MCP_TEST_EVENTS", events.to_str().unwrap()),
        ]),
    )]);
    let (mut tools, mcp) = dshx_mcp::mcp_tools(&config).await.unwrap();
    let g = golden();

    // Declarations: name, description and the argument schema byte for byte.
    for expected in g["tools"].as_array().unwrap() {
        let name = expected["name"].as_str().unwrap();
        if expected.get("defer_loading").is_some() {
            continue; // added later through tools/list_changed, checked below
        }
        let ours = find(&tools, name).schema();
        assert_eq!(ours.description, expected["description"], "{name}");
        assert_eq!(
            serde_json::to_string(&ours.parameters).unwrap(),
            serde_json::to_string(&expected["input_schema"]).unwrap(),
            "{name}"
        );
    }

    // System prompt sections, joined like the agent renders them.
    let prompt: Vec<String> = mcp
        .prompt_sections()
        .into_iter()
        .map(|s| s.text)
        .filter(|t| !t.is_empty())
        .collect();
    assert_eq!(prompt.join("\n\n"), g["prompt_tail"].as_str().unwrap());

    let store = Arc::new(dshx_tools::testing::MemoryAttachments::default());
    let mut changes = mcp.subscribe();
    for case in g["results"].as_array().unwrap() {
        let name = case["tool"].as_str().unwrap();
        if name == "mcp__t__added" {
            // `change` announced the new tool; the handle publishes it.
            tokio::time::timeout(std::time::Duration::from_secs(10), changes.changed())
                .await
                .unwrap()
                .unwrap();
            tools = mcp.tools();
            let added = find(&tools, name).schema();
            let expected = g["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            assert_eq!(added.description, expected["description"]);
        }
        let ctx = dshx_tools::testing::builder(Path::new("."))
            .attachments(store.clone())
            .build();
        if name == "mcp__t__change" {
            changes.borrow_and_update();
        }
        let (is_error, parts) = call(&tools, name, case["input"].clone(), ctx).await;
        assert_eq!(is_error, case["is_error"].as_bool().unwrap(), "{case}");
        // The DeepSeek adapter drops empty text blocks.
        let parts: Vec<_> = parts
            .into_iter()
            .filter(|p| !matches!(p, ToolResultPart::Text { text } if text.is_empty()))
            .collect();
        let expected = case["content"].as_array().unwrap();
        assert_eq!(parts.len(), expected.len(), "{case}: {parts:?}");
        for (ours, want) in parts.iter().zip(expected) {
            match (ours, want["type"].as_str().unwrap()) {
                (ToolResultPart::Text { text }, "text") if name == "mcp__t__info" => {
                    let info: Value = serde_json::from_str(text).unwrap();
                    assert_eq!(info["env"]["MCP_TEST_VAR"], "from-config");
                    assert_eq!(info["env"]["FAKE_API_KEY"], Value::Null);
                }
                (ToolResultPart::Text { text }, "text") => {
                    assert_eq!(text, want["text"].as_str().unwrap(), "{case}")
                }
                (ToolResultPart::Image { attachment }, "image") => {
                    assert_eq!(attachment.media_type, want["media_type"]);
                    assert_eq!((attachment.width, attachment.height), (1, 1));
                    let (bytes, _) = store.load(&attachment.attachment_id).await.unwrap();
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(want["data"].as_str().unwrap())
                        .unwrap();
                    assert_eq!(bytes, data);
                }
                (ours, _) => panic!("{case}: unexpected part {ours:?}"),
            }
        }
    }
    mcp.shutdown().await;
}
