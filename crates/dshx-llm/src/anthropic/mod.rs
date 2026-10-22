//! Anthropic Messages for Anthropic itself and Anthropic-compatible
//! endpoints: request body ([`serialize`]), thinking replay ([`replay`]) and
//! SSE translation ([`translate`]). Unlike the DeepSeek route there is no
//! in-history system prompt and no tool update: the system prompt sits at
//! the top and every request carries the full tool list, with
//! `cache_control` breakpoints on tools, system and the newest turn.

pub mod replay;
pub mod serialize;
pub mod translate;

pub use serialize::{Build, serialize};
pub use translate::Translator;

/// `anthropic-version` header value; also accepted by compatible gateways.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Protocol name in failure messages.
pub(crate) const LABEL: &str = "Anthropic Messages";

/// `POST` target: the root plus `/v1/messages`, without doubling an explicit `/v1`.
pub fn messages_url(base: &str) -> String {
    format!("{}/messages", v1_root(base))
}

/// `GET` target of the model list.
pub fn models_url(base: &str) -> String {
    format!("{}/models", v1_root(base))
}

fn v1_root(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let has_v1 = reqwest::Url::parse(base)
        .map(|u| u.path().trim_end_matches('/').ends_with("/v1"))
        .unwrap_or(false);
    if has_v1 {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_append_v1_once() {
        assert_eq!(
            messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            messages_url("https://gw.example/anthropic/"),
            "https://gw.example/anthropic/v1/messages"
        );
        assert_eq!(
            models_url("https://gw.example/v1"),
            "https://gw.example/v1/models"
        );
    }
}
