//! Request failures. `Display` is what the model reads after `Error: `, so
//! the texts follow the official MCP SDK dsh uses.
use serde_json::Value;

#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    /// JSON-RPC error object returned by the server; dsh shows its message only.
    #[error("{message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("Request timed out")]
    Timeout,
    #[error("Connection closed")]
    Closed,
    #[error("Not connected")]
    NotConnected,
    #[error("cancelled")]
    Cancelled,
    /// Transport failure (spawn, I/O, HTTP status).
    #[error("{0}")]
    Transport(String),
    /// Malformed or unsupported protocol data.
    #[error("{0}")]
    Protocol(String),
}

impl McpError {
    /// JSON-RPC error object of a response.
    pub(crate) fn from_rpc(error: &Value) -> Self {
        McpError::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error")
                .to_owned(),
            data: error.get("data").cloned(),
        }
    }
}
