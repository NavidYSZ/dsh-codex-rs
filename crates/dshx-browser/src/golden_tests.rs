//! Compares the port with golden data produced by running the original
//! `lib/browser-tool.js` / `lib/browser.js` under Node
//! (`tests/fixtures/browser-tool.json`).
use serde_json::Value;

use crate::browser::{Snap, Tab, describe_state, key_events, normalize_url};
use crate::tool::{ACTIONS, DESCRIPTION, TOOL_NAME, activity, describe, input_schema};

fn golden() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/browser-tool.json")).unwrap()
}

#[test]
fn name_description_and_schema_are_verbatim() {
    let g = golden();
    assert_eq!(g["name"], TOOL_NAME);
    assert_eq!(g["description"], DESCRIPTION);
    assert_eq!(g["actions"], serde_json::json!(ACTIONS));
    // Same keys in the same order, byte for byte.
    assert_eq!(
        serde_json::to_string(&g["schema"]).unwrap(),
        serde_json::to_string(&input_schema()).unwrap()
    );
}

#[test]
fn describe_labels_match() {
    for case in golden()["describe"].as_array().unwrap() {
        assert_eq!(
            describe(&case["input"]),
            case["label"].as_str().unwrap(),
            "input {}",
            case["input"]
        );
    }
}

#[test]
fn mini_activity_lines_match() {
    for case in golden()["activity"].as_array().unwrap() {
        let got = activity(
            &case["input"],
            case["locale"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        assert_eq!(got, case["text"].as_str().unwrap(), "case {case}");
    }
}

#[test]
fn key_events_match() {
    for case in golden()["keys"].as_array().unwrap() {
        let combo = case["combo"].as_str().unwrap();
        match key_events(combo) {
            Ok(events) => {
                let sent: Vec<Value> = case["sent"]
                    .as_array()
                    .expect("JS threw")
                    .iter()
                    .map(|s| s["params"].clone())
                    .collect();
                assert_eq!(
                    serde_json::to_string(&sent).unwrap(),
                    serde_json::to_string(&events).unwrap(),
                    "combo {combo:?}"
                );
            }
            Err(e) => assert_eq!(
                case["error"].as_str(),
                Some(e.to_string().as_str()),
                "combo {combo:?}"
            ),
        }
    }
}

#[test]
fn url_normalization_matches() {
    for case in golden()["urls"].as_array().unwrap() {
        let raw = case["raw"].as_str().unwrap();
        match normalize_url(raw) {
            Ok(url) => assert_eq!(case["url"].as_str(), Some(url.as_str()), "raw {raw:?}"),
            Err(e) => assert_eq!(
                case["error"].as_str(),
                Some(e.to_string().as_str()),
                "raw {raw:?}"
            ),
        }
    }
}

#[test]
fn page_outline_matches() {
    for case in golden()["describeState"].as_array().unwrap() {
        let snap: Snap = serde_json::from_value(case["snap"].clone()).unwrap();
        let tabs: Vec<Tab> = serde_json::from_value(case["tabs"].clone()).unwrap();
        let dialogs: Vec<String> = serde_json::from_value(case["dialogs"].clone()).unwrap();
        let text = describe_state(
            &snap,
            &tabs,
            case["current"].as_str(),
            case["visible"].as_bool().unwrap(),
            &dialogs,
        );
        assert_eq!(text, case["text"].as_str().unwrap());
    }
}

#[test]
fn host_and_jpeg_helpers() {
    use crate::tool::host_of;
    assert_eq!(host_of("http://127.0.0.1:8080/a?b"), "127.0.0.1:8080");
    assert_eq!(host_of("https://user:pw@example.com/"), "example.com");
    assert_eq!(host_of("about:blank"), "");
    assert_eq!(host_of("data:text/html,<a href='http://x/'>"), "");
    assert_eq!(host_of("chrome-error://chromewebdata/"), "chromewebdata");
    assert_eq!(host_of("file:///etc/hosts"), "");
}

#[test]
fn launch_flags_match_browser_js() {
    use crate::browser::{BrowserOptions, launch_args};
    let opts = BrowserOptions {
        chrome: None,
        profile_dir: "/p/profile".into(),
        width: 1280,
        height: 800,
        idle: std::time::Duration::from_secs(900),
        locale: "de".into(),
        quality: 0.7,
        mock_keychain: false,
    };
    // SAFETY: geteuid has no preconditions.
    let root = cfg!(target_os = "linux") && unsafe { libc::geteuid() } == 0;
    let mut headless = vec![
        "--user-data-dir=/p/profile",
        "--remote-debugging-pipe",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-sync",
        "--disable-features=Translate,MediaRouter,OptimizationHints",
        "--hide-crash-restore-bubble",
        "--window-size=1280,800",
        "--lang=de-DE",
        "--headless=new",
    ];
    if root {
        headless.push("--no-sandbox");
    }
    headless.push("about:blank");
    assert_eq!(launch_args(&opts, false), headless);
    let visible = launch_args(
        &BrowserOptions {
            locale: "en".into(),
            ..opts.clone()
        },
        true,
    );
    assert!(visible.contains(&"--window-size=1280,888".to_owned()));
    assert!(visible.contains(&"--lang=en-US".to_owned()));
    assert!(!visible.iter().any(|a| a.starts_with("--headless")));
    let mocked = launch_args(
        &BrowserOptions {
            mock_keychain: true,
            ..opts
        },
        false,
    );
    assert_eq!(
        mocked.contains(&"--use-mock-keychain".to_owned()),
        cfg!(target_os = "macos"),
        "only macOS has a keychain to keep out of"
    );
    assert!(!visible.iter().any(|a| a.contains("port")));
}
