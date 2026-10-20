//! The helper-backed mini window for the background browser.
#![cfg(unix)]
mod common;

use std::sync::Arc;

use common::{FakeHelper, MemStore, Reply, context, respond, short_tempdir};
use dshx_core::config::CodexConfig;
use dshx_macos::{CuaClient, Helper, HelperMiniSink, Settings};
use serde_json::json;

#[tokio::test]
async fn sends_the_requests_of_browser_js() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let client = Arc::new(CuaClient::with_socket_path(&helper.path));
    let sink = HelperMiniSink::new(Helper::new(
        client,
        Settings::with_locale(&CodexConfig::default(), "de"),
    ));
    assert_eq!(sink.locale(), "de");

    let shown = sink
        .show(
            "Browser · example.com",
            "globe",
            1280,
            800,
            Some("Klickt „Suche“"),
        )
        .await
        .unwrap();
    assert!(shown.ok && shown.wants_show() && shown.stopped().is_none());
    sink.step(0.5, 0.25, true).await.unwrap();
    sink.step(1.0, 0.0, false).await.unwrap();
    sink.frame("AAAA").await.unwrap();
    let polled = sink.poll().await.unwrap();
    assert_eq!(polled.visible, Some(false));

    assert_eq!(
        helper.lines(),
        vec![
            r#"{"id":1,"cmd":"mini_show","title":"Browser · example.com","icon":"globe","width":1280,"height":800,"activity":"Klickt „Suche“"}"#,
            r#"{"id":2,"cmd":"mini_step","x":0.5,"y":0.25,"click":true}"#,
            r#"{"id":3,"cmd":"mini_step","x":1,"y":0,"click":false}"#,
            r#"{"id":4,"cmd":"mini_frame","data":"AAAA"}"#,
            r#"{"id":5,"cmd":"mini_poll"}"#,
        ]
    );
}

#[tokio::test]
async fn frames_and_polls_never_start_the_helper() {
    let dir = short_tempdir();
    // An installed-looking app, so only `launch: false` keeps it from starting.
    let app = dir.path().join("DSH Computer Use.app");
    std::fs::create_dir(&app).unwrap();
    let client = CuaClient::with_socket_path(dir.path().join("cua.sock"))
        .with_app_path(&app)
        .with_open_program("/nonexistent/open");
    let sink = HelperMiniSink::new(Helper::new(
        Arc::new(client),
        Settings::with_locale(&CodexConfig::default(), "en"),
    ));
    assert!(sink.frame("AAAA").await.unwrap_err().helper_not_running());
    assert!(sink.poll().await.unwrap_err().helper_not_running());
    // `mini_show` does try to start it.
    let err = sink
        .show("Browser", "globe", 1280, 800, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with("spawn /nonexistent/open"),
        "{err}"
    );
}

#[tokio::test]
async fn stop_in_the_mini_window_is_reported() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |r| match r["cmd"].as_str() {
        Some("mini_step") => Reply::Json(
            json!({ "ok": false, "cancelled": true, "error": "The user stopped the agent." }),
        ),
        _ => Reply::Json(json!({ "ok": true, "cancelled": true, "requests": [] })),
    });
    let sink = HelperMiniSink::new(Helper::new(
        Arc::new(CuaClient::with_socket_path(&helper.path)),
        Settings::with_locale(&CodexConfig::default(), "en"),
    ));
    let reply = sink.step(0.1, 0.1, false).await.unwrap();
    assert_eq!(reply.stopped(), Some("The user stopped the agent."));
    // `cancelled` on a successful frame is information only.
    let reply = sink.frame("AAAA").await.unwrap();
    assert!(reply.cancelled && reply.stopped().is_none() && !reply.wants_show());
}

#[tokio::test]
async fn setup_is_shared_with_the_app_tool() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let mut config = CodexConfig::default();
    config.apps.enabled = true;
    config.computer_use.enabled = false;
    config.computer_use.blocked_apps = vec!["com.example.vault".into()];
    let shared = Helper::new(
        Arc::new(CuaClient::with_socket_path(&helper.path)),
        Settings::with_locale(&config, "en"),
    );
    let sink = HelperMiniSink::new(shared.clone());
    sink.setup().await;
    sink.setup().await;
    let app = dshx_macos::tools(&shared).pop().unwrap();
    app.execute(
        json!({ "action": "list_apps" }),
        context(Arc::new(MemStore::default()), true),
    )
    .await
    .unwrap();
    assert_eq!(
        helper.lines(),
        vec![
            r##"{"id":1,"cmd":"configure","accent":"#339cff","maxWidth":1280,"mini":{"doneAfter":6,"hideAfter":24,"done":"Done","stopped":"Stopped","stop":"Stop","open":"Show"},"blockedApps":["com.example.vault"]}"##,
            r#"{"id":2,"cmd":"app_list"}"#,
        ]
    );
}

#[test]
fn only_offered_on_macos() {
    assert_eq!(
        HelperMiniSink::for_config(&CodexConfig::default()).is_some(),
        cfg!(target_os = "macos")
    );
}
