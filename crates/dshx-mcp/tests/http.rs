//! Streamable HTTP against the fixture server in JSON and SSE reply modes:
//! headers, session handling, server notifications inside SSE replies,
//! session expiry with re-initialization, and session release on shutdown.
mod common;

use std::time::Duration;

use common::*;
use dshx_core::config::McpServerConfig;
use serde_json::json;

fn http_server(url: &str) -> McpServerConfig {
    let mut server = McpServerConfig {
        url: Some(url.into()),
        headers: [("Authorization".to_owned(), "Bearer test-token".to_owned())].into(),
        ..Default::default()
    };
    server.reconnect.initial_delay_ms = 20;
    server.reconnect.max_delay_ms = 100;
    server
}

#[tokio::test]
async fn json_replies_headers_and_session_release() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let fixture = HttpFixture::start(false, &log).await;
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("h", http_server(&fixture.url))]))
        .await
        .unwrap();
    let (is_error, parts) = call(&tools, "mcp__h__echo", json!({"text": "over http"}), ctx()).await;
    assert!(!is_error);
    assert_eq!(only_text(&parts), "over http");
    let (_, parts) = call(&tools, "list_mcp_resources", json!({"server": "h"}), ctx()).await;
    assert_eq!(
        only_text(&parts),
        "MCP server: h\n{\"resources\":[{\"name\":\"readme\",\"uri\":\"test://readme\",\"mimeType\":\"text/plain\"},{\"name\":\"blob\",\"uri\":\"test://blob\",\"mimeType\":\"application/octet-stream\"}]}"
    );
    let sections = mcp.prompt_sections();
    assert_eq!(sections[1].name, "mcp:h");
    assert_eq!(sections[1].text, "### MCP server: h\n\nHTTP fixture.");
    mcp.shutdown().await;

    let posts: Vec<_> = events(&log)
        .into_iter()
        .filter(|e| e["event"] == "post")
        .collect();
    assert_eq!(posts[0]["method"], "initialize");
    assert_eq!(posts[0]["session"], serde_json::Value::Null);
    for post in &posts {
        assert_eq!(post["auth"], "Bearer test-token");
        assert_eq!(post["accept"], "application/json, text/event-stream");
    }
    for post in &posts[1..] {
        assert_eq!(post["session"], "sess-1");
        assert_eq!(post["version"], "2025-11-25");
    }
    let client = events(&log)
        .into_iter()
        .find(|e| e["event"] == "client")
        .unwrap();
    assert_eq!(client["clientInfo"]["name"], "dsh-codex");
    let deletes: Vec<_> = events(&log)
        .into_iter()
        .filter(|e| e["event"] == "delete")
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0]["session"], "sess-1");
}

#[tokio::test]
async fn sse_replies_carry_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let fixture = HttpFixture::start(true, &log).await;
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("h", http_server(&fixture.url))]))
        .await
        .unwrap();
    let (_, parts) = call(&tools, "mcp__h__mixed", json!({}), ctx()).await;
    assert!(only_text(&parts).starts_with("first\nsecond\nResource link: readme (test://readme)"));
    let mut changes = mcp.subscribe();
    let (_, parts) = call(&tools, "mcp__h__change", json!({}), ctx()).await;
    assert_eq!(only_text(&parts), "changed");
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let tools = mcp.tools();
    let (_, parts) = call(&tools, "mcp__h__added", json!({}), ctx()).await;
    assert_eq!(only_text(&parts), "added works");
    mcp.shutdown().await;
}

#[tokio::test]
async fn expired_sessions_reinitialize() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let fixture = HttpFixture::start(false, &log).await;
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("h", http_server(&fixture.url))]))
        .await
        .unwrap();
    let mut changes = mcp.subscribe();
    call(&tools, "mcp__h__expire", json!({}), ctx()).await;
    // The next request meets a 404 for its session: the call fails and the
    // supervisor opens a new session.
    let (is_error, parts) = call(&tools, "mcp__h__echo", json!({"text": "a"}), ctx()).await;
    assert!(is_error);
    assert_eq!(only_text(&parts), "Error: Connection closed");
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let (is_error, parts) = call(&tools, "mcp__h__echo", json!({"text": "b"}), ctx()).await;
    assert!(!is_error, "{parts:?}");
    assert_eq!(only_text(&parts), "b");
    let inits = events(&log)
        .into_iter()
        .filter(|e| e["event"] == "post" && e["method"] == "initialize")
        .count();
    assert_eq!(inits, 2);
    mcp.shutdown().await;
}

#[tokio::test]
async fn unreachable_server_keeps_retrying_in_the_background() {
    // Nothing listens on this port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/mcp",
        listener.local_addr().unwrap().port()
    );
    drop(listener);
    let mut server = http_server(&url);
    server.reconnect.max_attempts = 1;
    let (tools, mcp) = dshx_mcp::mcp_tools(&config(vec![("h", server.clone())]))
        .await
        .unwrap();
    assert_eq!(tools.len(), 3, "resource tools only");
    mcp.shutdown().await;
    server.fail_on_startup_error = true;
    let err = dshx_mcp::mcp_tools(&config(vec![("h", server)]))
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("HTTP request failed"), "{err}");
}
