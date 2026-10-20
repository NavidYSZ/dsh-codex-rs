//! Settings the tools send to the helper, and the once-per-process `configure`.
use std::sync::{Arc, OnceLock};

use dshx_core::config::CodexConfig;
use serde_json::Value;

use crate::client::{CallOptions, CuaClient, CuaError};
use crate::js::{self, Obj};
use crate::locale::{self, Strings};

/// The `[codex]` settings with the locale resolved (`computerUse.locale` or
/// the system language) and the localized helper strings filled in.
#[derive(Debug, Clone)]
pub struct Settings {
    config: CodexConfig,
    locale: String,
    strings: Strings,
}

impl Settings {
    /// `config.computerUse.locale ?? systemLocale()`.
    pub fn new(config: &CodexConfig) -> Self {
        let locale = config
            .computer_use
            .locale
            .clone()
            .unwrap_or_else(locale::system_locale);
        Settings::with_locale(config, locale)
    }

    /// Settings with an explicit locale (no system lookup).
    pub fn with_locale(config: &CodexConfig, locale: impl Into<String>) -> Self {
        let locale = locale.into();
        Settings {
            config: config.clone(),
            strings: locale::strings(&locale),
            locale,
        }
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    pub fn config(&self) -> &CodexConfig {
        &self.config
    }

    pub fn computer_enabled(&self) -> bool {
        self.config.computer_use.enabled
    }

    pub fn apps_enabled(&self) -> bool {
        self.config.apps.enabled
    }

    /// `computerUse.maxWidth` as sent to the helper.
    pub(crate) fn max_width(&self) -> Value {
        Value::from(self.config.computer_use.max_width)
    }

    /// `computerUse.quality`; printed via its shortest decimal so 0.8 stays 0.8.
    pub(crate) fn quality(&self) -> Value {
        let q = self.config.computer_use.quality;
        js::num(q.to_string().parse().unwrap_or(f64::from(q)))
    }

    /// `{ ...DEFAULTS.mini, ...miniStrings, ...config.mini }`
    fn mini(&self) -> Value {
        let mini = &self.config.mini;
        Obj::new()
            .set("doneAfter", mini.done_after)
            .set("hideAfter", mini.hide_after)
            .set("done", self.strings.done)
            .set("stopped", self.strings.stopped)
            .set("stop", self.strings.stop)
            .set("open", self.strings.open)
            .into_value()
    }

    /// `blockedApps` is only sent when configured (the JS default has none).
    fn blocked_apps(&self) -> Option<Value> {
        let apps = &self.config.computer_use.blocked_apps;
        (!apps.is_empty()).then(|| Value::from(apps.clone()))
    }

    /// What the `computer` tool configures before its first action.
    pub(crate) fn computer_configure(&self) -> Obj {
        let cu = &self.config.computer_use;
        Obj::cmd("configure")
            .set("accent", cu.accent.as_str())
            .set("label", cu.label.as_str())
            .set(
                "banner",
                cu.banner.as_deref().unwrap_or(self.strings.banner),
            )
            .set(
                "escHint",
                cu.esc_hint.as_deref().unwrap_or(self.strings.esc_hint),
            )
            .set("showLabel", cu.show_label)
            .set("showGlow", cu.show_glow)
            .set("showActivity", cu.show_activity)
            .set("clickPulse", cu.click_pulse)
            .set("maxWidth", self.max_width())
            .set("mini", self.mini())
            .opt("blockedApps", self.blocked_apps())
    }

    /// What `app` and the browser's mini window configure (`helperSetup`).
    pub(crate) fn setup_configure(&self) -> Obj {
        Obj::cmd("configure")
            .set("accent", self.config.computer_use.accent.as_str())
            .set("maxWidth", self.max_width())
            .set("mini", self.mini())
            .opt("blockedApps", self.blocked_apps())
    }
}

/// Which `configure` requests went through already.
#[derive(Debug, Default)]
struct Configured {
    computer: tokio::sync::Mutex<bool>,
    setup: tokio::sync::Mutex<bool>,
}

/// The helper connection shared by `computer`, `app` and the browser's mini
/// window: client, settings and the once-per-process `configure` state.
#[derive(Debug)]
pub struct Helper {
    client: Arc<CuaClient>,
    settings: Settings,
    configured: Arc<Configured>,
}

static SHARED: OnceLock<(Arc<CuaClient>, Arc<Configured>)> = OnceLock::new();

impl Helper {
    /// The installed helper. Client and configure state are process-wide, so
    /// the helper is configured once per process however often this is called
    /// (the first configuration wins, like the plugin's host lifetime).
    pub fn shared(config: &CodexConfig) -> Arc<Helper> {
        let (client, configured) = SHARED
            .get_or_init(|| (Arc::new(CuaClient::new()), Arc::default()))
            .clone();
        Arc::new(Helper {
            client,
            settings: Settings::new(config),
            configured,
        })
    }

    /// A helper behind `client` with its own configure state (tests, other sockets).
    pub fn new(client: Arc<CuaClient>, settings: Settings) -> Arc<Helper> {
        Arc::new(Helper {
            client,
            settings,
            configured: Arc::default(),
        })
    }

    pub fn client(&self) -> &Arc<CuaClient> {
        &self.client
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// `helperSetup()`: sends accent, width, blocked apps and mini strings once.
    /// Never fails; a transport error leaves it to the next call.
    pub async fn setup(&self) {
        let mut done = self.configured.setup.lock().await;
        if !*done {
            let request = self.settings.setup_configure();
            *done = self
                .client
                .call(&request.0, CallOptions::default())
                .await
                .is_ok();
        }
    }

    /// The `computer` tool's own full configure before its first action; errors
    /// fail the action and the next action tries again.
    pub(crate) async fn configure_computer(&self) -> Result<(), CuaError> {
        let mut done = self.configured.computer.lock().await;
        if !*done {
            self.client
                .call(
                    &self.settings.computer_configure().0,
                    CallOptions::default(),
                )
                .await?;
            *done = true;
        }
        Ok(())
    }
}
