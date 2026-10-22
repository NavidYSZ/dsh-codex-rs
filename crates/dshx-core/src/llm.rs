//! Provider-neutral model streaming (dsh `GenerateOptions` / `StreamChunk`).
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::message::{ContentBlock, Message};

/// JSON-schema description of a tool, as sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema object for the arguments.
    pub parameters: Value,
}

/// `off`, `low`, `high`, `max` (dsh reasoning effort ids for DeepSeek).
pub type ReasoningEffort = String;

/// One fully assembled model request.
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Ordered history exactly as the model sees it. A leading `system` message
    /// carries the system prompt; later system messages are in-history updates.
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Session id, used for request attribution headers only.
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Uncached input only (disjoint from the cache counters).
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// Serializable provider or transport failure (dsh `LlmFailure`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFailure {
    pub message: String,
    /// Stable code: `RATE_LIMIT`, `SERVER`, `TIMEOUT`, `TRANSPORT`, `EMPTY_RESPONSE`,
    /// `AUTH`, `INVALID_REQUEST`, `CONTEXT_WINDOW_EXCEEDED`, `ABORTED`, ...
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `IMAGE_OFFLOAD_REQUIRED`: how many more of the oldest retained images
    /// must be offloaded before the request fits the route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload_images: Option<u64>,
}

impl LlmFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        LlmFailure {
            message: message.into(),
            code: code.into(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Aborted { failure: LlmFailure },
    Error { failure: LlmFailure },
}

/// Streaming protocol between an adapter and the agent loop. Block indexes
/// correlate interleaved deltas; `BlockEnd` carries the assembled block.
/// Exactly one `Finish` ends every stream (errors included).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    BlockStart {
        index: usize,
        block_type: &'static str,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ReasoningDelta {
        index: usize,
        text: String,
    },
    ToolCallDelta {
        index: usize,
        id: String,
        name: Option<String>,
        arguments_delta: String,
    },
    BlockEnd {
        index: usize,
        block: ContentBlock,
    },
    Usage(TokenUsage),
    /// `replay_state` goes into the assistant message's `source.replayState`.
    Finish {
        reason: FinishReason,
        replay_state: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    /// `["text"]` or `["text", "image"]`.
    pub input_modalities: Vec<String>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
}

impl ModelInfo {
    pub fn accepts_images(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "image")
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Provider route name recorded in assistant message sources (`deepseek-official`).
    fn provider(&self) -> &str;
    fn models(&self) -> Vec<ModelInfo>;
    /// Starts one streaming request. Never fails directly: transport and
    /// provider failures arrive as a terminal `Finish` with an error reason.
    /// Retries are the caller's job (see `dshx-agent`).
    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk>;
}
