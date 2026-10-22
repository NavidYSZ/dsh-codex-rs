//! Skills and MCP servers on demand, against the mock model (wire dump):
//! a plain prompt carries no skill catalog, no `mcp__` tool and no server
//! instructions; `@t` adds exactly the server's tools as deferred
//! `tool_addition`s plus a note with its instructions; `/demo` injects the
//! skill; switched-off tools never appear; a deactivated server's declared
//! tools answer with an error. Also the composer routes: suggestions,
//! chat MCP state, `/compact`, the export and the settings routes.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply, scenarios};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

fn fixture_server() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dshx-mcp/tests/fixtures/mcp_server.py")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Web {
    child: Child,
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Web {
    fn start(home: &Path, others: &Path, mock: &MockDeepSeek) -> Web {
        let mut child = Command::new(BIN)
            .args(["web", "--port", "0", "--no-open"])
            .env("DSHX_HOME", home)
            .env("DSH_HOME", others.join("dsh"))
            .env("DSH_AGENTS_HOME", others.join("agents"))
            .env("DEEPSEEK_API_KEY", "sk-mock")
            .env("DEEPSEEK_BASE_URL", mock.base_url())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let url = loop {
            let line = lines.next().expect("web server line").unwrap();
            if let Some(url) = line.strip_prefix("dsh-codex web: ") {
                break url.to_owned();
            }
        };
        std::thread::spawn(move || for _ in lines {});
        let (base, token) = url.split_once("/?token=").unwrap();
        Web {
            child,
            base: base.to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    async fn raw(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, String) {
        let mut request = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.text().await.unwrap())
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, text) = self.raw(method.clone(), path, body).await;
        assert!(status < 300, "{method} {path}: {status} {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    async fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path, None).await
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn turns(mock: &MockDeepSeek) -> Vec<RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|r| !scenarios::is_title_request(r))
        .collect()
}

fn tool_names(r: &RecordedRequest) -> Vec<String> {
    r.body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Every text block and the system prompt of a request.
fn all_text(r: &RecordedRequest) -> String {
    let mut out = r.system().unwrap_or_default().to_owned();
    for m in r.messages() {
        for b in m["content"].as_array().into_iter().flatten() {
            if let Some(t) = b["text"].as_str() {
                out.push_str(t);
                out.push('\n');
            }
            for c in b["content"].as_array().into_iter().flatten() {
                if let Some(t) = c["text"].as_str() {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Tool names of the `tool_addition` blocks in a request's history.
fn additions(r: &RecordedRequest) -> Vec<String> {
    r.messages()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "tool_addition")
        .map(|b| b["tool"]["name"].as_str().unwrap().to_owned())
        .collect()
}

fn last_user_text(r: &RecordedRequest) -> String {
    r.messages()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

async fn wait_for(mock: &MockDeepSeek, count: usize) -> Vec<RecordedRequest> {
    for _ in 0..200 {
        let now = turns(mock);
        if now.len() >= count {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the model got {} requests, not {count}", turns(mock).len());
}

async fn idle(web: &Web, session: &str) {
    for _ in 0..200 {
        if web.get(&format!("/sessions/{session}")).await["status"] == "idle" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("session stayed busy");
}

/// Sends a prompt and returns the first request it caused.
async fn prompt(web: &Web, mock: &MockDeepSeek, session: &str, text: &str) -> RecordedRequest {
    let before = turns(mock).len();
    web.call(
        reqwest::Method::POST,
        &format!("/sessions/{session}/prompt"),
        Some(json!({"text": text})),
    )
    .await;
    let requests = wait_for(mock, before + 1).await;
    idle(web, session).await;
    requests[before].clone()
}

fn responder(r: &RecordedRequest) -> Reply {
    if scenarios::is_title_request(r) {
        return Reply::text("Testtitel");
    }
    if all_text(r).contains("You are now acting as a compaction engine") {
        return Reply::text(
            "## Primary Request and Intent\n- test the composer\n\n## Next Step\n- (none)",
        );
    }
    let user = last_user_text(r);
    if user.contains("call echo") && r.tool_results() == 0 {
        return Reply::stream()
            .tool_call("toolu_echo", "mcp__t__echo", json!({"text": "hi"}))
            .into();
    }
    Reply::text("ok")
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_and_mcp_join_a_chat_only_on_demand() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(responder);
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join(".git")).unwrap();
    write(
        &ws.path().join(".dsh/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill for the on-demand test.\n---\n\nSay DEMO when asked.\n",
    );
    write(&ws.path().join("src/main.rs"), "fn main() {}\n");
    write(
        &home.path().join("config.toml"),
        &format!(
            "[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n\n\
             [mcp_servers.t]\ncommand = \"python3\"\nargs = [{}]\ndisabled_tools = [\"fail\"]\nenv = {{ MCP_TEST_INSTRUCTIONS = \"Use the fixture tools carefully.\" }}\n",
            serde_json::to_string(&fixture_server()).unwrap()
        ),
    );
    let web = Web::start(home.path(), others.path(), &mock);
    let cwd = ws.path().canonicalize().unwrap().display().to_string();
    let session = web
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": cwd})),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The server connects in the background; wait for its tools.
    for _ in 0..100 {
        let servers = web.get("/mcp/servers").await;
        if servers[0]["status"] == "connected" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A long prompt so that /compact later has something to condense.
    let long = format!("hello {}", "lorem ipsum dolor sit amet ".repeat(400));
    let first = prompt(&web, &mock, &session, &long).await;
    let names = tool_names(&first);
    assert!(!names.iter().any(|n| n.starts_with("mcp__")), "{names:?}");
    assert!(
        !names.iter().any(|n| n.contains("mcp_resource")),
        "{names:?}"
    );
    assert!(!names.contains(&"skill".to_owned()), "{names:?}");
    let text = all_text(&first);
    assert!(!text.contains("<available_skills>"), "{text}");
    assert!(!text.contains("MCP server") && !text.contains("fixture tools carefully"));
    // dsh's file-reference guidance is part of the system prompt.
    assert!(
        first
            .system()
            .unwrap()
            .contains("Tokens prefixed with @ are paths")
    );

    // The menus.
    let slash = web
        .get(&format!(
            "/composer/suggest?kind=slash&q=&session={session}"
        ))
        .await;
    let commands: Vec<&str> = slash["sections"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    for c in ["compact", "model", "permission", "status", "view", "export"] {
        assert!(commands.contains(&c), "{c}: {commands:?}");
    }
    let skills = web
        .get(&format!(
            "/composer/suggest?kind=slash&q=dem&session={session}"
        ))
        .await;
    let skill = &skills["sections"][0]["items"][0];
    assert_eq!(skills["sections"][0]["id"], "skills");
    assert_eq!(skill["name"], "demo");
    assert_eq!(skill["insert"], "/demo ");
    let mention = web
        .get(&format!(
            "/composer/suggest?kind=mention&q=t&session={session}&model=deepseek-flash"
        ))
        .await;
    assert_eq!(mention["cacheMiss"], false);
    let server = &mention["sections"][0]["items"][0];
    assert_eq!(server["name"], "t");
    assert_eq!(server["status"], "connected");
    assert_eq!(server["active"], false);
    let files = web
        .get(&format!(
            "/composer/suggest?kind=mention&q=main&session={session}"
        ))
        .await;
    assert_eq!(files["sections"][1]["items"][0]["insert"], "@src/main.rs");
    let pro = web
        .get(&format!(
            "/composer/suggest?kind=mention&q=t&session={session}&model=deepseek-v4-pro"
        ))
        .await;
    assert_eq!(pro["cacheMiss"], true);

    // @t switches the server on at this point of the history.
    let second = prompt(&web, &mock, &session, "use @t please").await;
    let names = tool_names(&second);
    let mcp: Vec<&String> = names.iter().filter(|n| n.starts_with("mcp__")).collect();
    assert!(mcp.contains(&&"mcp__t__echo".to_owned()), "{names:?}");
    assert!(!names.contains(&"mcp__t__fail".to_owned()), "{names:?}");
    for tool in second.body["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let deferred = tool["defer_loading"] == true;
        let bridged = name.starts_with("mcp__") || name.contains("mcp_resource");
        assert_eq!(deferred, bridged, "{name}");
    }
    let mut added = additions(&second);
    added.sort();
    let mut expected: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("mcp__") || n.contains("mcp_resource"))
        .cloned()
        .collect();
    expected.sort();
    assert_eq!(added, expected);
    assert_eq!(
        second.header("anthropic-beta"),
        Some("mid-conversation-tool-changes-2026-07-01")
    );
    let text = all_text(&second);
    assert!(
        text.contains("The user turned on the MCP server \"t\""),
        "{text}"
    );
    assert!(text.contains("Use the fixture tools carefully."));
    assert!(!second.system().unwrap().contains("fixture tools carefully"));
    let active = web.get(&format!("/sessions/{session}/mcp")).await;
    assert_eq!(active["active"], json!(["t"]));

    // /demo loads the skill's instructions into this turn only.
    let third = prompt(&web, &mock, &session, "/demo do it").await;
    let text = all_text(&third);
    assert!(text.contains("<skill_content name=\"demo\">"), "{text}");
    assert!(!text.contains("<available_skills>"));
    assert!(!tool_names(&third).contains(&"skill".to_owned()));

    // Off again: on the addition-only route the tools stay declared, but a
    // call fails, and the model was told.
    let off = web
        .call(
            reqwest::Method::DELETE,
            &format!("/sessions/{session}/mcp/t"),
            None,
        )
        .await;
    assert_eq!(off["active"], json!([]));
    let before = turns(&mock).len();
    prompt(&web, &mock, &session, "call echo").await;
    let requests = wait_for(&mock, before + 2).await;
    let fourth = &requests[before];
    assert!(tool_names(fourth).contains(&"mcp__t__echo".to_owned()));
    assert!(all_text(fourth).contains("The user turned off the MCP server \"t\""));
    let result = requests[before + 1]
        .messages()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .find(|b| b["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["is_error"], true);
    assert!(
        result.to_string().contains("turned off the MCP server"),
        "{result}"
    );

    // Settings: the model may load the skill on its own now.
    let row = web
        .call(
            reqwest::Method::PUT,
            "/skills/demo/settings",
            Some(json!({"model": true, "cwd": cwd})),
        )
        .await;
    assert_eq!(row["name"], "demo");
    assert_eq!(row["model"], true);
    assert_eq!(row["enabled"], true);
    let fifth = prompt(&web, &mock, &session, "anything").await;
    assert!(tool_names(&fifth).contains(&"skill".to_owned()));
    assert!(all_text(&fifth).contains("<available_skills>"));
    let server = web
        .call(
            reqwest::Method::PUT,
            "/mcp/servers/t/settings",
            Some(json!({"always": true, "disabledTools": ["fail", "slow"]})),
        )
        .await;
    assert_eq!(server["always"], true);
    assert_eq!(server["disabledTools"], json!(["fail", "slow"]));
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(config.contains("always = true"), "{config}");
    assert!(config.contains("[skills.entries.demo]"), "{config}");

    // /compact condenses the history between turns.
    let compacted = web
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{session}/compact"),
            None,
        )
        .await;
    assert_eq!(compacted["compacted"], true, "{compacted}");
    assert!(compacted["items"].as_u64().unwrap() > 0);

    // The log export: the header line, then one line per event.
    let (status, jsonl) = web
        .raw(
            reqwest::Method::GET,
            &format!("/sessions/{session}/export"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let lines: Vec<Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["id"], session);
    assert!(lines.iter().any(|l| l["type"] == "compaction/end"));
    assert!(
        lines
            .iter()
            .any(|l| l["data"]["source"]["kind"] == "mcp-activation")
    );

    // An uploaded file is stored once, read-only, and named by path.
    let (status, saved) = web
        .raw(reqwest::Method::POST, "/files?name=../notes.txt", None)
        .await;
    assert_eq!(status, 200, "{saved}");
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["name"], "notes.txt");
    assert!(Path::new(saved["path"].as_str().unwrap()).is_file());
}
