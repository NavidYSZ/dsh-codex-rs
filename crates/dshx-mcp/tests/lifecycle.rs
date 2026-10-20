//! Connection lifecycle against the fixture server: reconnect after a crash,
//! giving up, process-tree shutdown, timeouts and cancellation, tool-list
//! changes, server-to-client requests, images for text-only models, startup
//! policy and the plain-HTTP refusal.
mod common;

use std::time::Duration;

use common::*;
use dshx_core::ToolResultPart;
use serde_json::{Value, json};

fn info(parts: &[ToolResultPart]) -> Value {
    serde_json::from_str(&only_text(parts)).unwrap()
}

#[tokio::test]
async fn reconnects_after_the_server_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let server = quick_reconnect(
        stdio_server(&[("MCP_TEST_EVENTS", log.to_str().unwrap())]),
        3,
    );
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("r", server)]))
        .await
        .unwrap();
    let first = info(&call(&tools, "mcp__r__info", json!({}), ctx()).await.1)["pid"].clone();

    let mut changes = mcp.subscribe();
    let (is_error, parts) = call(&tools, "mcp__r__crash", json!({}), ctx()).await;
    assert!(is_error);
    assert_eq!(only_text(&parts), "Error: Connection closed");
    // The supervisor starts a new process and republishes the tools.
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let (is_error, parts) = call(&tools, "mcp__r__info", json!({}), ctx()).await;
    assert!(!is_error, "{parts:?}");
    let second = info(&parts)["pid"].clone();
    assert_ne!(first, second);
    assert!(!alive(first.as_i64().unwrap()));
    let starts = events(&log)
        .iter()
        .filter(|e| e["event"] == "start")
        .count();
    assert_eq!(starts, 2);
    mcp.shutdown().await;
    assert!(!alive(second.as_i64().unwrap()));
}

#[tokio::test]
async fn gives_up_after_the_attempt_budget_and_removes_tools() {
    let mut server = quick_reconnect(stdio_server(&[]), 2);
    server.args = vec!["-c".into(), "import sys; sys.exit(1)".into()];
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("dead", server.clone())]))
        .await
        .unwrap();
    // Only the resource tools; the server never connected.
    let names: Vec<_> = tools.iter().map(|t| t.schema().name).collect();
    assert_eq!(
        names,
        [
            "list_mcp_resources",
            "list_mcp_resource_templates",
            "read_mcp_resource"
        ]
    );
    let (is_error, parts) = call(
        &tools,
        "list_mcp_resources",
        json!({"server": "dead"}),
        ctx(),
    )
    .await;
    assert!(is_error);
    assert_eq!(
        only_text(&parts),
        "Error: mcp-client(dead): server is disconnected"
    );
    mcp.shutdown().await;

    server.fail_on_startup_error = true;
    let err = dshx_mcp::mcp_tools(&config(vec![("dead", server)]))
        .await
        .err()
        .unwrap();
    assert!(
        err.to_string()
            .starts_with("mcp-client(dead): initial connection or tool synchronization failed"),
        "{err}"
    );
}

#[tokio::test]
async fn shutdown_terminates_the_whole_process_tree() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let server = stdio_server(&[("MCP_TEST_EVENTS", log.to_str().unwrap())]);
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("p", server)]))
        .await
        .unwrap();
    let pid = info(&call(&tools, "mcp__p__info", json!({}), ctx()).await.1)["pid"]
        .as_i64()
        .unwrap();
    let grandchild: i64 = only_text(
        &call(&tools, "mcp__p__spawn_child", json!({}), ctx())
            .await
            .1,
    )
    .parse()
    .unwrap();
    assert!(alive(pid) && alive(grandchild));
    mcp.shutdown().await;
    assert!(!alive(pid), "server still running");
    wait_for("the grandchild to be reaped", || !alive(grandchild)).await;
    // The server saw its stdin close first (the polite stop).
    assert!(events(&log).iter().any(|e| e["event"] == "stdin-closed"));
    // Calls after shutdown fail cleanly.
    let (is_error, parts) = call(&tools, "mcp__p__echo", json!({"text": "x"}), ctx()).await;
    assert!(is_error);
    assert_eq!(only_text(&parts), "Error: Not connected");
}

#[tokio::test]
async fn timeouts_and_cancellation_notify_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let mut server = stdio_server(&[("MCP_TEST_EVENTS", log.to_str().unwrap())]);
    server.tool_call_timeout_ms = 300;
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("s", server)]))
        .await
        .unwrap();

    let (is_error, parts) = call(&tools, "mcp__s__slow", json!({"ms": 5000}), ctx()).await;
    assert!(is_error);
    assert_eq!(only_text(&parts), "Error: Request timed out");
    wait_for("cancel after timeout", || {
        events(&log)
            .iter()
            .filter(|e| e["event"] == "slow-cancelled")
            .count()
            == 1
    })
    .await;

    let ctx = ctx();
    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let result = find(&tools, "mcp__s__slow")
        .execute(json!({"ms": 5000}), ctx)
        .await;
    assert!(matches!(result, Err(dshx_core::ToolError::Cancelled)));
    wait_for("cancel after abort", || {
        events(&log)
            .iter()
            .filter(|e| e["event"] == "slow-cancelled")
            .count()
            == 2
    })
    .await;
    // The connection is still usable.
    let (is_error, parts) = call(
        &tools,
        "mcp__s__echo",
        json!({"text": "ok"}),
        dshx_tools::testing::context(dir.path()),
    )
    .await;
    assert!(!is_error);
    assert_eq!(only_text(&parts), "ok");
    mcp.shutdown().await;
}

#[tokio::test]
async fn server_requests_list_changes_and_names() {
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("t", stdio_server(&[]))]))
        .await
        .unwrap();
    // The server pings the client in the middle of a call.
    let (_, parts) = call(&tools, "mcp__t__ping_client", json!({}), ctx()).await;
    assert_eq!(only_text(&parts), "client answered: {}");
    // Lossy names carry a hash; the raw name goes to the server.
    let (_, parts) = call(
        &tools,
        "mcp__t__weird_name_with_dots_10f7f669ff8b",
        json!({}),
        ctx(),
    )
    .await;
    assert_eq!(only_text(&parts), "weird ok");
    // Non-object arguments become {}.
    let (_, parts) = call(&tools, "mcp__t__ordered", json!("x"), ctx()).await;
    assert_eq!(only_text(&parts), "ordered {}");
    // An unknown content block fails the call.
    let (is_error, parts) = call(&tools, "mcp__t__unknown_block", json!({}), ctx()).await;
    assert!(is_error);
    assert_eq!(
        only_text(&parts),
        "Error: Invalid result for tools/call: content[1].type: unsupported content type \"hologram\""
    );
    let mut changes = mcp.subscribe();
    call(&tools, "mcp__t__change", json!({}), ctx()).await;
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(mcp.owns_tool("mcp__t__added"));
    // A stale tool object whose tool disappeared reports it like dsh.
    mcp.shutdown().await;
}

#[tokio::test]
async fn images_for_text_only_models_become_diagnostics() {
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("t", stdio_server(&[]))]))
        .await
        .unwrap();
    let state = std::sync::Arc::new(dshx_tools::ToolState::new());
    state.set_model("deepseek-v4-pro");
    let ctx = dshx_tools::testing::builder(std::path::Path::new("."))
        .images(false)
        .state(state)
        .build();
    let (is_error, parts) = call(&tools, "mcp__t__image", json!({}), ctx).await;
    assert!(!is_error);
    assert_eq!(
        parts,
        vec![
            text("caption before"),
            text(
                "[image unavailable: image/png; model \"deepseek-v4-pro\" does not declare image input; raw image data remains available to programmatic callers]"
            ),
            text("caption after"),
        ]
    );
    mcp.shutdown().await;
}

#[tokio::test]
async fn invalid_entries_are_skipped_or_fatal() {
    let remote = dshx_core::config::McpServerConfig {
        url: Some("http://example.com/mcp".into()),
        ..Default::default()
    };
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("web", remote.clone())]))
        .await
        .unwrap();
    assert!(tools.is_empty());
    assert!(mcp.prompt_sections().is_empty());
    let strict = dshx_core::config::McpServerConfig {
        fail_on_startup_error: true,
        ..remote
    };
    let err = dshx_mcp::mcp_tools(&config(vec![("web", strict)]))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("refusing plain http:// to the remote host example.com"),
        "{err}"
    );
    // Disabled servers are ignored entirely.
    let disabled = dshx_core::config::McpServerConfig {
        enabled: false,
        ..stdio_server(&[])
    };
    let (tools, _) = dshx_mcp::mcp_tools(&config(vec![("off", disabled)]))
        .await
        .unwrap();
    assert!(tools.is_empty());
}
