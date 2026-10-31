//! "Test connection" ([`dshx_mcp::probe`]) and the per-server log of a
//! running handle.
mod common;

use std::time::Duration;

use common::*;
use dshx_core::config::McpServerConfig;

fn shell(script: &str) -> McpServerConfig {
    McpServerConfig {
        command: Some("sh".into()),
        args: vec!["-c".into(), script.into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn probe_lists_the_tools_and_stops_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let events_file = dir.path().join("events.jsonl");
    let config = stdio_server(&[("MCP_TEST_EVENTS", events_file.to_str().unwrap())]);
    let probe = dshx_mcp::probe("fixture", &config, None, Duration::from_secs(20)).await;
    assert!(probe.ok, "{probe:?}");
    assert_eq!(probe.error, None);
    assert!(!probe.needs_login);
    let echo = probe.tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(echo.description, "Echo the given text back.");
    // The throwaway connection is gone: the server saw its stdin close.
    wait_for("the probed server to see its stdin close", || {
        events(&events_file)
            .iter()
            .any(|e| e["event"] == "stdin-closed")
    })
    .await;
}

#[tokio::test]
async fn probe_reports_the_error_and_stderr_of_a_failing_server() {
    let config = shell("echo 'Error: the API token is missing' >&2; exit 3");
    let probe = dshx_mcp::probe("broken", &config, None, Duration::from_secs(10)).await;
    assert!(!probe.ok);
    assert!(probe.tools.is_empty());
    let error = probe.error.unwrap();
    // The child can exit before either side of the initialization exchange:
    // the reader sees EOF, or the writer reaches the already closed pipe.
    assert!(
        error.contains("exited") || error.contains("closed") || error.contains("Broken pipe"),
        "{error}"
    );
    assert_eq!(
        probe.stderr,
        vec!["Error: the API token is missing".to_owned()]
    );
}

#[tokio::test]
async fn probe_gives_up_after_the_timeout() {
    // Reads requests but never answers.
    let config = shell("echo waiting >&2; cat > /dev/null");
    let started = std::time::Instant::now();
    let probe = dshx_mcp::probe("silent", &config, None, Duration::from_millis(600)).await;
    assert!(!probe.ok);
    assert!(probe.error.unwrap().contains("no answer"));
    // (The shell may add a line about its killed child.)
    assert_eq!(probe.stderr.first().map(String::as_str), Some("waiting"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn probe_rejects_an_invalid_config() {
    let probe = dshx_mcp::probe("bad name", &shell("true"), None, Duration::from_secs(5)).await;
    assert!(!probe.ok);
    assert!(probe.error.unwrap().contains("server name"));
    let missing = dshx_mcp::probe(
        "nothing",
        &McpServerConfig::default(),
        None,
        Duration::from_secs(5),
    )
    .await;
    assert!(missing.error.unwrap().contains("set command"));
    let unknown = McpServerConfig {
        command: Some("/nonexistent/mcp-server".into()),
        ..Default::default()
    };
    let spawn = dshx_mcp::probe("ghost", &unknown, None, Duration::from_secs(5)).await;
    assert!(spawn.error.unwrap().contains("spawn"));
}

#[tokio::test]
async fn the_handle_keeps_stderr_and_connection_events_per_server() {
    let noisy = shell(&format!(
        "echo 'booting noisy' >&2; exec python3 '{}'",
        fixture().display()
    ));
    let broken = quick_reconnect(shell("echo 'cannot start' >&2; exit 1"), 1);
    let config = config(vec![("noisy", noisy), ("broken", broken)]);
    let (_, mcp) = dshx_mcp::mcp_tools(&config).await.unwrap();
    wait_for("the noisy server's stderr", || {
        mcp.log("noisy").iter().any(|l| l.text == "booting noisy")
    })
    .await;
    let noisy = mcp.log("noisy");
    assert!(
        noisy
            .iter()
            .any(|l| l.kind == "info" && l.text.starts_with("starting sh -c"))
    );
    assert!(
        noisy
            .iter()
            .any(|l| l.kind == "info" && l.text.starts_with("connected, "))
    );
    let line = noisy.iter().find(|l| l.text == "booting noisy").unwrap();
    assert_eq!(line.kind, "stderr");
    assert!(line.time > 1_600_000_000_000);
    let (_, schema) = mcp
        .tool_schemas("noisy")
        .into_iter()
        .find(|(n, _)| n == "echo")
        .unwrap();
    assert_eq!(schema["required"][0], "text");

    wait_for("the broken server to give up", || {
        mcp.log("broken")
            .iter()
            .any(|l| l.text.starts_with("gave up"))
    })
    .await;
    let broken = mcp.log("broken");
    assert!(
        broken
            .iter()
            .any(|l| l.kind == "stderr" && l.text == "cannot start")
    );
    assert!(
        broken
            .iter()
            .any(|l| l.kind == "error" && l.text.starts_with("connection failed"))
    );

    // A restart keeps the history; removing the server drops it.
    mcp.restart("noisy", Some(Duration::from_secs(20)))
        .await
        .unwrap();
    let after = mcp.log("noisy");
    assert!(after.iter().any(|l| l.text == "restarted"));
    assert!(after.iter().any(|l| l.text == "booting noisy"));
    mcp.apply(&Default::default(), None).await.unwrap();
    assert!(mcp.log("noisy").is_empty());
    mcp.shutdown().await;
}
