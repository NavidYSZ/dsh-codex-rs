//! The Skills and MCP pages' routes of `dsh-codex web` against the real
//! runtime: "Test connection" with a tiny stdio server, adding and editing
//! a server whose secrets stay in `credentials.toml`, the server log and
//! tool schemas, and skills (detail, blank template, invalid frontmatter).
//! Every home folder is a temporary directory.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::MockDeepSeek;
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

    async fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method.clone(), path, body).await;
        assert_eq!(status, 200, "{method} {path}: {value}");
        value
    }

    async fn get(&self, path: &str) -> Value {
        self.ok(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.ok(reqwest::Method::POST, path, Some(body)).await
    }

    async fn put(&self, path: &str, body: Value) -> Value {
        self.ok(reqwest::Method::PUT, path, Some(body)).await
    }

    /// Polls `path` until `ok` holds (10 s).
    async fn wait(&self, path: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..200 {
            let value = self.get(path).await;
            if ok(&value) {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}: {}", self.get(path).await);
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

fn has_line(log: &Value, kind: &str, text: &str) -> bool {
    log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["kind"] == kind && l["text"] == text)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_and_servers_with_secrets() {
    let mock = MockDeepSeek::start().await;
    let home = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    let web = Web::start(home.path(), others.path(), &mock);
    let fixture = fixture_server();

    // Test connection: a working stdio server, then a broken one.
    let probe = web
        .post(
            "/mcp/test",
            json!({"name": "fixture", "config": {"command": "python3", "args": [fixture]}}),
        )
        .await;
    assert_eq!(probe["ok"], true, "{probe}");
    let tools = probe["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "echo"));
    let broken = web
        .post(
            "/mcp/test",
            json!({"name": "broken", "config": {"command": "sh",
                "args": ["-c", "echo 'npm ERR! 404 Not Found' >&2; exit 1"]}, "timeoutMs": 5000}),
        )
        .await;
    assert_eq!(broken["ok"], false);
    assert!(!broken["error"].as_str().unwrap().is_empty(), "{broken}");
    assert_eq!(broken["stderr"], json!(["npm ERR! 404 Not Found"]));
    // Nothing was saved.
    assert_eq!(web.get("/mcp/servers").await, json!([]));

    // Add a server whose variable is a secret: it reaches the process
    // (the shell prints it to stderr) but never config.toml.
    let script = format!("echo \"token=$MCP_TEST_VAR\" >&2; exec python3 '{fixture}'");
    let added = web
        .post(
            "/mcp/servers",
            json!({"name": "fx", "config": {"command": "sh", "args": ["-c", script],
                "env_vars": ["HOME"]},
                "secrets": {"env": {"MCP_TEST_VAR": "s3cret-value"}}}),
        )
        .await;
    assert_eq!(added["env"], json!(["MCP_TEST_VAR"]));
    assert_eq!(added["secrets"]["env"], json!(["MCP_TEST_VAR"]));
    assert_eq!(added["envVars"], json!(["HOME"]));
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!config.contains("s3cret-value"), "{config}");
    assert!(config.contains("[mcp_servers.fx]"), "{config}");
    let credentials = std::fs::read_to_string(home.path().join("credentials.toml")).unwrap();
    assert!(credentials.contains("s3cret-value"), "{credentials}");
    web.wait("/mcp/servers/fx/log", "the secret in stderr", |log| {
        has_line(log, "stderr", "token=s3cret-value")
    })
    .await;
    let log = web.get("/mcp/servers/fx/log").await;
    assert!(
        log["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["kind"] == "info" && l["text"].as_str().unwrap().starts_with("connected"))
    );
    // The detail carries the tools' schemas.
    let detail = web
        .wait("/mcp/servers/fx", "the tools", |s| {
            s["tools"].as_array().is_some_and(|t| !t.is_empty())
        })
        .await;
    let echo = detail["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["rawName"] == "echo")
        .unwrap();
    assert_eq!(echo["inputSchema"]["required"], json!(["text"]));

    // Editing keeps a masked (null) value and restarts with the new args.
    let script2 = format!("echo \"again=$MCP_TEST_VAR\" >&2; exec python3 '{fixture}'");
    let edited = web
        .put(
            "/mcp/servers/fx",
            json!({"config": {"command": "sh", "args": ["-c", script2]},
                "secrets": {"env": {"MCP_TEST_VAR": null}}}),
        )
        .await;
    assert_eq!(edited["secrets"]["env"], json!(["MCP_TEST_VAR"]));
    assert_eq!(edited["envVars"], json!([]));
    web.wait(
        "/mcp/servers/fx/log",
        "the restart with the kept secret",
        |log| has_line(log, "stderr", "again=s3cret-value"),
    )
    .await;
    // Test connection of the edited server resolves the masked value too.
    let probe = web
        .post(
            "/mcp/test",
            json!({"name": "fx", "config": {"command": "sh", "args": ["-c",
                "echo \"probe=$MCP_TEST_VAR\" >&2; exit 2"]},
                "secrets": {"env": {"MCP_TEST_VAR": null}}, "timeoutMs": 5000}),
        )
        .await;
    assert_eq!(probe["stderr"], json!(["probe=s3cret-value"]));

    // Unknown names are 404, bad forms 400.
    let (status, _) = web
        .call(reqwest::Method::GET, "/mcp/servers/nope/log", None)
        .await;
    assert_eq!(status, 404);
    let (status, body) = web
        .call(
            reqwest::Method::PUT,
            "/mcp/servers/fx",
            Some(json!({"config": {"command": "sh"}, "secrets": {"env": {"OTHER": null}}})),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("OTHER needs a value")
    );

    // Removing the server removes its secrets.
    let (status, _) = web
        .call(reqwest::Method::DELETE, "/mcp/servers/fx", None)
        .await;
    assert_eq!(status, 204);
    assert!(
        !home.path().join("credentials.toml").exists()
            || !std::fs::read_to_string(home.path().join("credentials.toml"))
                .unwrap()
                .contains("s3cret-value")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_detail_template_and_problems() {
    let mock = MockDeepSeek::start().await;
    let home = tempfile::tempdir().unwrap();
    let others = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let skills = project.path().join(".agents/skills");
    std::fs::create_dir_all(skills.join("brand-kit/assets")).unwrap();
    std::fs::write(
        skills.join("brand-kit/SKILL.md"),
        "---\nname: brand-kit\ntitle: Brand Kit Pro\ndescription: Logos and colors.\n---\n\n# Brand\n",
    )
    .unwrap();
    std::fs::write(skills.join("brand-kit/assets/logo.svg"), "<svg/>").unwrap();
    std::fs::create_dir_all(skills.join("broken")).unwrap();
    std::fs::write(skills.join("broken/SKILL.md"), "no frontmatter\n").unwrap();
    let web = Web::start(home.path(), others.path(), &mock);
    let cwd = project.path().canonicalize().unwrap();
    let q = format!("cwd={}", cwd.display());

    let list = web.get(&format!("/skills?{q}")).await;
    let rows = list.as_array().unwrap();
    let brand = rows.iter().find(|r| r["name"] == "brand-kit").unwrap();
    assert_eq!(brand["title"], "Brand Kit Pro");
    assert_eq!(brand["source"], "project-agents");
    let broken = rows.iter().find(|r| r["name"] == "broken").unwrap();
    assert_eq!(broken["invalid"], true);
    assert_eq!(broken["error"], "missing YAML frontmatter");
    assert_eq!(broken["userInvocable"], false);

    let detail = web.get(&format!("/skills/brand-kit?{q}")).await;
    assert_eq!(detail["body"], "# Brand\n");
    assert_eq!(detail["frontmatter"]["title"], "Brand Kit Pro");
    let files: Vec<&str> = detail["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["assets", "assets/logo.svg", "SKILL.md"]);
    let (status, _) = web
        .call(reqwest::Method::GET, &format!("/skills/unknown?{q}"), None)
        .await;
    assert_eq!(status, 404);

    // Blank templates: personal (the agents home) and in the project.
    let personal = web
        .post(
            "/skills",
            json!({"name": "release-notes", "scope": "personal", "cwd": cwd,
                "description": "Write release notes."}),
        )
        .await;
    let file = others.path().join("agents/skills/release-notes/SKILL.md");
    assert!(file.is_file());
    assert_eq!(personal["title"], "Release Notes");
    assert_eq!(personal["source"], "user-agents");
    assert!(
        personal["content"]
            .as_str()
            .unwrap()
            .contains("name: release-notes")
    );
    let local = web
        .post(
            "/skills",
            json!({"name": "deploy-check", "scope": "project", "cwd": cwd.join("sub")}),
        )
        .await;
    assert_eq!(local["source"], "project-agents");
    assert!(skills.join("deploy-check/SKILL.md").is_file());
    // Taken names and bad names are refused.
    for body in [
        json!({"name": "brand-kit", "scope": "personal", "cwd": cwd}),
        json!({"name": "Bad Name", "scope": "personal"}),
        json!({"name": "x", "scope": "project"}),
    ] {
        let (status, err) = web.call(reqwest::Method::POST, "/skills", Some(body)).await;
        assert_eq!(status, 400, "{err}");
    }
}
