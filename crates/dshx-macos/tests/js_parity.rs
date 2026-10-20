//! Differential tests against the original plugin: the same scenario runs
//! through `index.js` (Node) and through the Rust tools, each against the same
//! fake helper; helper requests (byte for byte), values, presentation meta,
//! model content, stored images and error texts must match.
//! Skipped when `node` is not installed.
#![cfg(unix)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{FakeHelper, MemStore, context, respond, short_tempdir};
use dshx_core::config::CodexConfig;
use dshx_macos::{CuaClient, Helper, Settings, locale};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn have_node() -> bool {
    let ok = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipped: node is not installed");
    }
    ok
}

fn socket_in(home: &Path) -> PathBuf {
    home.join("Library/Application Support/dsh-codex/cua.sock")
}

async fn run_js(scenario: &Value, env: &[(&str, &str)]) -> (Value, Vec<String>) {
    let home = short_tempdir();
    let helper = FakeHelper::start(&socket_in(home.path()), respond);
    let file = home.path().join("scenario.json");
    std::fs::write(&file, serde_json::to_vec(scenario).unwrap()).unwrap();
    let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/harness.mjs");
    let out = tokio::process::Command::new("node")
        .arg(harness)
        .arg(repo_root())
        .arg(&file)
        .env("HOME", home.path())
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env_remove("LANG")
        .envs(env.iter().copied())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (serde_json::from_slice(&out.stdout).unwrap(), helper.lines())
}

async fn run_rust(scenario: &Value, locale: Option<String>) -> (Value, Vec<String>) {
    let dir = short_tempdir();
    let helper = FakeHelper::start(&dir.path().join("cua.sock"), respond);
    let config: CodexConfig = serde_json::from_value(scenario["config"].clone()).unwrap();
    let settings = match locale {
        Some(locale) => Settings::with_locale(&config, locale),
        None => Settings::new(&config),
    };
    let tools = dshx_macos::tools(&Helper::new(
        Arc::new(CuaClient::with_socket_path(&helper.path)),
        settings,
    ));
    let store = Arc::new(MemStore::default());
    let mut results = Vec::new();
    for step in scenario["steps"].as_array().unwrap() {
        let tool = tools
            .iter()
            .find(|t| t.schema().name == step["tool"])
            .expect("tool registered");
        let ctx = context(store.clone(), step.get("images") != Some(&json!(false)));
        results.push(match tool.execute(step["args"].clone(), ctx).await {
            Ok(out) => {
                json!({ "ok": true, "value": out.value, "meta": out.meta, "content": out.content })
            }
            Err(err) => json!({ "ok": false, "error": err.to_string() }),
        });
    }
    let names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
    let saved = store.saved.lock().unwrap().clone();
    (
        json!({ "tools": names, "results": results, "saved": saved }),
        helper.lines(),
    )
}

fn compare(scenario: &Value, js: &(Value, Vec<String>), rust: &(Value, Vec<String>)) {
    let (js, js_lines) = js;
    let (rust, rust_lines) = rust;
    assert_eq!(rust["tools"], js["tools"]);
    let steps = scenario["steps"].as_array().unwrap();
    assert_eq!(js["results"].as_array().unwrap().len(), steps.len());
    assert_eq!(rust["results"].as_array().unwrap().len(), steps.len());
    assert!(
        js_lines.len() >= steps.len() / 2,
        "the harness reached the fake helper"
    );
    for (i, step) in steps.iter().enumerate() {
        let (j, r) = (&js["results"][i], &rust["results"][i]);
        let what = format!("step {i}: {}", step);
        assert_eq!(r["ok"], j["ok"], "{what}\njs: {j}\nrust: {r}");
        if j["ok"] == json!(true) {
            for key in ["value", "meta", "content"] {
                // Compare serialized text so key order counts too.
                assert_eq!(r[key].to_string(), j[key].to_string(), "{what}: {key}");
            }
        } else {
            assert_eq!(r["error"], j["error"], "{what}: error");
        }
    }
    assert_eq!(rust["saved"], js["saved"], "stored images");
    assert_eq!(
        rust_lines.len(),
        js_lines.len(),
        "number of helper requests\njs: {js_lines:#?}\nrust: {rust_lines:#?}"
    );
    for (r, j) in rust_lines.iter().zip(js_lines) {
        assert_eq!(r, j, "helper request");
    }
}

fn step(tool: &str, args: Value) -> Value {
    json!({ "tool": tool, "args": args })
}

fn text_only(tool: &str, args: Value) -> Value {
    json!({ "tool": tool, "args": args, "images": false })
}

fn full_scenario() -> Value {
    let c = |args| step("computer", args);
    let a = |args| step("app", args);
    json!({
        "config": {
            "computerUse": { "enabled": true, "locale": "de", "blockedApps": ["com.example.secret"],
                             "maxWidth": 1024, "showLabel": true, "accent": "#ff6600" },
            "apps": { "enabled": true },
            "browser": { "enabled": false },
            "mini": { "doneAfter": 5 }
        },
        "steps": [
            c(json!({ "action": "screenshot" })),
            c(json!({ "action": "click", "x": 100, "y": 200.0 })),
            c(json!({ "action": "double_click", "x": 10.4, "y": 20.6, "modifiers": ["cmd", "shift"] })),
            text_only("computer", json!({ "action": "right_click", "x": 1, "y": 2 })),
            c(json!({ "action": "move", "x": 5, "y": 5 })),
            c(json!({ "action": "drag", "x": 1, "y": 2, "to_x": 300.5, "to_y": 400 })),
            c(json!({ "action": "scroll", "x": 50, "y": 60, "dy": 120.5, "dx": -2.5 })),
            c(json!({ "action": "scroll", "x": 50, "y": 60 })),
            c(json!({ "action": "type", "text": "Hallo Welt, this is a long text that exceeds forty characters 😀" })),
            c(json!({ "action": "key", "text": "cmd+l" })),
            c(json!({ "action": "open_app", "text": "Safari" })),
            c(json!({ "action": "wait", "seconds": 0.01 })),
            c(json!({ "action": "cursor_position" })),
            c(json!({ "action": "click", "x": 5 })),
            c(json!({ "action": "type" })),
            c(json!({ "action": "fly" })),
            c(json!({ "action": "key", "text": "FAIL" })),
            c(json!({ "action": "key", "text": "CANCEL" })),
            c(json!({ "action": "open_app", "text": "SILENT" })),
            a(json!({ "action": "list_apps" })),
            a(json!({ "action": "state", "app": "Notes" })),
            a(json!({ "action": "click", "index": 0 })),
            a(json!({ "action": "click", "x": 10.5, "y": 20 })),
            a(json!({ "action": "type", "index": 1, "text": "Milch und ein sehr langer Einkaufszettel für die Woche" })),
            a(json!({ "action": "set_value", "index": 1, "value": "Brot" })),
            a(json!({ "action": "key", "keys": "Return" })),
            a(json!({ "action": "scroll", "direction": "up", "amount": 2 })),
            a(json!({ "action": "scroll", "index": 1 })),
            a(json!({ "action": "menu", "path": ["Ablage"] })),
            a(json!({ "action": "menu", "path": ["Ablage", "Neue Notiz"] })),
            a(json!({ "action": "menu" })),
            a(json!({ "action": "action", "index": 2, "name": "AXIncrement" })),
            a(json!({ "action": "show" })),
            a(json!({ "action": "state", "app": "Empty", "window": "Einkauf" })),
            a(json!({ "action": "click", "index": 0 })),
            a(json!({ "action": "state" })),
            a(json!({ "action": "type", "text": 5 })),
            a(json!({ "action": "key" })),
            a(json!({ "action": "zoom" })),
            text_only("app", json!({ "action": "click", "index": 0, "count": 2 })),
            a(json!({ "action": "state", "app": "FAIL" })),
            a(json!({ "action": "key", "keys": "CANCEL" })),
            a(json!({ "action": "key", "keys": "SILENT" }))
        ]
    })
}

#[tokio::test]
async fn computer_and_app_match_the_js_plugin() {
    if !have_node() {
        return;
    }
    let scenario = full_scenario();
    let js = run_js(&scenario, &[]).await;
    let rust = run_rust(&scenario, None).await;
    compare(&scenario, &js, &rust);
}

#[tokio::test]
async fn system_locale_and_defaults_match_the_js_plugin() {
    if !have_node() {
        return;
    }
    // No locale configured: both sides take it from LANG. Default accent and
    // strings, no blockedApps; app first, so its setup configure comes first.
    let scenario = json!({
        "config": {
            "computerUse": { "enabled": true, "banner": "Custom banner", "showGlow": true, "quality": 0.6, "maxWidth": 1440 },
            "apps": { "enabled": true },
            "browser": { "enabled": false }
        },
        "steps": [
            step("app", json!({ "action": "list_apps" })),
            step("computer", json!({ "action": "screenshot" })),
            step("computer", json!({ "action": "type", "text": "x" })),
            step("app", json!({ "action": "state", "app": "Notes" })),
            step("app", json!({ "action": "type", "text": "Grüße" }))
        ]
    });
    let env = [("LANG", "de_DE.UTF-8")];
    let js = run_js(&scenario, &env).await;
    let locale = locale::detect_locale(
        |name| (name == "LANG").then(|| "de_DE.UTF-8".to_owned()),
        false,
        || None,
    );
    let rust = run_rust(&scenario, Some(locale)).await;
    compare(&scenario, &js, &rust);
}

#[tokio::test]
async fn schemas_match_the_js_sources() {
    if !have_node() {
        return;
    }
    let script = "import * as c from './lib/computer-tool.js'; import * as a from './lib/app-tool.js';\
        process.stdout.write(JSON.stringify([c, a].map((m) => ({ name: m.TOOL_NAME, description: m.DESCRIPTION, parameters: m.INPUT_SCHEMA }))));";
    let out = tokio::process::Command::new("node")
        .args(["--input-type=module", "-e", script])
        .current_dir(repo_root())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let js = String::from_utf8(out.stdout).unwrap();
    let fixture: Value = serde_json::from_str(include_str!("fixtures/js-schemas.json"))
        .expect("fixture is valid JSON");
    assert_eq!(
        fixture.to_string(),
        js,
        "tests/fixtures/js-schemas.json is out of date with lib/*.js"
    );
}

#[tokio::test]
async fn mini_requests_match_browser_js() {
    if !have_node() {
        return;
    }
    // No "show" requests, so browser.js never tries to open a window.
    let quiet =
        |_: &Value| common::Reply::Json(json!({ "ok": true, "cancelled": false, "requests": [] }));
    let home = short_tempdir();
    let js_helper = FakeHelper::start(&socket_in(home.path()), quiet);
    let out = tokio::process::Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/mini.mjs"))
        .arg(repo_root())
        .env("HOME", home.path())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "mini.mjs failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dir = short_tempdir();
    let rust_helper = FakeHelper::start(&dir.path().join("cua.sock"), quiet);
    let sink = dshx_macos::HelperMiniSink::new(Helper::new(
        Arc::new(CuaClient::with_socket_path(&rust_helper.path)),
        Settings::with_locale(&CodexConfig::default(), "de"),
    ));
    sink.show(
        "Browser · example.com",
        "globe",
        1280,
        800,
        Some("Klickt „Suche“"),
    )
    .await
    .unwrap();
    sink.step(640.0 / 1280.0, 200.0 / 800.0, true)
        .await
        .unwrap();
    sink.show("Browser · example.com", "globe", 1280, 800, Some("Liest"))
        .await
        .unwrap();
    sink.step(1280.0 / 1280.0, 0.0 / 800.0, false)
        .await
        .unwrap();
    sink.show("Browser · example.com", "globe", 1280, 800, Some("Scrollt"))
        .await
        .unwrap();
    sink.frame("AAAA").await.unwrap();
    sink.poll().await.unwrap();

    assert_eq!(js_helper.lines().len(), 7, "{:#?}", js_helper.lines());
    assert_eq!(rust_helper.lines(), js_helper.lines());
}
