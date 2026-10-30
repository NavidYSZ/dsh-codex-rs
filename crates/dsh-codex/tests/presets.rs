//! Agent presets end to end: a Minimal run sends the request the official
//! dsh 0.2.0-rc.2 sent for its Minimal composition
//! (`docs/rust/upstream/wire-minimal-0.2.0-rc.2/001.json`) and keeps its shell
//! between calls; Standard requests stay exactly as they were; the web API
//! creates, lists and describes presets.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply, scenarios};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

fn golden_wire() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/wire-minimal-0.2.0-rc.2/001.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Config that keeps the host's browser and Mac tools out of the requests.
fn quiet_home(extra: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "{extra}[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n"
        ),
    )
    .unwrap();
    home
}

fn headless(mock: &MockDeepSeek, home: &Path, ws: &Path, others: &Path, args: &[&str]) {
    let out = Command::new(BIN)
        .current_dir(ws)
        .env("DSHX_HOME", home)
        // User-level skill and instruction roots of the test machine stay out.
        .env("DSH_HOME", others.join("dsh"))
        .env("DSH_AGENTS_HOME", others.join("agents"))
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "FERTIG");
}

fn main_requests(mock: &MockDeepSeek) -> Vec<RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|r| !scenarios::is_title_request(r))
        .collect()
}

fn tool_results(request: &RecordedRequest) -> Vec<String> {
    request
        .messages()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "tool_result")
        .map(|b| match &b["content"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
            other => other.to_string(),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn minimal_sends_what_dsh_minimal_sends_and_keeps_its_shell() {
    let golden = golden_wire();
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r: &RecordedRequest| {
        if scenarios::is_title_request(r) {
            return Reply::text("Testtitel");
        }
        match r.tool_results() {
            0 => Reply::tool_call(
                "toolu_1",
                "bash",
                json!({"command": "mkdir -p sub && cd sub && export SEEN=7"}),
            ),
            1 => Reply::tool_call("toolu_2", "bash", json!({"command": "pwd; echo $SEEN"})),
            _ => Reply::text("FERTIG"),
        }
    });
    let home = quiet_home("");
    let ws = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    // Standard would load these; Minimal composes neither.
    std::fs::create_dir_all(ws.path().join(".git")).unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "Always answer in French.").unwrap();
    let prompt = golden["body"]["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    headless(
        &mock,
        home.path(),
        ws.path(),
        others.path(),
        &["--preset", "minimal", prompt],
    );

    let requests = main_requests(&mock);
    assert_eq!(requests.len(), 3);
    // The first request equals dsh's, minus the telemetry we never send.
    let mut expected = golden["body"].clone();
    let expected = expected.as_object_mut().unwrap();
    expected.remove("dsh_session_log");
    expected.remove("dsh_plugin_packages");
    let ours = requests[0].body.as_object().unwrap();
    for key in ["system", "tools", "messages"] {
        assert_eq!(
            serde_json::to_string(&ours[key]).unwrap(),
            serde_json::to_string(&expected[key]).unwrap(),
            "{key}"
        );
    }
    assert_eq!(
        &Value::Object(ours.clone()),
        &Value::Object(expected.clone())
    );

    // The second call ran in the shell the first one left in `sub`.
    let ws_path = ws.path().canonicalize().unwrap().display().to_string();
    assert_eq!(
        tool_results(&requests[2]),
        [
            "[Command finished with exit code 0]".to_owned(),
            format!("{ws_path}/sub\n7\n[Command finished with exit code 0]"),
        ]
    );
    // Every request keeps the one tool and the fixed prompt.
    for r in &requests {
        assert_eq!(r.body["tools"], expected["tools"]);
        assert_eq!(r.body["system"], expected["system"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn standard_requests_are_unchanged() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r: &RecordedRequest| {
        if scenarios::is_title_request(r) {
            Reply::text("Testtitel")
        } else {
            Reply::text("FERTIG")
        }
    });
    let ws = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "Be brief.").unwrap();
    // No preset at all (as before presets), the explicit Standard preset,
    // and Standard as the configured default.
    let plain = quiet_home("");
    headless(&mock, plain.path(), ws.path(), others.path(), &["Hi"]);
    let explicit = quiet_home("");
    headless(
        &mock,
        explicit.path(),
        ws.path(),
        others.path(),
        &["--preset", "standard", "Hi"],
    );
    let configured = quiet_home("default_preset = \"standard\"\n");
    headless(&mock, configured.path(), ws.path(), others.path(), &["Hi"]);

    let requests = main_requests(&mock);
    assert_eq!(requests.len(), 3);
    let shape = |r: &RecordedRequest| {
        let mut body = r.body.clone();
        body.as_object_mut().unwrap().remove("metadata");
        serde_json::to_string(&body).unwrap()
    };
    assert_eq!(shape(&requests[0]), shape(&requests[1]));
    assert_eq!(shape(&requests[0]), shape(&requests[2]));
    // Still the full dsh Standard tool set and prompt.
    let names: Vec<&str> = requests[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "bash",
        "edit",
        "read",
        "todo_write",
        "subagent",
        "create_goal",
    ] {
        assert!(names.contains(&name), "{name} missing: {names:?}");
    }
    // The `skill` tool joins only once the model may load a skill on its own.
    assert!(!names.contains(&"skill"), "{names:?}");
    let bash = &requests[0].body["tools"][0];
    assert_eq!(bash["name"], "bash");
    assert!(bash["input_schema"]["properties"]["description"].is_object());
    let system = requests[0].body["system"].as_str().unwrap();
    assert!(system.starts_with("You are an AI agent powered by DeepSeek Harness."));
}

#[tokio::test(flavor = "multi_thread")]
async fn ptc_cli_runs_typescript_and_returns_only_selected_nested_results() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r: &RecordedRequest| {
        if scenarios::is_title_request(r) {
            return Reply::text("PTC test");
        }
        if r.tool_results() == 0 {
            Reply::tool_call("ptc_program", "run_code", json!({
                "description": "Compare two files and print selected lines",
                "code": "type ReadResult = {lines: {text: string}[]}; const files: string[] = ['a.txt', 'b.txt']; const results = await Promise.all(files.map(file_path => tools.read({file_path}))); for (const result of results) { console.log((result as ReadResult).lines.filter(line => line.text.startsWith('KEEP')).map(line => line.text).join('\\n')); }"
            }))
        } else {
            Reply::text("FERTIG")
        }
    });
    let home = quiet_home("");
    let ws = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a.txt"), "KEEP_A\nPRIVATE_INTERMEDIATE_A\n").unwrap();
    std::fs::write(ws.path().join("b.txt"), "KEEP_B\nPRIVATE_INTERMEDIATE_B\n").unwrap();
    headless(
        &mock,
        home.path(),
        ws.path(),
        others.path(),
        &[
            "--preset",
            "ptc",
            "--sandbox",
            "danger-full-access",
            "Compare files",
        ],
    );
    let requests = main_requests(&mock);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let tools = request.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "run_code");
    }
    let results = tool_results(&requests[1]).join("\n");
    assert!(
        results.contains("KEEP_A") && results.contains("KEEP_B"),
        "{results}"
    );
    assert!(!results.contains("PRIVATE_INTERMEDIATE"), "{results}");
}

#[test]
fn preset_flag_is_for_new_sessions_only() {
    let home = quiet_home("");
    let ws = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .current_dir(ws.path())
        .env("DSHX_HOME", home.path())
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:9/anthropic")
        .args(["--preset", "minimal", "--session-id", "session-x", "Hi"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--preset only applies to new sessions"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(BIN)
        .current_dir(ws.path())
        .env("DSHX_HOME", home.path())
        .args(["--preset", "unknown-mode", "Hi"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown agent preset \"unknown-mode\""));
}

// ---- web API ---------------------------------------------------------------

struct Server {
    child: Child,
    base: String,
    token: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(mock: &MockDeepSeek, home: &Path) -> Server {
    let mut child = Command::new(BIN)
        .args(["web", "--port", "0", "--no-open"])
        .env("DSHX_HOME", home)
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env("DEEPSEEK_BASE_URL", mock.base_url())
        .env_remove("DSH_HOME")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut server = Server {
        child,
        base: String::new(),
        token: String::new(),
    };
    let mut line = String::new();
    loop {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "web exited");
        if let Some(url) = line.trim().strip_prefix("dsh-codex web: ") {
            let (base, token) = url.split_once("/?token=").unwrap();
            server.base = base.to_owned();
            server.token = token.to_owned();
            std::thread::spawn(move || for _ in stderr.lines() {});
            return server;
        }
    }
}

impl Server {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = reqwest::Client::new()
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {v}");
        v
    }

    async fn create(&self, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, "/sessions", Some(body))
            .await
    }

    async fn preset_of(&self, id: &str) -> (Value, Value) {
        let session = self.get(&format!("/sessions/{id}")).await;
        let rows = self.get("/sessions").await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap_or(Value::Null);
        (
            session["header"]["agentPreset"].clone(),
            row["preset"].clone(),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn web_creates_lists_and_describes_presets() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(|r: &RecordedRequest| {
        if scenarios::is_title_request(r) {
            return Reply::text("Titel");
        }
        match r.tool_results() {
            0 if r.body["tools"]
                .as_array()
                .is_some_and(|t| t.len() == 1 && t[0]["name"] == "bash") =>
            {
                Reply::tool_call("toolu_1", "bash", json!({"command": "echo hi"}))
            }
            _ => Reply::text("FERTIG"),
        }
    });
    let home = quiet_home("");
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());

    let info = server.get("/info").await;
    let ids: Vec<&str> = info["presets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["standard", "ptc", "minimal"]);
    let minimal = &info["presets"][2];
    assert_eq!(minimal["name"], "Minimal");
    assert!(
        minimal["description"]
            .as_str()
            .unwrap()
            .contains("terminal tool")
    );
    assert!(
        minimal["help"]["explanation"]
            .as_str()
            .unwrap()
            .contains("One persistent shell tool and a fixed system prompt.")
    );
    assert_eq!(info["settings"]["defaultPreset"], "standard");

    // An explicit Minimal chat: recorded like dsh, listed, fixed.
    let (status, created) = server
        .create(json!({"cwd": ws.path(), "preset": "minimal"}))
        .await;
    assert_eq!(status, 200, "{created}");
    let minimal_id = created["id"].as_str().unwrap().to_owned();
    // A Standard chat as before.
    let (_, created) = server.create(json!({"cwd": ws.path()})).await;
    let standard_id = created["id"].as_str().unwrap().to_owned();
    let (status, err) = server.create(json!({"preset": "bogus"})).await;
    assert_eq!(status, 400);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("unknown agent preset")
    );

    // Logs are written on the first prompt.
    for id in [&minimal_id, &standard_id] {
        let (status, v) = server
            .call(
                reqwest::Method::POST,
                &format!("/sessions/{id}/prompt"),
                Some(json!({"text": "Go"})),
            )
            .await;
        assert_eq!(status, 202, "{v}");
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while main_requests(&mock).len() < 3 {
        assert!(tokio::time::Instant::now() < deadline, "no requests");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        server.preset_of(&minimal_id).await,
        (json!("minimal"), json!("minimal"))
    );
    assert_eq!(
        server.preset_of(&standard_id).await,
        (json!("standard"), json!("standard"))
    );
    let requests = main_requests(&mock);
    let minimal_requests: Vec<_> = requests
        .iter()
        .filter(|r| r.body["tools"].as_array().is_some_and(|t| t.len() == 1))
        .collect();
    assert!(!minimal_requests.is_empty());
    assert_eq!(
        minimal_requests[0].body["system"],
        "You are a helpful software engineer assistant."
    );
    assert!(
        requests
            .iter()
            .any(|r| r.body["tools"].as_array().unwrap().len() > 10)
    );

    // Goals are no part of Minimal.
    let (status, err) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{minimal_id}/goal"),
            Some(json!({"action": "create", "objective": "Ship it"})),
        )
        .await;
    assert_eq!(status, 400);
    assert!(err["error"].as_str().unwrap().contains("Minimal"), "{err}");

    // The default for new chats is a setting.
    let (status, info) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"defaultPreset": "minimal"})),
        )
        .await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["settings"]["defaultPreset"], "minimal");
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(config.contains("default_preset = \"minimal\""), "{config}");
    let (status, _) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"defaultPreset": "unknown-mode"})),
        )
        .await;
    assert_eq!(status, 400);
    let (_, created) = server.create(json!({"cwd": ws.path()})).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let session = server.get(&format!("/sessions/{id}")).await;
    assert_eq!(session["header"]["agentPreset"], "minimal");

    // PTC is selectable as a default and retained by created sessions.
    let (status, info) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"defaultPreset": "ptc"})),
        )
        .await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["settings"]["defaultPreset"], "ptc");
    let (_, created) = server.create(json!({"cwd": ws.path()})).await;
    let ptc_id = created["id"].as_str().unwrap();
    let (status, result) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{ptc_id}/prompt"),
            Some(json!({"text": "Hello PTC"})),
        )
        .await;
    assert_eq!(status, 202, "{result}");
    assert_eq!(server.preset_of(ptc_id).await, (json!("ptc"), json!("ptc")));
    let (status, mcp) = server
        .call(
            reqwest::Method::GET,
            &format!("/sessions/{ptc_id}/mcp"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{mcp}");
}
