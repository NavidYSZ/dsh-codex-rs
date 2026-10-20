//! DeepSeek Messages adapter for dsh-codex.
//!
//! [`DeepSeekClient`] implements [`dshx_core::LlmClient`] for the route
//! `deepseek-official` and talks to DeepSeek's Anthropic-compatible
//! `POST {base}/v1/messages` endpoint exactly like DeepSeek Harness'
//! `@deepseek-ai/dsh-llm-deepseek`: same request body (byte for byte, see
//! [`json`]), same thinking replay (`source.replayState`, kind
//! `deepseek-messages` v1), same failure codes. It sends nothing extra: no
//! `dsh_session_log`, no `dsh_plugin_packages`, no anonymous user id.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use dshx_core::{GenerateRequest, LlmClient, Message, StreamChunk};
//! use futures::StreamExt;
//! let client = dshx_llm::DeepSeekClient::from_env()?;
//! let request = GenerateRequest {
//!     model: "deepseek-flash".into(),
//!     reasoning_effort: Some("high".into()),
//!     messages: vec![Message::system("You are terse."), Message::user_text("Hi")],
//!     tools: vec![],
//!     max_tokens: None,
//!     temperature: None,
//!     session_id: None,
//! };
//! let mut stream = client.stream(request, Default::default()).await;
//! let mut assembler = dshx_llm::BlockAssembler::new();
//! while let Some(chunk) = stream.next().await {
//!     assembler.push(&chunk);
//! }
//! let message = assembler.message(client.provider(), "deepseek-flash");
//! # let _ = message; Ok(()) }
//! ```
//!
//! Retries are the agent's job: it asks [`RetryPolicy::decide`] after each
//! failed attempt (see [`DeepSeekClient::retry_policy`]).

pub mod assembler;
pub mod auth;
pub mod client;
pub mod config;
pub mod images;
pub mod json;
pub mod models;
pub mod replay;
pub mod retry;
pub mod serialize;
pub mod sse;
pub mod translate;
pub mod transport;

pub use assembler::{BlockAssembler, chunk_json};
pub use auth::{
    API_KEY_ENV, ApiKeyChain, ApiKeySource, EnvApiKey, SharedApiKey, StaticApiKey, resolve_api_key,
};
pub use client::{ANTHROPIC_VERSION, DeepSeekClient, DeepSeekClientBuilder, SESSION_ID_HEADER};
pub use config::{BASE_URL_ENV, ConfigError, DeepSeekConfig, PUBLIC_BASE_URL};
pub use models::{
    CatalogModel, PROVIDER, REASONING_EFFORTS, ResolvedModel, SystemPromptUpdate, ToolUpdate,
    default_models,
};
pub use replay::{REPLAY_KIND, REPLAY_VERSION, read_replay, replay_state};
pub use retry::{DEFAULT_RETRYABLE_CODES, RetryDecision, RetryMode, RetryPolicy};
pub use serialize::{TOOL_CHANGES_BETA, WireRequest};
pub use transport::{is_context_window_exceeded, is_quota_exceeded};

/// Failure code of a request that exceeded the model's context window; the
/// agent compacts and retries once.
pub const CONTEXT_WINDOW_EXCEEDED: &str = "CONTEXT_WINDOW_EXCEEDED";
