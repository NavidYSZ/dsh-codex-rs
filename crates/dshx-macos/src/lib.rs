//! macOS tools of dsh-codex: the client for the Swift helper
//! ("DSH Computer Use.app"), the `computer` and `app` tools and the helper's
//! mini window for the background browser. Ports of `lib/cua-client.js`,
//! `lib/computer-tool.js`, `lib/app-tool.js` and the tool half of `index.js`.
//!
//! Everything compiles on every OS; the tools are only offered on macOS.
use std::sync::Arc;

use dshx_core::config::CodexConfig;
use dshx_core::{Tool, ToolError};

pub mod app;
pub mod client;
pub mod computer;
mod helper;
mod js;
pub mod locale;
pub mod mini;
mod output;

pub use app::AppTool;
pub use client::{CallOptions, CuaClient, CuaError};
pub use computer::ComputerTool;
pub use helper::{Helper, Settings};
pub use mini::{HelperMiniSink, MiniReply, browser_mini};

/// Why a macOS tool call failed. The message is what the model sees.
#[derive(Debug, thiserror::Error)]
pub enum MacError {
    /// Unusable tool input (`"click" needs numeric "x"`).
    #[error("{0}")]
    Input(String),
    /// The helper answered `ok: false`; `cancelled` when the user pressed Esc or Stop.
    #[error("{message}")]
    Helper { message: String, cancelled: bool },
    #[error(transparent)]
    Client(#[from] CuaError),
    #[error("{0}")]
    Attachment(String),
    /// The turn was cancelled.
    #[error("aborted")]
    Aborted,
}

/// Like the plugin's plain `Error`s (dsh shows them as `Error: <message>`),
/// bad input is a failure too, not dsh's schema-level `INVALID_ARGS`.
impl From<MacError> for ToolError {
    fn from(err: MacError) -> Self {
        match err {
            MacError::Aborted => ToolError::Cancelled,
            other => ToolError::Failed(other.to_string()),
        }
    }
}

/// `computer` and `app` as enabled in `[codex]`, in the plugin's order; empty
/// off macOS. They share one helper connection per process.
pub fn macos_tools(config: &CodexConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    tools(&Helper::shared(config))
}

/// The enabled tools behind `helper`, on any OS (tests use a fake helper).
pub fn tools(helper: &Arc<Helper>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if helper.settings().computer_enabled() {
        tools.push(Arc::new(ComputerTool::new(helper.clone())));
    }
    if helper.settings().apps_enabled() {
        tools.push(Arc::new(AppTool::new(helper.clone())));
    }
    tools
}
