//! The floating mini window (macOS Swift helper) as seen from the browser.
//!
//! `dshx-macos` implements [`MiniSink`] on top of the helper socket; every
//! other platform uses [`NoMini`]. Each method maps to one helper command of
//! the original plugin (`lib/browser.js`):
//!
//! | method  | helper request                                              | timeout | starts helper |
//! |---------|-------------------------------------------------------------|---------|---------------|
//! | `show`  | `{"cmd":"mini_show","title","icon","width","height","activity"}` | 5 s | yes |
//! | `step`  | `{"cmd":"mini_step","x","y","click"}` (x/y in 0…1)          | 5 s     | yes           |
//! | `frame` | `{"cmd":"mini_frame","data"}` (base64 JPEG)                 | 4 s     | no            |
//! | `poll`  | `{"cmd":"mini_poll"}`                                       | 2 s     | no            |
//! | `done`  | `{"cmd":"mini_done"}`                                       | -       | no            |
//!
//! Implementations swallow transport errors (return [`MiniReply::default`]):
//! the mini window must never fail a browser action. The only thing that
//! does is the user pressing Stop, reported through [`MiniReply::stopped`].
use async_trait::async_trait;

/// `mini_show`: brings up the window for the browser with the current step.
#[derive(Debug, Clone, PartialEq)]
pub struct MiniShow {
    /// `Browser · <host>` or `Browser`.
    pub title: String,
    /// Path of the browser's `.app` bundle (for its icon) or `globe`.
    pub icon: String,
    /// Viewport size in CSS pixels; the window keeps this aspect ratio.
    pub width: u32,
    pub height: u32,
    /// Status line, localized (`Klickt „Suchen“`, `Opening example.com`).
    pub activity: String,
}

/// `mini_step`: glides the agent cursor to a point, optionally with a click pulse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MiniStep {
    /// Fraction of the viewport width (0…1).
    pub x: f64,
    /// Fraction of the viewport height (0…1).
    pub y: f64,
    pub click: bool,
}

/// What the helper answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MiniReply {
    /// User requests queued in the window since the last call; the browser
    /// acts on `"show"` (the user clicked the preview: show the real window).
    pub requests: Vec<String>,
    /// Set when the user stopped the agent (`ok:false, cancelled:true`); the
    /// helper's error text, which becomes the tool error the model sees.
    pub stopped: Option<String>,
}

#[async_trait]
pub trait MiniSink: Send + Sync {
    /// Whether there is a mini window at all. When false the browser skips
    /// the screencast and never calls the other methods.
    fn enabled(&self) -> bool {
        false
    }
    async fn show(&self, show: MiniShow) -> MiniReply {
        let _ = show;
        MiniReply::default()
    }
    async fn step(&self, step: MiniStep) -> MiniReply {
        let _ = step;
        MiniReply::default()
    }
    /// One screencast frame (base64 JPEG, ≤ 720×460). Frames arriving while a
    /// previous one is still in flight are dropped by the browser.
    async fn frame(&self, jpeg_base64: String) -> MiniReply {
        let _ = jpeg_base64;
        MiniReply::default()
    }
    /// Picks up clicks in the window while the agent is thinking; called every
    /// 0.9 s for 40 s after each step.
    async fn poll(&self) -> MiniReply {
        MiniReply::default()
    }
    /// The browser shut down (idle exit or host shutdown): show "Done".
    async fn done(&self) {}
}

/// No mini window (Linux, tests, or disabled in the config).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMini;

impl MiniSink for NoMini {}
