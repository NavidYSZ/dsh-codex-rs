//! `dsh-codex web` with goals, schedules and Agent Teams on: the REST
//! routes the UI uses, a goal driven to its round limit, a reminder created
//! by the model and delivered by the host timer, and the Team view.
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

fn script(r: &RecordedRequest) -> Reply {
    if r.system().is_some_and(|s| s.contains("concise title")) {
        return Reply::text("Titel");
    }
    let mut blocks: Vec<(String, Value)> = Vec::new();
    for m in r.messages() {
        let role = m["role"].as_str().unwrap_or_default().to_owned();
        for b in m["content"].as_array().into_iter().flatten() {
            blocks.push((role.clone(), b.clone()));
        }
    }
    let marker = |t: &str| {
        t.starts_with("SCHEDULE ")
            || t.starts_with("<goal_round>")
            || t.starts_with("[SCHEDULE REMINDER")
    };
    let start = blocks
        .iter()
        .rposition(|(role, b)| role == "user" && b["text"].as_str().is_some_and(marker))
        .unwrap_or(0);
    let task = blocks[start].1["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let done = blocks[start..]
        .iter()
        .filter(|(_, b)| b["type"] == "tool_result")
        .count();
    let id = format!("toolu_{}", blocks.len());
    if task.starts_with("<goal_round>") {
        return Reply::text("Progress.");
    }
    if task.starts_with("[SCHEDULE REMINDER") {
        return Reply::text("Reminded.");
    }
    match done {
        0 => Reply::tool_call(
            id,
            "schedule_create",
            json!({"prompt": "Check the deploy", "title": "Deploy", "after_seconds": 1}),
        ),
        1 => Reply::tool_call(
            id,
            "schedule_create",
            json!({"prompt": "Standup", "title": "Standup", "daily": {"time": "09:00:00", "time_zone": "UTC"}}),
        ),
        _ => Reply::text("Scheduled."),
    }
}

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

fn start(mock: &MockDeepSeek, home: &std::path::Path) -> Server {
    std::fs::write(
        home.join("config.toml"),
        "[codex.schedule]\nenabled = true\n\n[codex.team]\nenabled = true\n\n[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n",
    )
    .unwrap();
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
    // Owned by the server first, so a failed start still reaps the process.
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

    async fn until(&self, what: &str, path: &str, f: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let v = self.get(path).await;
            if f(&v) {
                return v;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}: {v}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn web_serves_goals_schedules_and_teams() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(script);
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());

    let info = server.get("/info").await;
    assert_eq!(
        info["automation"],
        json!({"goals": true, "schedules": true, "timeContext": true, "team": true})
    );
    let (_, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(
        server.get(&format!("/sessions/{id}/goal")).await,
        json!({"goal": null})
    );

    // A goal from the UI: armed at once, one round, then the round limit.
    let (status, goal) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/goal"),
            Some(json!({"action": "create", "objective": "Ship it", "maxGoalRounds": 1})),
        )
        .await;
    assert_eq!(status, 200, "{goal}");
    assert_eq!(goal["goal"]["objective"], "Ship it");
    assert_eq!(goal["goal"]["activation"], "armed");
    let blocked = server
        .until("the round limit", &format!("/sessions/{id}/goal"), |v| {
            v["goal"]["phase"] == "blocked"
        })
        .await;
    assert_eq!(blocked["goal"]["blockedReason"]["code"], "round-limit");
    assert_eq!(blocked["goal"]["roundsStarted"], 1);
    let (status, err) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/goal"),
            Some(json!({"action": "pause"})),
        )
        .await;
    assert_eq!(status, 400, "{err}");
    let (status, cleared) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/goal"),
            Some(json!({"action": "clear"})),
        )
        .await;
    assert_eq!((status, cleared), (200, json!({"goal": null})));

    // Reminders created by the model; the one-second one is delivered.
    let (status, _) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(json!({"text": "SCHEDULE two reminders"})),
        )
        .await;
    assert_eq!(status, 202);
    let catalog = server
        .until("the delivery", "/schedules", |v| {
            v.as_array()
                .is_some_and(|a| a.len() == 2 && a.iter().any(|t| t["status"] == "inactive"))
        })
        .await;
    let once = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "Deploy")
        .unwrap()
        .clone();
    let daily = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "Standup")
        .unwrap()
        .clone();
    assert_eq!(once["sessionId"], id.as_str());
    assert!(once["lastDelivery"]["messageId"].is_string());
    let history = server
        .get(&format!(
            "/schedules/{}/history",
            once["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(history["records"][0]["prompt"], "Check the deploy");
    let session = server.get(&format!("/sessions/{id}")).await;
    let reminder = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "user/message" && e["data"]["source"]["kind"] == "schedule")
        .expect("the reminder reached the session");
    assert!(
        reminder["data"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[SCHEDULE REMINDER]\n")
    );
    let listed = server.get(&format!("/sessions/{id}/schedules")).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "only active reminders");
    assert_eq!(listed[0]["deliveryMode"], "host");

    // Update and delete from the UI.
    let daily_id = daily["id"].as_str().unwrap();
    let (status, updated) = server
        .call(reqwest::Method::PUT, &format!("/schedules/{daily_id}"), Some(json!({"title": "Daily standup", "daily": {"time": "10:30:00", "time_zone": "UTC"}})))
        .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["title"], "Daily standup");
    assert_eq!(updated["time"], "10:30:00.000");
    let (status, _) = server
        .call(
            reqwest::Method::PUT,
            &format!("/schedules/{daily_id}"),
            Some(json!({"every_seconds": 5})),
        )
        .await;
    assert_eq!(status, 400);
    let (status, deleted) = server
        .call(
            reqwest::Method::DELETE,
            &format!("/schedules/{daily_id}"),
            None,
        )
        .await;
    assert_eq!(
        (status, deleted),
        (200, json!({"id": daily_id, "deleted": true}))
    );
    let (status, _) = server
        .call(
            reqwest::Method::DELETE,
            &format!("/schedules/{daily_id}"),
            None,
        )
        .await;
    assert_eq!(status, 404);

    // The session leads an (empty) Team.
    let team = server.get(&format!("/sessions/{id}/team")).await;
    assert_eq!(
        team["members"],
        json!([{"id": id, "name": "lead", "role": "lead", "phase": "active"}])
    );
    assert_eq!(team["tasks"], json!([]));
    assert_eq!(team["roster"][0]["target"], "lead");
    // The model was offered the Team and schedule tools instead of subagents.
    let tools: Vec<String> = mock
        .requests()
        .iter()
        .rfind(|r| !r.system().is_some_and(|s| s.contains("concise title")))
        .unwrap()
        .body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for name in [
        "create_goal",
        "schedule_create",
        "spawn_teammate",
        "wait_agent",
    ] {
        assert!(
            tools.contains(&name.to_owned()),
            "{name} missing in {tools:?}"
        );
    }
    assert!(!tools.contains(&"subagent".to_owned()));
    assert!(home.path().join("storages/schedule.json").is_file());
}
