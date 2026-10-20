//! The helper's floating mini window as the background browser drives it
//! (the `mini_*` requests of `lib/browser.js`).
//!
//! Wiring once `dshx-browser` and this crate are merged: implement
//! `dshx_browser::MiniSink` for [`HelperMiniSink`] in `dshx-macos` (this crate
//! then depends on `dshx-browser`, never the other way round) by forwarding
//! each trait method to the method of the same purpose below, and hand
//! `HelperMiniSink::for_config(&config.codex)` to the browser on macOS.
//! The browser keeps its own logic: drop frames while one is in flight, open
//! the visible window when a reply [`MiniReply::wants_show`], fail the action
//! when [`MiniReply::stopped`] says the user pressed Stop, and poll every
//! 900 ms for up to 40 s after a step.
use std::sync::Arc;

use dshx_core::config::CodexConfig;
use serde_json::Value;

use crate::client::{CallOptions, CuaError};
use crate::helper::Helper;
use crate::js::{self, Obj};

/// The helper's answer to a `mini_*` request.
#[derive(Debug, Clone, PartialEq)]
pub struct MiniReply {
    pub ok: bool,
    /// The user stopped the agent (Stop in the mini window, Esc in the foreground).
    pub cancelled: bool,
    pub error: Option<String>,
    /// Clicks on the mini window since the last request (`"show"`).
    pub requests: Vec<String>,
    /// Only on `mini_poll`.
    pub visible: Option<bool>,
}

impl MiniReply {
    fn from_value(v: &Value) -> Self {
        MiniReply {
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            cancelled: js::truthy(v.get("cancelled")),
            error: v
                .get("error")
                .filter(|e| !e.is_null())
                .map(|e| js::text(Some(e))),
            requests: v
                .get("requests")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            visible: v.get("visible").and_then(Value::as_bool),
        }
    }

    /// `res.requests.includes('show')`: the user wants to see the real browser window.
    pub fn wants_show(&self) -> bool {
        self.requests.iter().any(|r| r == "show")
    }

    /// `res.ok === false && res.cancelled`: the error the action must fail with.
    pub fn stopped(&self) -> Option<&str> {
        (!self.ok && self.cancelled).then(|| self.error.as_deref().unwrap_or_default())
    }
}

/// Mini-window output of the background browser, backed by the helper.
#[derive(Debug, Clone)]
pub struct HelperMiniSink {
    helper: Arc<Helper>,
}

impl HelperMiniSink {
    pub fn new(helper: Arc<Helper>) -> Self {
        HelperMiniSink { helper }
    }

    /// The installed helper's mini window; `None` off macOS (`mini: mac && …`).
    pub fn for_config(config: &CodexConfig) -> Option<Self> {
        cfg!(target_os = "macos").then(|| HelperMiniSink::new(Helper::shared(config)))
    }

    /// `de` or `en`, for the browser's `--lang` and activity texts.
    pub fn locale(&self) -> &str {
        self.helper.settings().locale()
    }

    /// `helperSetup()` before each browser action: configures the helper once.
    pub async fn setup(&self) {
        self.helper.setup().await;
    }

    async fn call(&self, request: Obj, options: CallOptions) -> Result<MiniReply, CuaError> {
        let res = self.helper.client().call(&request.0, options).await?;
        Ok(MiniReply::from_value(&res))
    }

    /// `mini_show`: title (`Browser · host`), icon (app bundle path or SF Symbol
    /// name such as `globe`), viewport size for the aspect ratio, current step.
    /// May start the helper.
    pub async fn show(
        &self,
        title: &str,
        icon: &str,
        width: u32,
        height: u32,
        activity: Option<&str>,
    ) -> Result<MiniReply, CuaError> {
        let request = Obj::cmd("mini_show")
            .set("title", title)
            .set("icon", icon)
            .set("width", width)
            .set("height", height)
            .opt("activity", activity.map(Value::from));
        self.call(request, CallOptions::timeout_ms(5000)).await
    }

    /// `mini_step`: glides the mini cursor to `x`/`y` given as fractions of the viewport.
    pub async fn step(&self, x: f64, y: f64, click: bool) -> Result<MiniReply, CuaError> {
        let request = Obj::cmd("mini_step")
            .set("x", js::num(x))
            .set("y", js::num(y))
            .set("click", click);
        self.call(request, CallOptions::timeout_ms(5000)).await
    }

    /// `mini_frame`: one screencast JPEG (base64). Never starts the helper.
    pub async fn frame(&self, data: &str) -> Result<MiniReply, CuaError> {
        self.call(
            Obj::cmd("mini_frame").set("data", data),
            CallOptions::timeout_ms(4000).no_launch(),
        )
        .await
    }

    /// `mini_poll`: picks up clicks on the mini window. Never starts the helper.
    pub async fn poll(&self) -> Result<MiniReply, CuaError> {
        self.call(
            Obj::cmd("mini_poll"),
            CallOptions::timeout_ms(2000).no_launch(),
        )
        .await
    }
}

/// The browser's view of the mini window. Transport errors are swallowed so the
/// window can never fail a browser action; only Stop does (via `stopped`).
#[async_trait::async_trait]
impl dshx_browser::MiniSink for HelperMiniSink {
    fn enabled(&self) -> bool {
        true
    }

    async fn show(&self, show: dshx_browser::MiniShow) -> dshx_browser::MiniReply {
        self.setup().await;
        let activity = Some(show.activity.as_str()).filter(|a| !a.is_empty());
        let reply = HelperMiniSink::show(
            self,
            &show.title,
            &show.icon,
            show.width,
            show.height,
            activity,
        )
        .await;
        to_browser(reply)
    }

    async fn step(&self, step: dshx_browser::MiniStep) -> dshx_browser::MiniReply {
        self.setup().await;
        to_browser(HelperMiniSink::step(self, step.x, step.y, step.click).await)
    }

    async fn frame(&self, jpeg_base64: String) -> dshx_browser::MiniReply {
        to_browser(HelperMiniSink::frame(self, &jpeg_base64).await)
    }

    async fn poll(&self) -> dshx_browser::MiniReply {
        to_browser(HelperMiniSink::poll(self).await)
    }

    async fn done(&self) {
        let _ = self
            .call(
                Obj::cmd("mini_done"),
                CallOptions::timeout_ms(2000).no_launch(),
            )
            .await;
    }
}

fn to_browser(reply: Result<MiniReply, CuaError>) -> dshx_browser::MiniReply {
    match reply {
        Ok(reply) => dshx_browser::MiniReply {
            stopped: reply.stopped().map(str::to_owned),
            requests: reply.requests,
        },
        Err(_) => dshx_browser::MiniReply::default(),
    }
}

/// Mini-window sink for the background browser: the helper on macOS, nothing elsewhere.
pub fn browser_mini(config: &CodexConfig) -> Arc<dyn dshx_browser::MiniSink> {
    match HelperMiniSink::for_config(config) {
        Some(sink) => Arc::new(sink),
        None => Arc::new(dshx_browser::NoMini),
    }
}
