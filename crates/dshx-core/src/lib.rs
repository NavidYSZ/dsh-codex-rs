//! Shared vocabulary of dsh-codex. Every other crate builds on these types, so
//! changes here are contract changes: see `docs/rust/SPEC.md`.
//!
//! The message model mirrors DeepSeek Harness' canonical model
//! (`packages/llm/llm/src/{types,message}.ts`) field for field, because the
//! session log must stay readable by the official `dsh` (format v4).

pub mod attachment;
pub mod config;
pub mod event;
pub mod llm;
pub mod message;
pub mod tool;

pub use attachment::*;
pub use event::*;
pub use llm::*;
pub use message::*;
pub use tool::*;

/// Milliseconds since the Unix epoch, the time unit of every session event.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Random UUID v4 string, used for message ids and session ids.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
