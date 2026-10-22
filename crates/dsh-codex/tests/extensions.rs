//! The Plugins page routes of `dsh-codex web` against the real runtime: a
//! local marketplace, installing and toggling a plugin with a skill and an
//! MCP server, adding an MCP server, the connector catalog, and an open
//! session that sees the new tools and skills on its next prompt without a
//! process restart.
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

/// A Claude marketplace with one plugin: a skill and a stdio MCP server.
fn marketplace(root: &Path) {
    write(
        &root.join(".claude-plugin/marketplace.json"),
        &json!({"name": "team-tools", "plugins": [
            {"name": "notes", "source": "./plugins/notes", "description": "Notes helpers."}]})
        .to_string(),
    );
    let plugin = root.join("plugins/notes");
    write(
        &plugin.join(".claude-plugin/plugin.json"),
        &json!({"name": "notes", "version": "0.1.0", "description": "Notes helpers."}).to_string(),
    );
    write(
        &plugin.join("skills/note-taking/SKILL.md"),
        "---\nname: note-taking\ndescription: Take tidy notes.\n---\n\nWrite notes.\n",
    );
    write(
        &plugin.join(".mcp.json"),
        &json!({"mcpServers": {"fixture": {"command": "python3", "args": [fixture_server()]}}})
            .to_string(),
    );
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
        // Keep draining stderr so the server never blocks on it.
        std::thread::spawn(move || for _ in lines {});
        let (base, token) = url.split_once("/?token=").unwrap();
        Web {
            child,
            base: base.to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, body) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {body}");
        body
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, body) = self.call(reqwest::Method::POST, path, Some(body)).await;
        assert_eq!(status, 200, "POST {path}: {body}");
        body
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

fn tool_names(r: &RecordedRequest) -> Vec<String> {
    r.body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn prompt(web: &Web, mock: &MockDeepSeek, session: &str, text: &str) -> RecordedRequest {
    let before = turns(mock).len();
    let (status, body) = web
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{session}/prompt"),
            Some(json!({"text": text})),
        )
        .await;
    assert_eq!(status, 202, "{body}");
    for _ in 0..200 {
        let now = turns(mock);
        if now.len() > before {
            return now.last().unwrap().clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the model never got the prompt");
}

fn turns(mock: &MockDeepSeek) -> Vec<RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|r| !scenarios::is_title_request(r))
        .collect()
}

/// Waits until the session's turn ended.
async fn idle(web: &Web, session: &str) {
    for _ in 0..200 {
        if web.get(&format!("/sessions/{session}")).await["status"] == "idle" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("session stayed busy");
}

fn all_text(r: &RecordedRequest) -> String {
    r.body["messages"].to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_and_mcp_servers_change_without_a_restart() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|request| {
        if scenarios::is_title_request(request) {
            Reply::text("Titel")
        } else {
            Reply::text("ok")
        }
    });
    let home = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let market = tempfile::tempdir().unwrap();
    marketplace(market.path());
    let web = Web::start(home.path(), others.path(), &mock);

    let info = web.get("/info").await;
    assert_eq!(info["extensions"]["plugins"], 0);
    assert_eq!(info["extensions"]["mcpServers"], 0);

    let added = web
        .post(
            "/marketplaces",
            json!({"source": market.path().to_str().unwrap()}),
        )
        .await;
    assert_eq!(added["name"], "team-tools");
    assert_eq!(added["plugins"][0]["id"], "notes@team-tools");
    assert_eq!(added["plugins"][0]["installed"], false);

    // A session opened before any extension exists.
    let session = web
        .post("/sessions", json!({"cwd": ws.path().to_str().unwrap()}))
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = prompt(&web, &mock, &session, "hello").await;
    assert!(!tool_names(&first).iter().any(|n| n.starts_with("mcp__")));
    assert!(!all_text(&first).contains("note-taking"));
    idle(&web, &session).await;

    let plugin = web
        .post(
            "/plugins",
            json!({"marketplace": "team-tools", "name": "notes"}),
        )
        .await;
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["skills"][0]["name"], "note-taking");
    assert_eq!(plugin["mcpServers"][0]["name"], "notes_fixture");
    assert_eq!(plugin["mcpServers"][0]["status"], "connected");
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        config.contains("[plugins.\"notes@team-tools\"]"),
        "{config}"
    );

    let added = web
        .post(
            "/mcp/servers",
            json!({"name": "t", "config": {"command": "python3", "args": [fixture_server()]}}),
        )
        .await;
    assert_eq!(added["status"], "connected");
    assert!(
        added["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "mcp__t__echo")
    );

    let servers = web.get("/mcp/servers").await;
    let names: Vec<&str> = servers
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["t", "notes_fixture"]);
    assert_eq!(servers[1]["origin"], "plugin");
    assert_eq!(servers[1]["plugin"], "notes@team-tools");

    let skills = web
        .get(&format!("/skills?cwd={}", ws.path().to_str().unwrap()))
        .await;
    let note = skills
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "note-taking")
        .unwrap();
    assert_eq!(note["source"], "plugin");
    assert_eq!(note["plugin"], "notes@team-tools");
    assert_eq!(note["active"], true);

    // The open session gets the new tools once the prompt names them (`@t`,
    // and `@notes` for the plugin's server), the skill through `/name`.
    let second = prompt(&web, &mock, &session, "again @t @notes /note-taking").await;
    let tools = tool_names(&second);
    assert!(tools.contains(&"mcp__t__echo".to_owned()), "{tools:?}");
    assert!(
        tools.contains(&"mcp__notes_fixture__echo".to_owned()),
        "{tools:?}"
    );
    assert!(all_text(&second).contains("note-taking"));
    idle(&web, &session).await;

    // Disabling the plugin takes its server and skill away again.
    let disabled = web
        .post("/plugins/notes@team-tools/disable", json!({}))
        .await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["mcpServers"][0]["status"], "disabled");
    let servers = web.get("/mcp/servers").await;
    assert_eq!(servers.as_array().unwrap().len(), 1);
    let skills = web.get("/skills").await;
    let note = skills
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "note-taking")
        .unwrap();
    assert_eq!(note["active"], false);
    let third = prompt(&web, &mock, &session, "once more").await;
    assert!(!tool_names(&third).contains(&"mcp__notes_fixture__echo".to_owned()));
    idle(&web, &session).await;

    let disabled = web.post("/mcp/servers/t/disable", json!({})).await;
    assert_eq!(disabled["status"], "disabled");
    let (status, _) = web
        .call(reqwest::Method::DELETE, "/mcp/servers/t", None)
        .await;
    assert_eq!(status, 204);
    let (status, _) = web
        .call(reqwest::Method::DELETE, "/plugins/notes@team-tools", None)
        .await;
    assert_eq!(status, 204);
    let (status, _) = web
        .call(reqwest::Method::GET, "/plugins/notes@team-tools", None)
        .await;
    assert_eq!(status, 404);

    // The connector catalog; logins need OAuth-capable HTTP servers.
    let connectors = web.get("/connectors").await;
    assert_eq!(connectors.as_array().unwrap().len(), 15);
    assert_eq!(connectors[0]["added"], false);
    let (status, body) = web
        .call(
            reqwest::Method::POST,
            "/mcp/servers/missing/login",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 404, "{body}");

    let info = web.get("/info").await;
    assert_eq!(info["extensions"]["mcpServers"], 0);
    assert_eq!(info["extensions"]["marketplaces"], 1);
}
