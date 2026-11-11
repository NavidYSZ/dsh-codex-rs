//! `dsh-codex web` with an orchestration run: created over REST, planned in
//! a reserved round, executed by two dependency-ordered worker one-shots,
//! and completed with per-criterion evidence — everything asserted over the
//! orchestration REST view.
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dshx_testkit::{MockDeepSeek, RecordedRequest, Reply};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

/// The first text of the last user message (context snapshots are appended
/// after the prompt) and the texts of every `tool_result`.
fn history(r: &RecordedRequest) -> (Option<String>, Vec<String>) {
    let mut last_user = None;
    let mut results = Vec::new();
    for m in r.messages() {
        let role = m["role"].as_str().unwrap_or_default();
        let mut first_text = None;
        for b in m["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_result" {
                let text = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter_map(|x| x["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => String::new(),
                };
                results.push(text);
            } else if first_text.is_none()
                && let Some(t) = b["text"].as_str()
            {
                first_text = Some(t.to_owned());
            }
        }
        if role == "user"
            && let Some(t) = first_text
        {
            last_user = Some(t);
        }
    }
    (last_user, results)
}

/// The freshest `{"orchestration": …}` view in the tool results.
fn last_view(results: &[String]) -> Option<Value> {
    results.iter().rev().find_map(|t| {
        serde_json::from_str::<Value>(t)
            .ok()
            .and_then(|v| v.get("orchestration").cloned())
            .filter(|v| !v.is_null())
    })
}

fn script(r: &RecordedRequest) -> Reply {
    if r.system().is_some_and(|s| s.contains("concise title")) {
        return Reply::text("Titel");
    }
    let (last_user, results) = history(r);
    let last_user = last_user.unwrap_or_default();
    if last_user.starts_with("<orchestration_complete>") {
        assert_eq!(
            last_view(&results).unwrap()["phase"],
            "complete",
            "a plan must never tell the model that the run is complete"
        );
        return Reply::text("Finished with notes.md and report.md as evidence.");
    }
    if last_user.starts_with("<orchestration_task>") {
        // Real file writes in the isolated workspace; dependency two reads
        // the first artifact before writing its own.
        if last_user.contains("Your task [t-notes]") && results.is_empty() {
            return Reply::tool_call(
                "write_notes",
                "write",
                json!({"file_path": "notes.md", "content": "Sources: one, two, three.\n"}),
            );
        }
        if last_user.contains("Your task [t-report]") {
            if results.is_empty() {
                return Reply::tool_call("read_notes", "read", json!({"file_path": "notes.md"}));
            }
            if results.len() == 1 {
                return Reply::tool_call(
                    "write_report",
                    "write",
                    json!({"file_path": "report.md", "content": "Report based on sources one, two, three.\n"}),
                );
            }
        }
        return if last_user.contains("Your task [t-notes]") {
            Reply::text("Gathered 3 sources into notes.md.")
        } else {
            Reply::text("Wrote the final report to report.md.")
        };
    }
    if !last_user.starts_with("<orchestration_round>") {
        return Reply::text("OK");
    }
    // A planner round: read fresh state first, then plan or complete.
    let round = last_user
        .split("Round: ")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .and_then(|r| r.trim().parse::<i64>().ok())
        .unwrap_or_default();
    let stale = |v: &Value| v["roundsStarted"].as_i64().unwrap_or_default() < round;
    let Some(view) = last_view(&results).filter(|v| !stale(v)) else {
        return Reply::tool_call("toolu_get", "get_orchestration", json!({}));
    };
    let id = view["id"].as_str().unwrap_or_default().to_owned();
    let revision = view["revision"].as_i64().unwrap_or_default();
    let plan_version = view["planVersion"].as_i64().unwrap_or_default();
    let tasks = view["tasks"].as_array().cloned().unwrap_or_default();
    let all_done = !tasks.is_empty()
        && tasks
            .iter()
            .all(|t| t["status"].as_str() == Some("completed"));
    if plan_version == 0 {
        return Reply::tool_call(
            "toolu_plan",
            "update_orchestration",
            json!({
                "run_id": id, "revision": revision, "action": "plan",
                "tasks": [
                    {"id": "t-notes", "description": "Collect the sources into notes.md.", "dependsOn": []},
                    {"id": "t-report", "description": "Write report.md from the notes.", "dependsOn": ["t-notes"]},
                ],
            }),
        );
    }
    if all_done && view["phase"].as_str() == Some("active") {
        return Reply::tool_call(
            "toolu_complete",
            "update_orchestration",
            json!({
                "run_id": id, "revision": revision, "action": "complete",
                "checks": [
                    {"criterionId": "notes", "evidence": ["notes.md", "worker session of t-notes"]},
                    {"criterionId": "report", "evidence": ["report.md", "worker session of t-report"]},
                ],
            }),
        );
    }
    Reply::text("Continuing.")
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
        "[codex.browser]\nenabled = false\n\n[codex.computerUse]\nenabled = false\n\n[codex.apps]\nenabled = false\n",
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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
async fn web_runs_an_orchestration_to_completion() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(script);
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());

    let (status, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // Malformed inputs must not leave a broken run in this chat's log.
    for invalid in [
        json!({"maxRounds": 0}),
        json!([]),
        json!({"maxWorkers": 65}),
    ] {
        let (status, v) = server
            .call(
                reqwest::Method::POST,
                &format!("/sessions/{id}/orchestration"),
                Some(json!({
                    "action": "create", "objective": "invalid budget", "limits": invalid,
                    "criteria": [{"id": "done", "criterion": "Done"}],
                })),
            )
            .await;
        assert_eq!(status, 400, "{v}");
        assert!(
            server.get(&format!("/sessions/{id}/orchestration")).await["orchestration"].is_null()
        );
    }

    // A run over REST: objective plus two acceptance criteria.
    let (status, v) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/orchestration"),
            Some(json!({
                "action": "create",
                "objective": "Produce notes.md and report.md",
                "criteria": [
                    {"id": "notes", "criterion": "notes.md lists the sources"},
                    {"id": "report", "criterion": "report.md exists and summarizes them"},
                ],
            })),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["orchestration"]["phase"], "active");
    assert_eq!(v["orchestration"]["activation"], "armed");

    // The driver plans, dispatches both workers (t-report after t-notes)
    // and completes with one evidence-citing check per criterion.
    let done = server
        .until(
            "the completed run",
            &format!("/sessions/{id}/orchestration"),
            |v| v["orchestration"]["phase"] == "complete",
        )
        .await;
    let run = &done["orchestration"];
    assert_eq!(run["workersDispatched"], 2, "two worker dispatches");
    assert_eq!(run["planVersion"], 1);
    assert_eq!(
        std::fs::read_to_string(ws.path().join("notes.md")).unwrap(),
        "Sources: one, two, three.\n"
    );
    assert!(
        std::fs::read_to_string(ws.path().join("report.md"))
            .unwrap()
            .contains("one, two, three")
    );
    let tasks = run["tasks"].as_array().cloned().unwrap_or_default();
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    for t in &tasks {
        assert_eq!(t["status"], "completed", "{t:?}");
        assert!(t["childId"].as_str().is_some(), "evidence chain: {t:?}");
    }
    let report = tasks.iter().find(|t| t["id"] == "t-report").unwrap();
    assert_eq!(report["dependsOn"], json!(["t-notes"]));
    let checks = run["checks"].as_array().cloned().unwrap_or_default();
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(
        checks
            .iter()
            .all(|c| c["evidence"].as_array().is_some_and(|e| !e.is_empty()))
    );

    // A finished run is replaced by the next one.
    let (status, v) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/orchestration"),
            Some(json!({
                "action": "create",
                "objective": "Nothing left",
                "criteria": [{"id": "done", "criterion": "immediately complete"}],
                "limits": {"maxRounds": 1},
            })),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["orchestration"]["limits"]["maxRounds"], 1);
    let blocked = server
        .until(
            "the round cap",
            &format!("/sessions/{id}/orchestration"),
            |v| v["orchestration"]["phase"] == "blocked",
        )
        .await;
    assert_eq!(blocked["orchestration"]["roundsStarted"], 1);
    assert_eq!(
        blocked["orchestration"]["blockedReason"]["code"],
        "round-limit"
    );

    // The worker receives ordinary work tools but never run-control tools.
    let requests = mock.requests();
    let workers: Vec<_> = requests
        .iter()
        .filter(|r| {
            history(r)
                .0
                .is_some_and(|s| s.starts_with("<orchestration_task>"))
        })
        .collect();
    assert!(!workers.is_empty());
    for r in workers {
        for tool in r.body["tools"].as_array().into_iter().flatten() {
            let name = tool["name"].as_str().unwrap_or_default();
            assert!(
                !dshx_orchestrator::TOOL_NAMES.contains(&name),
                "worker received {name}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clearing_running_workers_does_not_settle_into_the_next_run() {
    let mock = MockDeepSeek::start().await;
    let delayed = Arc::new(AtomicBool::new(false));
    let seen = delayed.clone();
    mock.set_responder(move |r| {
        if history(r)
            .0
            .is_some_and(|s| s.starts_with("<orchestration_task>"))
            && !seen.swap(true, Ordering::SeqCst)
        {
            return Reply::stream()
                .text("old worker result")
                .delay(Duration::from_secs(1))
                .into();
        }
        script(r)
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());
    let (status, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let path = format!(
        "/sessions/{}/orchestration",
        created["id"].as_str().unwrap()
    );
    let body = json!({"action": "create", "objective": "Produce notes.md and report.md", "criteria": [
        {"id": "notes", "criterion": "notes.md lists the sources"},
        {"id": "report", "criterion": "report.md exists and summarizes them"},
    ]});
    let (status, _) = server
        .call(reqwest::Method::POST, &path, Some(body.clone()))
        .await;
    assert_eq!(status, 200);
    let old = server
        .until("a running worker", &path, |v| {
            delayed.load(Ordering::SeqCst)
                && v["orchestration"]["tasks"]
                    .as_array()
                    .is_some_and(|ts| ts.iter().any(|t| t["status"] == "running"))
        })
        .await;
    let (status, cleared) = server
        .call(
            reqwest::Method::POST,
            &path,
            Some(json!({"action": "clear"})),
        )
        .await;
    assert_eq!(status, 200, "{cleared}");
    assert!(cleared["orchestration"].is_null());
    let (status, new) = server.call(reqwest::Method::POST, &path, Some(body)).await;
    assert_eq!(status, 200, "{new}");
    assert_ne!(old["orchestration"]["id"], new["orchestration"]["id"]);
    let done = server
        .until("the replacement run", &path, |v| {
            v["orchestration"]["phase"] == "complete"
        })
        .await;
    assert_eq!(done["orchestration"]["workersDispatched"], 2);
    assert_eq!(done["orchestration"]["id"], new["orchestration"]["id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn enabling_orchestration_supports_a_chat_opened_while_disabled() {
    let mock = MockDeepSeek::start().await;
    mock.set_responder(script);
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());
    let (status, info) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"orchestration": {"enabled": false}})),
        )
        .await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["automation"]["orchestration"], false);
    let (status, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_str().unwrap();
    let (status, _) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/prompt"),
            Some(json!({"text": "Open this chat"})),
        )
        .await;
    assert_eq!(status, 202);
    server
        .until("the first reply", &format!("/sessions/{id}"), |v| {
            v["status"] == "idle"
                && v["events"].as_array().is_some_and(|es| {
                    es.iter()
                        .any(|e| e["type"] == "turn/end" || e["kind"] == "turn/end")
                })
        })
        .await;
    let path = format!("/sessions/{id}/orchestration");
    assert_eq!(server.call(reqwest::Method::GET, &path, None).await.0, 404);
    let (status, info) = server
        .call(
            reqwest::Method::PUT,
            "/settings",
            Some(json!({"orchestration": {"enabled": true}})),
        )
        .await;
    assert_eq!(status, 200, "{info}");
    let (status, view) = server
        .call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "action": "create", "objective": "Produce notes.md and report.md", "criteria": [
                    {"id": "notes", "criterion": "notes.md lists the sources"},
                    {"id": "report", "criterion": "report.md summarizes them"},
                ],
            })),
        )
        .await;
    assert_eq!(status, 200, "{view}");
    let completed = server
        .until("the complete loop in the already-open chat", &path, |v| {
            v["orchestration"]["phase"] == "complete"
        })
        .await;
    assert_eq!(completed["orchestration"]["workersDispatched"], 2);
    assert!(ws.path().join("report.md").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_cancels_workers_while_the_planner_is_idle() {
    let mock = MockDeepSeek::start().await;
    let seen = Arc::new(AtomicBool::new(false));
    let worker_seen = seen.clone();
    mock.set_responder(move |r| {
        if history(r)
            .0
            .is_some_and(|s| s.starts_with("<orchestration_task>"))
        {
            worker_seen.store(true, Ordering::SeqCst);
            Reply::stream()
                .text("slow worker")
                .delay(Duration::from_secs(1))
                .into()
        } else {
            script(r)
        }
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());
    let (_, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/sessions/{id}/orchestration");
    let (status, view) = server
        .call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "action": "create", "objective": "Produce notes.md and report.md", "criteria": [
                    {"id": "notes", "criterion": "notes.md lists the sources"},
                    {"id": "report", "criterion": "report.md summarizes them"},
                ],
            })),
        )
        .await;
    assert_eq!(status, 200, "{view}");
    server
        .until("a running background worker", &path, |v| {
            seen.load(Ordering::SeqCst)
                && v["orchestration"]["tasks"]
                    .as_array()
                    .is_some_and(|ts| ts.iter().any(|t| t["status"] == "running"))
        })
        .await;
    let (status, _) = server
        .call(
            reqwest::Method::POST,
            &format!("/sessions/{id}/cancel"),
            None,
        )
        .await;
    assert_eq!(status, 202);
    let stopped = server
        .until("worker cancellation", &path, |v| {
            v["orchestration"]["activation"] == "disarmed"
                && v["orchestration"]["tasks"]
                    .as_array()
                    .is_some_and(|ts| ts.iter().any(|t| t["status"] == "failed"))
        })
        .await;
    assert_eq!(stopped["orchestration"]["workersDispatched"], 1);
    assert_eq!(stopped["orchestration"]["roundsStarted"], 1);
    assert!(!ws.path().join("report.md").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_after_a_plan_prevents_worker_dispatch() {
    let mock = MockDeepSeek::start().await;
    let planned = Arc::new(AtomicBool::new(false));
    let seen = planned.clone();
    mock.set_responder(move |r| {
        let (prompt, results) = history(r);
        if prompt.is_some_and(|s| s.starts_with("<orchestration_round>"))
            && last_view(&results).is_some_and(|v| v["planVersion"] == 1)
        {
            seen.store(true, Ordering::SeqCst);
            Reply::stream()
                .text("plan committed")
                .delay(Duration::from_secs(1))
                .into()
        } else {
            script(r)
        }
    });
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let server = start(&mock, home.path());
    let (_, created) = server
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"cwd": ws.path()})),
        )
        .await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/sessions/{id}/orchestration");
    let (status, view) = server
        .call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "action": "create", "objective": "Produce notes.md and report.md", "criteria": [
                    {"id": "notes", "criterion": "notes.md lists the sources"},
                    {"id": "report", "criterion": "report.md summarizes them"},
                ],
            })),
        )
        .await;
    assert_eq!(status, 200, "{view}");
    server
        .until("the saved plan before the answer ends", &path, |v| {
            planned.load(Ordering::SeqCst) && v["orchestration"]["planVersion"] == 1
        })
        .await;
    assert_eq!(
        server
            .call(
                reqwest::Method::POST,
                &format!("/sessions/{id}/cancel"),
                None
            )
            .await
            .0,
        202
    );
    let stopped = server
        .until("a paused run after Stop", &path, |v| {
            v["orchestration"]["phase"] == "paused"
        })
        .await;
    assert_eq!(stopped["orchestration"]["activation"], "disarmed");
    assert_eq!(stopped["orchestration"]["workersDispatched"], 0);
    assert!(!ws.path().join("notes.md").exists());
}
