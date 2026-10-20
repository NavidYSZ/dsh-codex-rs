//! The binary with a project skill and the MCP fixture server
//! (`crates/dshx-mcp/tests/fixtures/mcp_server.py`) against the mock model,
//! compared with what the official dsh 0.2.0-rc.2 sent in the same setup
//! (`tests/golden/mcp-skills-dsh-0.2.0-rc.2.json`): tool declarations,
//! prompt sections, the skill catalog message and the results of a `skill`
//! and an `mcp__` call.
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply, scenarios};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

fn golden() -> Value {
    serde_json::from_str(include_str!("golden/mcp-skills-dsh-0.2.0-rc.2.json")).unwrap()
}

fn fixture_server() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dshx-mcp/tests/fixtures/mcp_server.py")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

fn reply(request: &RecordedRequest) -> Reply {
    if scenarios::is_title_request(request) {
        return Reply::text("Testtitel");
    }
    if request.tool_results() == 0 {
        Reply::stream()
            .tool_call("toolu_skill", "skill", json!({"name": "demo"}))
            .tool_call("toolu_echo", "mcp__t__echo", json!({"text": "hi"}))
            .into()
    } else {
        Reply::text("FERTIG")
    }
}

fn texts(blocks: &Value) -> Vec<Value> {
    blocks
        .as_array()
        .unwrap()
        .iter()
        .map(|b| json!({"type": b["type"], "text": b["text"]}))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_and_mcp_reach_the_model_like_dsh() {
    let g = golden();
    let mock = MockDeepSeek::start().await;
    mock.set_responder(reply);
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    let events = home.path().join("mcp-events.jsonl");
    std::fs::create_dir_all(ws.path().join(".git")).unwrap();
    let skill = ws.path().join(".dsh/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill for the integration test.\n---\n\nSay DEMO when asked.\n",
    )
    .unwrap();
    let config = format!(
        "[mcp_servers.t]\ncommand = \"python3\"\nargs = [{}]\nenv = {{ MCP_TEST_INSTRUCTIONS = \"Use the fixture tools carefully.\", MCP_TEST_EVENTS = {} }}\n",
        toml_string(&fixture_server()),
        toml_string(&events.display().to_string()),
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();

    let out = Command::new(BIN)
        .current_dir(ws.path())
        .env("DSHX_HOME", home.path())
        // User-level skill roots of the test machine stay out of the catalog.
        .env("DSH_HOME", others.path().join("dsh"))
        .env("DSH_AGENTS_HOME", others.path().join("agents"))
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .arg(g["prompt"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "FERTIG");

    let ws_path = ws.path().canonicalize().unwrap().display().to_string();
    let expand = |v: &Value| -> Value {
        serde_json::from_str(&v.to_string().replace("{WS}", &ws_path)).unwrap()
    };
    let requests: Vec<RecordedRequest> = mock
        .requests()
        .into_iter()
        .filter(|r| !scenarios::is_title_request(r))
        .collect();
    assert_eq!(requests.len(), 2);
    let first = &requests[0].body;

    // Declarations of skill, the resource tools and every mcp__ tool, byte for byte.
    let ours: Vec<&Value> = first["tools"].as_array().unwrap().iter().collect();
    for expected in g["tools"].as_array().unwrap() {
        let name = expected["name"].as_str().unwrap();
        let tool = ours
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(tool.to_string(), expected.to_string(), "{name}");
    }
    // Tools both harnesses offer appear in the same order.
    let names: Vec<&str> = ours.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let dsh_order: Vec<&str> = g["tool_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .filter(|n| names.contains(n))
        .collect();
    let our_order: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| dsh_order.contains(n))
        .collect();
    assert_eq!(our_order, dsh_order);
    assert!(dsh_order.len() > 20, "{dsh_order:?}");

    // The MCP prompt sections close the system prompt like in dsh.
    let tail = expand(&g["system_tail"]);
    assert!(
        first["system"]
            .as_str()
            .unwrap()
            .ends_with(tail.as_str().unwrap()),
        "{}",
        first["system"]
    );

    // Prompt, runtime context, then the skill catalog.
    let user = &first["messages"][0];
    assert_eq!(user["role"], "user");
    assert_eq!(
        Value::Array(texts(&user["content"])),
        expand(&g["first_user_blocks"])
    );

    // The skill and mcp__ results.
    let second = &requests[1].body;
    let results: Vec<Value> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "tool_result")
        .map(|b| json!({"is_error": b["is_error"], "content": texts(&b["content"])}))
        .collect();
    assert_eq!(Value::Array(results), expand(&g["tool_results"]));

    // The run stopped the MCP server (stdin closed, then the process exited).
    let log = std::fs::read_to_string(&events).unwrap();
    assert!(log.contains("\"stdin-closed\""), "{log}");
}

fn toml_string(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// An SDK session keeps its agent between prompts; a tool the MCP server
/// announced through `tools/list_changed` is offered from the next turn on.
#[tokio::test(flavor = "multi_thread")]
async fn sdk_sessions_pick_up_mcp_tool_changes() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|request| {
        if scenarios::is_title_request(request) {
            Reply::text("Testtitel")
        } else if request.tool_results() == 0 {
            Reply::stream()
                .tool_call("toolu_change", "mcp__t__change", json!({}))
                .into()
        } else {
            Reply::text("ok")
        }
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[mcp_servers.t]\ncommand = \"python3\"\nargs = [{}]\n",
            toml_string(&fixture_server())
        ),
    )
    .unwrap();
    let mut child = Command::new(BIN)
        .current_dir(ws.path())
        .env("DSHX_HOME", home.path())
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .args(["--profile", "sdk"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let cwd = ws.path().canonicalize().unwrap().display().to_string();
    let prompt = |id: i64, text: &str| json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": {"sessionId": "s-1", "contentBlocks": [{"type": "text", "text": text}]}});
    let wait = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "session/wait", "params": {"sessionId": "s-1"}});
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"cwd": cwd, "provider": "deepseek-official", "model": "deepseek-flash"}}),
        prompt(2, "change the tools"),
        wait(3),
        prompt(4, "again"),
        wait(5),
        json!({"jsonrpc": "2.0", "id": 6, "method": "shutdown"}),
    ];
    for r in &requests {
        writeln!(stdin, "{r}").unwrap();
        stdin.flush().unwrap();
        loop {
            let frame: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            assert!(frame.get("error").is_none(), "{frame}");
            if frame["id"] == r["id"] {
                break;
            }
        }
        if r["id"] == 3 {
            // The re-sync after tools/list_changed runs beside the turn.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let tools = |r: &RecordedRequest| -> Vec<String> {
        r.body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let requests: Vec<RecordedRequest> = mock
        .requests()
        .into_iter()
        .filter(|r| !scenarios::is_title_request(r))
        .collect();
    assert_eq!(requests.len(), 3);
    assert!(!tools(&requests[0]).contains(&"mcp__t__added".to_owned()));
    assert!(tools(&requests[2]).contains(&"mcp__t__added".to_owned()));
}
