//! Connection settings (dsh `llm-deepseek/src/config.ts`, resolved form).
use std::time::Duration;

use crate::models::{CatalogModel, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS, default_models};
use crate::retry::RetryPolicy;

/// Public API root; `DEEPSEEK_BASE_URL` or the config file may point elsewhere.
pub const PUBLIC_BASE_URL: &str = "https://api.deepseek.com/anthropic";
/// Environment variable naming the endpoint root.
pub const BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";
/// Default idle limit while one stream read is outstanding (five minutes).
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Base64 image payload budget of one inline request (20 MiB).
pub const DEFAULT_MAX_INLINE_REQUEST_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Base64-byte removal step once the inline budget is exceeded (10 MiB).
pub const DEFAULT_INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM: u64 = 10 * 1024 * 1024;
/// Provider image-count limit per request.
pub const DEFAULT_MAX_IMAGES_PER_REQUEST: u64 = 600;
/// Image-count removal step once the count limit is exceeded.
pub const DEFAULT_IMAGE_OFFLOAD_COUNT_QUANTUM: u64 = 20;

/// Validated settings of one [`crate::DeepSeekClient`].
#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    /// Messages API root, e.g. `https://api.deepseek.com/anthropic`; `/v1/messages` is appended.
    pub base_url: String,
    /// Advertised models; requests for other ids are still sent (as text-only).
    pub models: Vec<CatalogModel>,
    /// Default output cap; a model's own cap and an explicit request value win.
    pub max_tokens: u32,
    pub default_context_window: u64,
    /// Deployment policy: `true` limits every request to effort `off`.
    pub thinking_disabled: bool,
    /// Default effort when a request names none (`None` = `high`).
    pub reasoning_effort: Option<String>,
    pub stream_idle_timeout: Duration,
    /// Provider-owned retry policy, executed by the agent.
    pub retry_policy: RetryPolicy,
    /// `user-agent` header value.
    pub user_agent: String,
    /// Send `x-deepseek-harness-session-id` when the request carries a session id.
    pub send_session_id: bool,
    pub max_inline_request_image_bytes: u64,
    pub inline_image_offload_byte_quantum: u64,
    pub max_images_per_request: u64,
    pub image_offload_count_quantum: u64,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        DeepSeekConfig {
            base_url: PUBLIC_BASE_URL.into(),
            models: default_models(),
            max_tokens: DEFAULT_MAX_TOKENS,
            default_context_window: DEFAULT_CONTEXT_WINDOW,
            thinking_disabled: false,
            reasoning_effort: None,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            retry_policy: RetryPolicy::default(),
            user_agent: user_agent(),
            send_session_id: true,
            max_inline_request_image_bytes: DEFAULT_MAX_INLINE_REQUEST_IMAGE_BYTES,
            inline_image_offload_byte_quantum: DEFAULT_INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM,
            max_images_per_request: DEFAULT_MAX_IMAGES_PER_REQUEST,
            image_offload_count_quantum: DEFAULT_IMAGE_OFFLOAD_COUNT_QUANTUM,
        }
    }
}

/// `dsh-codex/<version>`.
pub fn user_agent() -> String {
    format!("dsh-codex/{}", env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("dshx-llm: {0}")]
pub struct ConfigError(pub String);

impl DeepSeekConfig {
    /// Defaults plus `DEEPSEEK_BASE_URL` when set.
    pub fn from_env() -> Self {
        let mut config = DeepSeekConfig::default();
        if let Some(url) = env_base_url() {
            config.base_url = url;
        }
        config
    }

    /// Settings from the user's `config.toml`; `DEEPSEEK_BASE_URL` overrides `base_url`.
    pub fn from_core(core: &dshx_core::config::Config) -> Self {
        let mut config = DeepSeekConfig {
            base_url: core.base_url.clone(),
            max_tokens: core.max_tokens,
            reasoning_effort: core.reasoning_effort.clone(),
            ..DeepSeekConfig::default()
        };
        if let Some(url) = env_base_url() {
            config.base_url = url;
        }
        config
    }

    /// Checks the invariants dsh's resolver enforces.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|e| ConfigError(format!("invalid base URL: {e}")))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ConfigError(
                "Messages baseURL must be an HTTP(S) root without credentials, query, or fragment"
                    .into(),
            ));
        }
        if self.thinking_disabled && self.reasoning_effort.as_deref().is_some_and(|e| e != "off") {
            return Err(ConfigError(
                "only reasoningEffort \"off\" can be configured when thinking is disabled".into(),
            ));
        }
        if let Some(effort) = &self.reasoning_effort
            && !crate::models::REASONING_EFFORTS.contains(&effort.as_str())
        {
            return Err(ConfigError(format!("unknown reasoning effort {effort:?}")));
        }
        if self.max_tokens == 0 || self.default_context_window == 0 {
            return Err(ConfigError(
                "maxTokens and defaultContextWindow must be positive".into(),
            ));
        }
        if self.stream_idle_timeout.is_zero() {
            return Err(ConfigError("streamIdleTimeoutMs must be positive".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for model in &self.models {
            if model.id.is_empty() || !seen.insert(model.id.as_str()) {
                return Err(ConfigError(format!(
                    "catalog model ids must be unique and non-empty ({:?})",
                    model.id
                )));
            }
            if model.input_modalities.is_empty()
                || model
                    .input_modalities
                    .iter()
                    .any(|m| m != "text" && m != "image")
            {
                return Err(ConfigError(format!(
                    "catalog model {:?} has invalid inputModalities",
                    model.id
                )));
            }
        }
        self.retry_policy.validate().map_err(ConfigError)
    }

    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// `POST` target: the root plus `/v1/messages`, without doubling an explicit `/v1`.
    pub fn messages_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let has_v1 = reqwest::Url::parse(base)
            .map(|u| u.path().trim_end_matches('/').ends_with("/v1"))
            .unwrap_or(false);
        if has_v1 {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }
}

fn env_base_url() -> Option<String> {
    std::env::var(BASE_URL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_url_appends_v1_once() {
        let mut config = DeepSeekConfig::default();
        assert_eq!(
            config.messages_url(),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        config.base_url = "http://127.0.0.1:1/anthropic///".into();
        assert_eq!(
            config.messages_url(),
            "http://127.0.0.1:1/anthropic/v1/messages"
        );
        config.base_url = "http://gw.example/v1".into();
        assert_eq!(config.messages_url(), "http://gw.example/v1/messages");
    }

    #[test]
    fn rejects_unsafe_base_urls_and_effort_conflicts() {
        for url in [
            "ftp://example.com",
            "https://user:pass@example.com",
            "https://example.com/?key=x",
            "https://example.com/#x",
        ] {
            let config = DeepSeekConfig {
                base_url: url.into(),
                ..Default::default()
            };
            assert!(config.validate().is_err(), "{url}");
        }
        let config = DeepSeekConfig {
            thinking_disabled: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert!(DeepSeekConfig::default().validate().is_ok());
    }
}
