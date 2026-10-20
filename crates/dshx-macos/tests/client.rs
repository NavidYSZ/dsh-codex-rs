//! The socket client (`lib/cua-client.js`): framing, ids, timeouts, errors and
//! starting the helper on demand.
#![cfg(unix)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{FakeHelper, Reply, short_tempdir};
use dshx_macos::{CallOptions, CuaClient, CuaError};
use serde_json::{Map, Value, json};

fn req(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// A stand-in for `/usr/bin/open` that records its arguments.
fn fake_open(dir: &Path, exit: i32) -> PathBuf {
    let script = dir.join("open");
    let log = dir.join("open.args");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit {exit}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn open_calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("open.args"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn one_json_line_per_request_with_counting_ids() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |r| {
        Reply::Json(json!({ "ok": true, "echo": r["cmd"] }))
    });
    let client = CuaClient::with_socket_path(&helper.path);
    let a = client
        .call(&req(json!({ "cmd": "status" })), CallOptions::default())
        .await
        .unwrap();
    let b = client
        .call(
            &req(json!({ "cmd": "configure", "accent": "#339cff" })),
            CallOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(a, json!({ "ok": true, "echo": "status", "id": 1 }));
    assert_eq!(b["id"], json!(2));
    assert_eq!(
        helper.lines(),
        vec![
            r#"{"id":1,"cmd":"status"}"#,
            r##"{"id":2,"cmd":"configure","accent":"#339cff"}"##
        ]
    );
}

#[tokio::test]
async fn missing_socket_without_launch_fails_fast() {
    let dir = short_tempdir();
    let socket = dir.path().join("cua.sock");
    let open = fake_open(dir.path(), 0);
    let client = CuaClient::with_socket_path(&socket)
        .with_app_path(dir.path())
        .with_open_program(&open);
    let err = client
        .call(
            &req(json!({ "cmd": "mini_poll" })),
            CallOptions::timeout_ms(2000).no_launch(),
        )
        .await
        .unwrap_err();
    assert!(err.helper_not_running());
    assert_eq!(
        err.to_string(),
        format!("connect ENOENT {}", socket.display())
    );
    assert!(
        open_calls(dir.path()).is_empty(),
        "launch: false must not start the helper"
    );
}

#[tokio::test]
async fn refused_connection_counts_as_not_running() {
    let dir = short_tempdir();
    let socket = dir.path().join("cua.sock");
    // A stale socket file nobody listens on.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let client = CuaClient::with_socket_path(&socket);
    let err = client
        .call(
            &req(json!({ "cmd": "status" })),
            CallOptions::default().no_launch(),
        )
        .await
        .unwrap_err();
    assert!(err.helper_not_running());
    assert_eq!(
        err.to_string(),
        format!("connect ECONNREFUSED {}", socket.display())
    );
}

#[tokio::test]
async fn not_installed_helper_is_reported() {
    let dir = short_tempdir();
    let app = dir.path().join("Applications/DSH Computer Use.app");
    let client = CuaClient::with_socket_path(dir.path().join("cua.sock")).with_app_path(&app);
    let err = client
        .call(&req(json!({ "cmd": "status" })), CallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CuaError::NotInstalled(_)));
    assert_eq!(
        err.to_string(),
        format!(
            "computer-use helper not installed at {} (run helper/build.sh --install)",
            app.display()
        )
    );
}

#[tokio::test]
async fn starts_the_helper_and_retries_until_it_listens() {
    let dir = short_tempdir();
    let socket = dir.path().join("cua.sock");
    let app = dir.path().join("DSH Computer Use.app");
    std::fs::create_dir(&app).unwrap();
    let open = fake_open(dir.path(), 0);
    let client = CuaClient::with_socket_path(&socket)
        .with_app_path(&app)
        .with_open_program(&open);

    let late = socket.clone();
    let starter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        FakeHelper::start(&late, |_| Reply::Json(json!({ "ok": true })))
    });
    let answer = client
        .call(&req(json!({ "cmd": "status" })), CallOptions::default())
        .await
        .unwrap();
    let helper = starter.await.unwrap();
    assert_eq!(answer, json!({ "ok": true, "id": 1 }));
    assert_eq!(
        open_calls(dir.path()),
        vec![format!("-g {}", app.display())]
    );
    // Failed connects do not use up ids.
    assert_eq!(helper.lines(), vec![r#"{"id":1,"cmd":"status"}"#]);
}

#[tokio::test]
async fn gives_up_when_the_helper_never_listens() {
    let dir = short_tempdir();
    let app = dir.path().join("DSH Computer Use.app");
    std::fs::create_dir(&app).unwrap();
    let client = CuaClient::with_socket_path(dir.path().join("cua.sock"))
        .with_app_path(&app)
        .with_open_program(fake_open(dir.path(), 0));
    let started = Instant::now();
    let err = client
        .call(&req(json!({ "cmd": "status" })), CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "computer-use helper did not start");
    // 40 attempts, 150 ms apart.
    assert!(
        started.elapsed() >= Duration::from_millis(5900),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn failing_open_is_reported() {
    let dir = short_tempdir();
    let app = dir.path().join("DSH Computer Use.app");
    std::fs::create_dir(&app).unwrap();
    let open = fake_open(dir.path(), 1);
    let client = CuaClient::with_socket_path(dir.path().join("cua.sock"))
        .with_app_path(&app)
        .with_open_program(&open);
    let err = client
        .call(&req(json!({ "cmd": "status" })), CallOptions::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with(&format!(
            "Command failed: {} -g {}",
            open.display(),
            app.display()
        )),
        "{err}"
    );
}

#[tokio::test]
async fn timeout_names_the_command() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |_| Reply::Hang);
    let client = CuaClient::with_socket_path(&helper.path);
    let started = Instant::now();
    let err = client
        .call(
            &req(json!({ "cmd": "app_state" })),
            CallOptions::timeout_ms(200),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "computer-use helper timed out on \"app_state\""
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn closing_without_an_answer_and_bad_json_fail() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |r| match r["cmd"].as_str() {
        Some("close") => Reply::Close,
        _ => Reply::Raw("{not json\n"),
    });
    let client = CuaClient::with_socket_path(&helper.path);
    let err = client
        .call(&req(json!({ "cmd": "close" })), CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "computer-use helper closed the connection without answering"
    );
    let err = client
        .call(&req(json!({ "cmd": "garbage" })), CallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CuaError::Json(_)), "{err}");
}

#[test]
fn default_paths_follow_home() {
    let home = std::env::var("HOME").unwrap_or_default();
    let socket = dshx_macos::client::default_socket_path();
    assert!(socket.ends_with("Library/Application Support/dsh-codex/cua.sock"));
    assert!(socket.starts_with(&home));
    assert!(dshx_macos::client::default_app_path().ends_with("Applications/DSH Computer Use.app"));
}
