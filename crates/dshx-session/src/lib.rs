//! Session persistence in DeepSeek Harness (dsh) session format v4, plus the
//! model-visible history derived from it.
//!
//! - [`SessionStore`] / [`FileLog`]: the on-disk store, byte-compatible with
//!   dsh (`<root>/--<cwd>--/<id>/session.v4.jsonl.zstd`, one checksummed zstd
//!   frame per durable batch, `flock` on `session.lock`, torn-tail recovery).
//! - [`reader`]: decoding of dsh-written logs with dsh's refusal rules.
//! - [`derive_messages`] / [`SurfaceFold`]: the surface fold of dsh `surface.ts`.
//! - [`interrupted_turn_closers`]: crash repair of an open final turn.
//! - [`validate`]: "would the official dsh load this log?" checks.
//! - [`FileAttachments`]: content-addressed image store like dsh `attachment-local`.
//! - [`Credentials`]: the DeepSeek API key file.
//! - [`MemoryLog`]: in-memory `SessionLog` for tests.

mod attachments;
mod credentials;
mod error;
mod file_log;
pub mod format;
mod frames;
mod lock;
mod memory;
pub mod reader;
mod repair;
mod store;
mod surface;
pub mod validate;

pub use attachments::{FileAttachments, ImagePolicy, probe_image, sniff_media_type};
pub use credentials::{Credentials, resolve_api_key};
pub use error::{Result, SessionError};
pub use file_log::FileLog;
pub use lock::SessionLock;
pub use memory::MemoryLog;
pub use repair::{
    TOOL_NOT_STARTED, TOOL_OUTCOME_UNKNOWN, ToolCallRecovery, interrupted_turn_closers,
    to_new_events,
};
pub use store::{Located, SessionStore, SessionSummary, StoredSession, agent_preset, summarize};
pub use surface::{
    SurfaceFold, SurfaceOp, SurfaceReplacement, derive_event_message, derive_messages,
    fold_lenient, fold_surface, message_value, surface_op_of,
};
