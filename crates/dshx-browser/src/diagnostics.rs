//! A real, isolated browser check. It never opens the user's browser profile
//! or a remote website, and uses the same actions as the model-facing tool.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use dshx_core::config::BrowserConfig;
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::{Browser, BrowserOptions, NoMini, tool};

const LIMIT: Duration = Duration::from_secs(45);
const MARKER: &str = "AETHER_BROWSER_CHECK_OK";
const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>Aether browser check</title>
<main><label>Test entry <input aria-label="Test entry" id="entry"></label>
<button id="verify" onclick="document.querySelector('output').textContent='Result: '+document.querySelector('input').value">Verify entry</button>
<output>Result: waiting</output></main>"#;

#[derive(Debug, Clone, Serialize)]
pub struct BrowserCheck {
    pub name: &'static str,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserDiagnostic {
    pub ok: bool,
    pub checked_at: u64,
    pub duration_ms: u64,
    pub checks: Vec<BrowserCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub chrome: Option<String>,
    pub isolated: bool,
}

struct Fixture(JoinHandle<()>);

impl Drop for Fixture {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn fixture() -> Result<(Fixture, String)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            // No uploaded bytes or arbitrary paths are interpreted. A client
            // that never finishes its request cannot hold this fixture open.
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await?;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{PAGE}",
                    PAGE.len()
                );
                stream.write_all(response.as_bytes()).await
            })
            .await;
        }
    });
    Ok((Fixture(task), format!("http://{address}/")))
}

fn index(text: &str, name: &str) -> Result<u64> {
    text.lines()
        .find(|line| line.contains(name) && line.contains('['))
        .and_then(|line| line.split_once('['))
        .and_then(|(_, rest)| rest.split_once(']'))
        .and_then(|(number, _)| number.parse().ok())
        .ok_or_else(|| anyhow!("the test page did not expose the {name} control"))
}

async fn actions(browser: &Browser, url: &str, report: &mut BrowserDiagnostic) -> Result<()> {
    let settings = tool::Settings {
        locale: "en".into(),
        quality: 0.5,
    };
    let state = tool::run(browser, &json!({"action":"state"}), &settings)
        .await
        .context("starting Chrome and connecting to DevTools")?;
    ensure!(state.image.is_some(), "the browser returned no screenshot");
    report.checks[0].ok = true;
    let page = tool::run(browser, &json!({"action":"open","url":url}), &settings)
        .await
        .context("opening the local test page")?;
    let entry = index(&page.text, "Test entry")?;
    report.checks[1].ok = true;
    let page = tool::run(
        browser,
        &json!({"action":"type","index":entry,"text":MARKER,"clear":true}),
        &settings,
    )
    .await
    .context("typing into the test field")?;
    ensure!(
        page.text.contains(MARKER),
        "the typed value was not visible"
    );
    report.checks[2].ok = true;
    let button = index(&page.text, "Verify entry")?;
    tool::run(
        browser,
        &json!({"action":"click","index":button}),
        &settings,
    )
    .await
    .context("clicking the test button")?;
    report.checks[3].ok = true;
    let page = tool::run(browser, &json!({"action":"read"}), &settings)
        .await
        .context("reading the test result")?;
    ensure!(
        page.text.contains(&format!("Result: {MARKER}")),
        "the test button did not produce the expected readable result"
    );
    report.checks[4].ok = true;
    Ok(())
}

/// Run with saved settings in a temporary profile. The caller should retain
/// this future through cleanup (the HTTP host runs it in a bounded task).
pub async fn diagnose(config: &BrowserConfig) -> BrowserDiagnostic {
    let start = Instant::now();
    let chrome = crate::find_chrome(config.chrome.as_deref());
    let mut report = BrowserDiagnostic {
        ok: false,
        checked_at: dshx_core::now_ms(),
        duration_ms: 0,
        checks: ["launch", "open", "type", "click", "read"]
            .into_iter()
            .map(|name| BrowserCheck { name, ok: false })
            .collect(),
        error: None,
        chrome: chrome.as_ref().map(|p| p.display().to_string()),
        isolated: true,
    };
    let result: Result<()> = async {
        ensure!(config.enabled, "the background browser is disabled");
        ensure!(
            chrome.is_some(),
            "no executable Chrome/Chromium browser was found"
        );
        let profile = tempfile::Builder::new()
            .prefix("aether-browser-check-")
            .tempdir()?;
        let (fixture, url) = fixture().await.context("starting the local test page")?;
        let mut options = BrowserOptions::from_config(config, "en");
        options.profile_dir = profile.path().join("profile");
        options.mock_keychain = true;
        let browser = Browser::new(options, Arc::new(NoMini));
        let result = tokio::time::timeout(LIMIT, actions(&browser, &url, &mut report))
            .await
            .map_err(|_| anyhow!("the browser check timed out after 45 seconds"))
            .and_then(|r| r);
        // Closing Chrome also reaps its process group before TempDir removal.
        browser.close().await;
        drop(browser);
        drop(fixture);
        profile
            .close()
            .context("removing the temporary browser profile")?;
        result
    }
    .await;
    report.ok = result.is_ok();
    report.error = result.err().map(|e| format!("{e:#}"));
    report.duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_and_missing_binary_are_not_successes() {
        let disabled = diagnose(&BrowserConfig {
            enabled: false,
            ..Default::default()
        })
        .await;
        assert!(!disabled.ok);
        assert!(disabled.checks.iter().all(|c| !c.ok));
        let missing = diagnose(&BrowserConfig {
            chrome: Some(std::path::PathBuf::from("/does-not-exist/aether-chrome")),
            ..Default::default()
        })
        .await;
        assert!(!missing.ok);
        assert!(missing.chrome.is_none());
        assert!(missing.error.unwrap().contains("no executable"));
    }

    #[tokio::test]
    async fn invalid_executable_reports_launch_failure() {
        let config = BrowserConfig {
            chrome: Some("/usr/bin/false".into()),
            ..Default::default()
        };
        if !crate::is_executable(config.chrome.as_ref().unwrap()) {
            return;
        }
        let result = diagnose(&config).await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("starting Chrome"));
    }
}
