//! The real binary against the scripted DeepSeek mock: headless text and JSON
//! output, resuming with --session-id, and the dsh SDK protocol on stdio.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use dshx_testkit::{MockDeepSeek, scenarios};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

async fn mock() -> MockDeepSeek {
    let mock = MockDeepSeek::start().await;
    // `thinking`: two bash steps with signed thinking, then a text answer.
    mock.set_responder(|request| scenarios::reply("thinking", request));
    mock
}

fn command(mock: &MockDeepSeek, home: &std::path::Path, ws: &std::path::Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(ws)
        .env("DSHX_HOME", home)
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .env_remove("DSH_HOME");
    cmd
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_text_json_and_resume() {
    let mock = mock().await;
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();

    let out = command(&mock, home.path(), ws.path())
        .arg("Run the commands.")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer = String::from_utf8_lossy(&out.stdout);
    assert!(!answer.trim().is_empty());
    // Reasoning deltas are streamed to stderr under one heading.
    assert!(String::from_utf8_lossy(&out.stderr).contains("dsh-codex: reasoning:"));

    let out = command(&mock, home.path(), ws.path())
        .args(["--json", "Once more."])
        .output()
        .unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.first().unwrap()["type"], "session");
    assert_eq!(lines.last().unwrap()["type"], "final");
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["type"].as_str()).collect();
    for kind in ["status", "thinking", "tool_call", "tool_result", "text"] {
        assert!(kinds.contains(&kind), "missing {kind} in {kinds:?}");
    }
    let session_id = lines[0]["sessionId"].as_str().unwrap().to_owned();

    let out = command(&mock, home.path(), ws.path())
        .args(["--session-id", &session_id, "--json", "Continue."])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(first["sessionId"], session_id.as_str());

    let out = command(&mock, home.path(), ws.path())
        .args(["--session-id", "session-missing", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sdk_protocol_runs_a_turn() {
    let mock = mock().await;
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let mut child = command(&mock, home.path(), ws.path())
        .args(["--profile", "sdk"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let cwd = ws.path().canonicalize().unwrap().display().to_string();
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"cwd": cwd, "provider": "deepseek-official", "model": "deepseek-flash"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": {"sessionId": "s-1", "contentBlocks": [{"type": "text", "text": "hi"}]}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "session/wait", "params": {"sessionId": "s-1"}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "session/working-directory/get", "params": {"sessionId": "s-1"}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "shutdown"}),
    ];
    // Like an SDK client: send one request, read until its response arrives.
    let mut lines = stdout.lines();
    let mut frames: Vec<Value> = Vec::new();
    for r in &requests {
        writeln!(stdin, "{r}").unwrap();
        stdin.flush().unwrap();
        loop {
            let frame: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            let done = frame["id"] == r["id"];
            frames.push(frame);
            if done {
                break;
            }
        }
    }
    drop(stdin);
    frames.extend(lines.map(|l| serde_json::from_str::<Value>(&l.unwrap()).unwrap()));
    let started = std::time::Instant::now();
    assert!(child.wait().unwrap().success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "slow exit after shutdown"
    );
    let by_id = |id: i64| {
        frames
            .iter()
            .find(|f| f["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no response {id}: {frames:#?}"))
    };
    assert_eq!(
        by_id(1)["result"]["serverInfo"]["name"],
        "deepseek-harness-sdk-runtime"
    );
    assert!(
        by_id(2)["result"]["messageId"].is_string(),
        "prompt: {}",
        by_id(2)
    );
    assert_eq!(by_id(3)["result"], json!({}), "wait: {}", by_id(3));
    assert_eq!(by_id(4)["result"]["cwd"], cwd.as_str());
    assert_eq!(by_id(5)["error"]["code"], -32601);
    assert_eq!(by_id(6)["result"], json!({}));
    let events: Vec<&Value> = frames
        .iter()
        .filter(|f| f["method"] == "session.event")
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["params"]["event"]["type"] == "turn/end")
    );
    assert!(events.iter().all(|e| e["params"]["sessionId"] == "s-1"));
    assert!(
        frames
            .iter()
            .any(|f| f["method"] == "session.status" && f["params"]["status"] == "running")
    );
}
