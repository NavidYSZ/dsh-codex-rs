//! The model-facing tools of dsh-codex (SPEC §5.3): `bash`, `job_list`,
//! `job_output`, `job_kill`, `read`, `write`, `edit`, `read_image`, `glob`,
//! `grep`, `todo_write`, `web_fetch`, `web_search`.
//!
//! Names, descriptions and argument schemas are byte-identical to dsh
//! 0.2.0-rc.2, and the text each tool returns matches dsh's tool, so the model
//! sees the same tool surface as under the official harness.
//!
//! # Using the tools
//!
//! ```no_run
//! use std::sync::Arc;
//! # fn demo(config: &dshx_core::config::Config) {
//! let mut registry = dshx_core::ToolRegistry::new();
//! for tool in dshx_tools::default_tools(config) {
//!     registry.register(tool);
//! }
//! // One state per session, shared by all of its ToolContexts:
//! let state = Arc::new(dshx_tools::ToolState::new());
//! let mut job_events = state.jobs().subscribe();
//! # }
//! ```
//!
//! A `ToolContext` needs the session's [`ToolState`] in `state`; see
//! [`testing`] for a ready-made context. Errors (`ToolError`) are meant to be
//! shown to the model as `Error: <message>` (dsh's wording; `Cancelled` is
//! dsh's `tool call aborted`).
use std::sync::Arc;

use dshx_core::Tool;

mod args;
pub mod bash;
pub mod env;
pub mod fs;
mod html;
pub mod jobs;
pub mod output;
pub mod process;
pub mod sandbox;
mod schemas;
pub mod search;
pub mod spill;
mod state;
pub mod testing;
pub mod todo;
mod util;
pub mod web;

pub use args::validate as validate_args;
pub use jobs::{CompletionNotice, JobEvent, JobRegistry, JobView};
pub use schemas::{TOOL_NAMES, upstream_schema};
pub use spill::{SpillRef, SpillStore};
pub use state::{Observation, ToolState};

/// All tools of this crate in the order dsh sends them (sorted by name).
pub fn default_tools(config: &dshx_core::config::Config) -> Vec<Arc<dyn Tool>> {
    let _ = config;
    vec![
        Arc::new(bash::BashTool::new()),
        Arc::new(fs::mutate::EditTool),
        Arc::new(search::GlobTool),
        Arc::new(search::GrepTool),
        Arc::new(jobs::JobKillTool),
        Arc::new(jobs::JobListTool),
        Arc::new(jobs::JobOutputTool),
        Arc::new(fs::read::ReadTool),
        Arc::new(fs::image::ReadImageTool),
        Arc::new(todo::TodoWriteTool),
        Arc::new(web::WebFetchTool::new()),
        Arc::new(web::WebSearchTool),
        Arc::new(fs::mutate::WriteTool),
    ]
}
