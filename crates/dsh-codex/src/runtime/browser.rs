//! Saved versus active browser configuration. Browser changes are explicit
//! and never close a browser another chat is currently using.
use dshx_core::config::BrowserConfig;
use serde_json::{Value, json};

use super::Runtime;

pub(super) const ROUTING: &str = "For websites, use the browser tool or the browser MCP server selected for the task. Keep the requested browser, project, profile and account; a different backend does not share its cookies or traffic capture. Call connected MCP tools directly, not improvised shell wrappers. Do not switch to computer or app control of a browser unless the user explicitly asks for desktop interaction. A website HTTP error, blocked redirect, missing login, stale element, proxy error or unavailable connection does not authorize taking over the user's screen. Diagnose the specific cause, refresh browser state when indices are stale, and report the needed repair if browser control is unavailable. Browser Use settings can test the local browser and show MCP connections. Built-in browser results have a summary field; MCP results have content blocks and may have structuredContent. For HuntProxy, inspect current browser status and use the intended project/profile. browser_action navigate is for navigation; request uses the active page's same origin, preserves browser policies, and disables redirects. Its generic network error mentioning disabled redirects is not proof of a redirect. Check events/history and the page origin before retrying; never automatically replay a state-changing request or disable proxy protections to hide an error.";

impl Runtime {
    pub fn browser_status(&self) -> Value {
        let saved = self.config().codex.browser;
        let resolved = dshx_browser::find_chrome(saved.chrome.as_deref());
        let options = self.browser.as_ref().map(|tool| tool.browser().options());
        let active_chrome = options.and_then(|opts| opts.chrome.as_ref());
        let restart = saved.enabled != self.browser_at_start.enabled
            || saved.chrome != self.browser_at_start.chrome
            || (saved.enabled && resolved.as_ref() != active_chrome);
        json!({
            "configured": {"enabled":saved.enabled,"chrome":saved.chrome},
            "active": {
                "enabled": self.browser_at_start.enabled,
                "available": self.browser.is_some(),
                "chrome": active_chrome,
                "profile": options.map(|opts| opts.profile_dir.clone()).unwrap_or_else(dshx_browser::default_profile_dir),
                "running": self.browser.as_ref().and_then(|tool| tool.browser().pid()).is_some(),
            },
            "resolvedChrome":resolved,
            "restartRequired":restart,
        })
    }
}

/// Only the settings fields supported by Browser Use may be changed here.
/// In particular, never switch keychain modes on an existing signed-in profile.
pub(crate) fn parse_settings(
    value: &Value,
    current: &BrowserConfig,
) -> anyhow::Result<BrowserConfig> {
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("browser must be an object"))?;
    anyhow::ensure!(
        fields
            .keys()
            .all(|k| matches!(k.as_str(), "enabled" | "chrome")),
        "unknown browser setting"
    );
    let mut next = current.clone();
    if let Some(enabled) = fields.get("enabled") {
        next.enabled = enabled
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("browser.enabled must be a boolean"))?;
    }
    if let Some(chrome) = fields.get("chrome") {
        next.chrome = match chrome {
            Value::Null => None,
            Value::String(path) if path.trim().is_empty() => None,
            Value::String(path) => {
                let path = std::path::PathBuf::from(path.trim());
                anyhow::ensure!(
                    path.is_absolute(),
                    "browser.chrome must be an absolute executable path"
                );
                anyhow::ensure!(
                    dshx_browser::is_executable(&path),
                    "browser.chrome must point to an executable file on this instance"
                );
                Some(path)
            }
            _ => anyhow::bail!(
                "browser.chrome must be a path string or null for automatic detection"
            ),
        };
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_core::config::Config;
    use dshx_server::Backend;
    use dshx_session::{Credentials, FileAttachments, SessionStore};

    fn runtime(home: &std::path::Path, config: Config) -> Arc<Runtime> {
        Runtime::build(
            home.to_owned(),
            config,
            SessionStore::new(home.join("sessions")),
            FileAttachments::new(home),
            Credentials::new(home.join("credentials.toml")),
        )
        .unwrap()
    }

    use std::sync::Arc;

    #[test]
    fn browser_patch_is_strict_and_preserves_profile_options() {
        let before = BrowserConfig {
            width: 1440,
            mock_keychain: false,
            ..Default::default()
        };
        for invalid in [
            json!(null),
            json!({"enabled":"yes"}),
            json!({"chrome":7}),
            json!({"chrome":"relative"}),
            json!({"chrome":"/tmp"}),
            json!({"mockKeychain":true}),
        ] {
            assert!(parse_settings(&invalid, &before).is_err(), "{invalid}");
        }
        let after = parse_settings(&json!({"enabled":false,"chrome":null}), &before).unwrap();
        assert!(!after.enabled);
        assert_eq!(after.width, 1440);
        assert!(!after.mock_keychain);
    }

    #[tokio::test]
    async fn saved_browser_config_is_separate_from_active_and_reloads() {
        let home = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.codex.browser.enabled = false;
        config.codex.apps.enabled = false;
        config.codex.computer_use.enabled = false;
        let rt = runtime(home.path(), config);
        let backend = crate::web::WebBackend::new(rt.clone());
        assert_eq!(
            backend.browser_status().await.unwrap()["restartRequired"],
            false
        );
        let failed = backend.browser_test().await.unwrap();
        assert_eq!(failed["ok"], false);
        assert!(failed["error"].as_str().unwrap().contains("disabled"));
        let info = backend
            .update_settings(json!({"browser":{"enabled":true,"chrome":null}}))
            .await
            .unwrap();
        assert_eq!(info["settings"]["browser"]["enabled"], true);
        let state = backend.browser_status().await.unwrap();
        assert_eq!(state["configured"]["enabled"], true);
        assert_eq!(state["active"]["enabled"], false);
        assert_eq!(state["active"]["available"], false);
        assert_eq!(state["restartRequired"], true);
        let bytes = std::fs::read(home.path().join("config.toml")).unwrap();
        assert!(
            backend
                .update_settings(json!({"browser":{"chrome":"/not-a-real-executable/aether"}}))
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(home.path().join("config.toml")).unwrap(),
            bytes
        );
        let reloaded: Config = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let rt2 = runtime(home.path(), reloaded);
        assert_eq!(rt2.browser_status()["restartRequired"], false);
        rt2.shutdown().await;
        rt.shutdown().await;
    }
}
