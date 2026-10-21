//! Changing the server set at runtime: `apply` keeps unchanged servers
//! connected, starts added and changed ones, stops removed ones; `restart`
//! reconnects one server.
mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::*;
use dshx_core::ToolResultPart;
use dshx_core::config::McpServerConfig;
use dshx_mcp::McpHandle;
use serde_json::{Value, json};

fn pid(parts: &[ToolResultPart]) -> i64 {
    serde_json::from_str::<Value>(&only_text(parts)).unwrap()["pid"]
        .as_i64()
        .unwrap()
}

fn names(handle: &McpHandle) -> Vec<String> {
    let mut names: Vec<String> = handle
        .tools()
        .iter()
        .map(|t| t.schema().name)
        .filter(|n| n.ends_with("__info"))
        .collect();
    names.sort();
    names
}

const WAIT: Option<Duration> = Some(Duration::from_secs(20));

#[tokio::test]
async fn apply_reconciles_without_restarting_unchanged_servers() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let server = || stdio_server(&[("MCP_TEST_EVENTS", log.to_str().unwrap())]);
    let handle = McpHandle::new(None);
    assert!(
        handle.tools().is_empty(),
        "no resource tools without servers"
    );
    let changes = handle.subscribe();
    let mut servers: BTreeMap<String, McpServerConfig> = [("a".to_owned(), server())].into();
    handle.apply(&servers, WAIT).await.unwrap();
    assert!(changes.has_changed().unwrap());
    assert_eq!(names(&handle), ["mcp__a__info"]);
    assert!(handle.owns_tool("list_mcp_resources"));
    let a_pid = pid(&call(&handle.tools(), "mcp__a__info", json!({}), ctx())
        .await
        .1);

    servers.insert("b".into(), server());
    handle.apply(&servers, WAIT).await.unwrap();
    assert_eq!(names(&handle), ["mcp__a__info", "mcp__b__info"]);
    let tools = handle.tools();
    assert_eq!(
        pid(&call(&tools, "mcp__a__info", json!({}), ctx()).await.1),
        a_pid,
        "a kept its process"
    );
    let b_pid = pid(&call(&tools, "mcp__b__info", json!({}), ctx()).await.1);
    let status = handle.status();
    assert_eq!(status.len(), 2);
    assert!(status.iter().all(|s| s.status == "connected"));
    assert!(status[0].tools.iter().any(|t| t.raw_name == "info"));

    // Changing b restarts it; disabling a stops it; an invalid entry is listed.
    servers.get_mut("b").unwrap().tool_call_timeout_ms = 5_000;
    servers.get_mut("a").unwrap().enabled = false;
    servers.insert("bad name".into(), server());
    handle.apply(&servers, WAIT).await.unwrap();
    assert_eq!(names(&handle), ["mcp__b__info"]);
    assert!(!alive(a_pid));
    let new_b = pid(&call(&handle.tools(), "mcp__b__info", json!({}), ctx())
        .await
        .1);
    assert_ne!(new_b, b_pid);
    assert!(!alive(b_pid));
    let bad = handle
        .status()
        .into_iter()
        .find(|s| s.name == "bad name")
        .unwrap();
    assert_eq!(bad.status, "error");
    assert!(bad.error.unwrap().contains("must match"));

    handle.restart("b", WAIT).await.unwrap();
    let restarted = pid(&call(&handle.tools(), "mcp__b__info", json!({}), ctx())
        .await
        .1);
    assert_ne!(restarted, new_b);
    assert!(handle.restart("a", WAIT).await.is_err());

    handle.apply(&BTreeMap::new(), WAIT).await.unwrap();
    assert!(handle.tools().is_empty());
    assert!(handle.prompt_sections().is_empty());
    assert!(!alive(restarted));
}
