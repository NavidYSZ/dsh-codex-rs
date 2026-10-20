//! Typed subagent failures (dsh `SubagentError` / `SubagentDepthError`).
use std::fmt;

/// A refused delegation or control request. The model sees `message` as the
/// tool error text (`Error: <message>`), exactly like dsh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentError {
    /// Stable dsh code: `UNAUTHORIZED`, `NOT_RESUMABLE`, `PARENT_UNAVAILABLE`,
    /// `ACTIVATION_LIMIT_REACHED`, `DRAINING`, `DEPTH`, ...
    pub code: &'static str,
    pub message: String,
}

impl SubagentError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        SubagentError {
            code,
            message: message.into(),
        }
    }

    /// dsh `SubagentDepthError`.
    pub fn depth(attempted: u32, max: u32) -> Self {
        SubagentError::new(
            "DEPTH",
            format!("subagent depth {attempted} exceeds maxDepth {max}"),
        )
    }

    /// JS `String(error)`: the error class name and message.
    pub fn display_with_name(&self) -> String {
        let name = if self.code == "DEPTH" {
            "SubagentDepthError"
        } else {
            "SubagentError"
        };
        format!("{name}: {}", self.message)
    }
}

impl fmt::Display for SubagentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SubagentError {}
