//! `computer` and `app` through the `Tool` trait against a fake helper:
//! exact requests and output, configure once, errors, cancellation, schemas.
#![cfg(unix)]
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{FakeHelper, MemStore, Reply, context, respond, short_tempdir};
use dshx_core::config::CodexConfig;
use dshx_core::{Tool, ToolError};
use dshx_macos::{CuaClient, Helper, Settings};
use serde_json::{Value, json};

fn config() -> CodexConfig {
    let mut config = CodexConfig::default();
    config.computer_use.enabled = true;
    config.apps.enabled = true;
    config
}

fn tools_at(
    socket: &std::path::Path,
    config: &CodexConfig,
    locale: &str,
) -> (Arc<dyn Tool>, Arc<dyn Tool>) {
    let client = Arc::new(
        CuaClient::with_socket_path(socket).with_app_path(socket.with_extension("missing.app")),
    );
    let tools = dshx_macos::tools(&Helper::new(client, Settings::with_locale(config, locale)));
    assert_eq!(tools.len(), 2);
    (tools[0].clone(), tools[1].clone())
}

const MINI_EN: &str = r#""mini":{"doneAfter":6,"hideAfter":24,"done":"Done","stopped":"Stopped","stop":"Stop","open":"Show"}"#;

#[tokio::test]
async fn click_output_and_configure_once() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let (computer, app) = tools_at(&helper.path, &config(), "en");
    let store = Arc::new(MemStore::default());

    let out = computer
        .execute(
            json!({ "action": "click", "x": 100, "y": 200 }),
            context(store.clone(), true),
        )
        .await
        .unwrap();
    let summary = "Click (100, 200) — done. Screenshot 1280×800px of the display; frontmost app: Finder. Agent cursor at (100, 200).";
    let image = json!({ "attachmentId": "att-1", "mediaType": "image/jpeg", "bytes": 3, "width": 1280, "height": 800, "name": "screen.jpg" });
    assert_eq!(
        out.value.unwrap().to_string(),
        json!({ "summary": summary, "cursor": { "x": 100, "y": 200 }, "screen": { "width": 1280, "height": 800 }, "image": image })
            .to_string()
    );
    assert_eq!(
        out.meta.unwrap().to_string(),
        json!({ "tool": "computer", "action": "click", "label": "Click (100, 200)", "x": 100, "y": 200, "toX": null, "toY": null,
                "screen": { "width": 1280, "height": 800 }, "cursor": { "x": 100, "y": 200 }, "place": null, "target": null })
            .to_string()
    );
    assert_eq!(
        serde_json::to_value(&out.content).unwrap(),
        json!([{ "type": "text", "text": summary }, { "type": "image", "attachment": image }])
    );
    assert!(!out.is_error);

    computer
        .execute(
            json!({ "action": "cursor_position" }),
            context(store.clone(), true),
        )
        .await
        .unwrap();
    app.execute(
        json!({ "action": "list_apps" }),
        context(store.clone(), true),
    )
    .await
    .unwrap();
    let listed = app
        .execute(
            json!({ "action": "list_apps" }),
            context(store.clone(), true),
        )
        .await
        .unwrap();
    assert_eq!(
        listed.value.unwrap(),
        json!({ "summary": "Running apps:\nNotes (com.apple.Notes), in front\nRechner (com.apple.calculator), hidden" })
    );

    assert_eq!(
        helper.lines(),
        vec![
            format!(
                r##"{{"id":1,"cmd":"configure","accent":"#339cff","label":"DeepSeek","banner":"DeepSeek is using your computer","escHint":"Esc to cancel","showLabel":false,"showGlow":false,"showActivity":true,"clickPulse":true,"maxWidth":1280,{MINI_EN}}}"##
            ),
            r#"{"id":2,"cmd":"click","x":100,"y":200,"button":"left","count":1,"modifiers":[],"screenshotAfter":true,"settle":0.5,"maxWidth":1280,"quality":0.8,"activity":"Clicking"}"#.to_owned(),
            r#"{"id":3,"cmd":"cursor_position"}"#.to_owned(),
            format!(r##"{{"id":4,"cmd":"configure","accent":"#339cff","maxWidth":1280,{MINI_EN}}}"##),
            r#"{"id":5,"cmd":"app_list"}"#.to_owned(),
            r#"{"id":6,"cmd":"app_list"}"#.to_owned(),
        ]
    );
}

#[tokio::test]
async fn text_only_models_get_no_image() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let (computer, app) = tools_at(&helper.path, &config(), "en");
    let store = Arc::new(MemStore::default());
    let out = computer
        .execute(
            json!({ "action": "screenshot" }),
            context(store.clone(), false),
        )
        .await
        .unwrap();
    let value = out.value.unwrap();
    assert!(value["summary"].as_str().unwrap().ends_with(
        "Agent cursor at (10, 20). (The current model cannot see images; switch to an image-capable model for computer use.)"
    ));
    assert_eq!(value["screen"], json!({ "width": 1280, "height": 800 }));
    assert!(value.get("image").is_none());
    assert_eq!(out.content.len(), 1);
    // `app` has no text-only hint.
    let out = app
        .execute(
            json!({ "action": "state", "app": "Notes" }),
            context(store.clone(), false),
        )
        .await
        .unwrap();
    assert!(
        out.value.unwrap()["summary"]
            .as_str()
            .unwrap()
            .ends_with("[2] AXButton \"\" @(5,5)")
    );
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scaled_attachments_get_a_note() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let (_, app) = tools_at(&helper.path, &config(), "de");
    let out = app
        .execute(
            json!({ "action": "state", "app": "Notes" }),
            context(Arc::new(MemStore::default()), true),
        )
        .await
        .unwrap();
    let value = out.value.unwrap();
    let summary = value["summary"].as_str().unwrap();
    assert!(summary.starts_with("App: Notes (com.apple.Notes), in the background\nWindow 42: \"Einkauf\" 800×600 px screenshot\nOther windows: Notizen | Ideen\nOutline, [index] role \"title\" @(x,y) in screenshot pixels:\n[0] AXButton \"Sichern\""));
    assert!(summary.ends_with(" (The attached image is scaled to 640×400; keep using coordinates of the 800×600 screenshot.)"));
    assert_eq!(value["place"], json!("Notes"));
    let request: Value = serde_json::from_str(&helper.lines()[1]).unwrap();
    assert_eq!(request["activity"], json!("Sieht sich das Fenster an"));
}

#[tokio::test]
async fn computer_configure_failure_fails_the_call_and_is_retried() {
    let dir = short_tempdir();
    let socket = dir.path().join("cua.sock");
    let (computer, _) = tools_at(&socket, &config(), "en");
    let store = Arc::new(MemStore::default());
    let err = computer
        .execute(
            json!({ "action": "screenshot" }),
            context(store.clone(), true),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(ref m) if m.starts_with("computer-use helper not installed at ")),
        "{err}"
    );

    let helper = FakeHelper::start(&socket, respond);
    computer
        .execute(
            json!({ "action": "screenshot" }),
            context(store.clone(), true),
        )
        .await
        .unwrap();
    computer
        .execute(json!({ "action": "screenshot" }), context(store, true))
        .await
        .unwrap();
    let cmds: Vec<Value> = helper.requests().iter().map(|r| r["cmd"].clone()).collect();
    assert_eq!(
        cmds,
        vec![json!("configure"), json!("screenshot"), json!("screenshot")]
    );
}

#[tokio::test]
async fn app_setup_failure_is_swallowed_and_retried() {
    let dir = short_tempdir();
    let socket = dir.path().join("cua.sock");
    let (_, app) = tools_at(&socket, &config(), "en");
    let store = Arc::new(MemStore::default());
    // Setup fails silently; the action itself then reports the missing helper.
    let err = app
        .execute(
            json!({ "action": "list_apps" }),
            context(store.clone(), true),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("computer-use helper not installed at "),
        "{err}"
    );

    let helper = FakeHelper::start(&socket, respond);
    app.execute(
        json!({ "action": "list_apps" }),
        context(store.clone(), true),
    )
    .await
    .unwrap();
    app.execute(json!({ "action": "list_apps" }), context(store, true))
        .await
        .unwrap();
    let cmds: Vec<Value> = helper.requests().iter().map(|r| r["cmd"].clone()).collect();
    assert_eq!(
        cmds,
        vec![json!("configure"), json!("app_list"), json!("app_list")]
    );
}

#[tokio::test]
async fn configure_answered_with_an_error_still_counts() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |r| match r["cmd"].as_str() {
        Some("configure") => Reply::Json(json!({ "ok": false, "error": "nope" })),
        _ => respond(r),
    });
    let (computer, app) = tools_at(&helper.path, &config(), "en");
    let store = Arc::new(MemStore::default());
    for _ in 0..2 {
        computer
            .execute(
                json!({ "action": "cursor_position" }),
                context(store.clone(), true),
            )
            .await
            .unwrap();
        app.execute(
            json!({ "action": "list_apps" }),
            context(store.clone(), true),
        )
        .await
        .unwrap();
    }
    let configures = helper
        .requests()
        .iter()
        .filter(|r| r["cmd"] == json!("configure"))
        .count();
    assert_eq!(configures, 2, "one for computer, one for app");
}

#[tokio::test]
async fn helper_errors_and_user_stop_reach_the_model() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let (computer, app) = tools_at(&helper.path, &config(), "en");
    let store = Arc::new(MemStore::default());
    let err = computer
        .execute(
            json!({ "action": "key", "text": "CANCEL" }),
            context(store.clone(), true),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)));
    assert_eq!(
        err.to_string(),
        "The user stopped the agent (Esc or the Stop button). Stop operating the computer, browser and apps, and ask the user how to continue."
    );
    let err = app
        .execute(
            json!({ "action": "key", "keys": "SILENT" }),
            context(store.clone(), true),
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "app control failed");
    let err = computer
        .execute(
            json!({ "action": "drag", "x": 1, "y": 2, "to_x": 3 }),
            context(store, true),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(ref m) if m == "\"drag\" needs numeric \"to_y\""),
        "{err}"
    );
}

#[tokio::test]
async fn cancellation_stops_waiting_for_the_helper() {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), |r| match r["cmd"].as_str() {
        Some("configure") => Reply::Json(json!({ "ok": true })),
        _ => Reply::Hang,
    });
    let (computer, app) = tools_at(&helper.path, &config(), "en");
    for tool in [computer, app] {
        let ctx = context(Arc::new(MemStore::default()), true);
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.cancel();
        });
        let started = Instant::now();
        let args = if tool.schema().name == "computer" {
            json!({ "action": "wait", "seconds": 10 })
        } else {
            json!({ "action": "show" })
        };
        let err = tool.execute(args, ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::Cancelled), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

#[test]
fn schemas_match_the_js_fixture() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/js-schemas.json")).unwrap();
    let tools = dshx_macos::tools(&Helper::new(
        Arc::new(CuaClient::with_socket_path("/nonexistent")),
        Settings::with_locale(&config(), "en"),
    ));
    let ours: Vec<Value> = tools
        .iter()
        .map(|t| {
            let s = t.schema();
            json!({ "name": s.name, "description": s.description, "parameters": s.parameters })
        })
        .collect();
    // Serialized comparison: key order of the schemas counts too.
    assert_eq!(Value::Array(ours).to_string(), fixture.to_string());
    for tool in &tools {
        assert_eq!(tool.timeout(), Some(Duration::from_secs(120)));
        assert!(!tool.concurrency_safe());
        assert_eq!(tool.describe(&json!({})), tool.schema().name);
    }
}

#[test]
fn only_enabled_tools_and_only_on_macos() {
    let client = Arc::new(CuaClient::with_socket_path("/nonexistent"));
    let mut only_app = config();
    only_app.computer_use.enabled = false;
    let tools = dshx_macos::tools(&Helper::new(client, Settings::with_locale(&only_app, "en")));
    assert_eq!(
        tools.iter().map(|t| t.schema().name).collect::<Vec<_>>(),
        vec!["app"]
    );

    let names: Vec<String> = dshx_macos::macos_tools(&CodexConfig::default())
        .iter()
        .map(|t| t.schema().name)
        .collect();
    if cfg!(target_os = "macos") {
        assert_eq!(names, vec!["computer", "app"]);
    } else {
        assert!(names.is_empty());
    }
}
