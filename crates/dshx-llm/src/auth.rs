//! API key sources. The key is resolved per request, so a key stored by the
//! server's settings page reaches the next request without a restart.
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dshx_core::LlmFailure;

/// Default environment variable holding the key.
pub const API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// Supplies the `x-api-key` value. `None` means "no key here" (the next
/// source of an [`ApiKeyChain`] is asked; a lone source fails with
/// `MISSING_CREDENTIAL`).
#[async_trait]
pub trait ApiKeySource: Send + Sync {
    async fn api_key(&self) -> Option<String>;
}

/// Reads an environment variable on every request (default `DEEPSEEK_API_KEY`).
#[derive(Debug, Clone)]
pub struct EnvApiKey {
    pub var: String,
}

impl Default for EnvApiKey {
    fn default() -> Self {
        EnvApiKey {
            var: API_KEY_ENV.into(),
        }
    }
}

#[async_trait]
impl ApiKeySource for EnvApiKey {
    async fn api_key(&self) -> Option<String> {
        std::env::var(&self.var).ok().filter(|v| !v.is_empty())
    }
}

/// A key that can be replaced at runtime (e.g. by `PUT /api/settings`).
#[derive(Debug, Default, Clone)]
pub struct SharedApiKey(Arc<RwLock<Option<String>>>);

impl SharedApiKey {
    pub fn new(key: Option<String>) -> Self {
        SharedApiKey(Arc::new(RwLock::new(key)))
    }
    pub fn set(&self, key: Option<String>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = key;
    }
    pub fn get(&self) -> Option<String> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl ApiKeySource for SharedApiKey {
    async fn api_key(&self) -> Option<String> {
        self.get().filter(|v| !v.is_empty())
    }
}

/// A fixed key (tests, `--api-key`-style overrides).
#[derive(Clone)]
pub struct StaticApiKey(pub String);

impl std::fmt::Debug for StaticApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StaticApiKey(<redacted>)")
    }
}

#[async_trait]
impl ApiKeySource for StaticApiKey {
    async fn api_key(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

/// First source that has a key wins (e.g. stored key, then environment).
#[derive(Clone, Default)]
pub struct ApiKeyChain(pub Vec<Arc<dyn ApiKeySource>>);

#[async_trait]
impl ApiKeySource for ApiKeyChain {
    async fn api_key(&self) -> Option<String> {
        for source in &self.0 {
            if let Some(key) = source.api_key().await {
                return Some(key);
            }
        }
        None
    }
}

/// Resolves and checks a key like dsh `assertUsableApiKey`: surrounding
/// whitespace is trimmed; blank keys and characters outside printable ASCII
/// fail with `INVALID_CREDENTIAL`, absence with `MISSING_CREDENTIAL`.
pub async fn resolve_api_key(source: &dyn ApiKeySource) -> Result<String, LlmFailure> {
    let Some(raw) = source.api_key().await else {
        return Err(LlmFailure::new(
            "MISSING_CREDENTIAL",
            format!(
                "llm-deepseek: no API key for provider route \"{}\"; store one in the dsh-codex settings or export {API_KEY_ENV}",
                crate::PROVIDER
            ),
        ));
    };
    let key = raw.trim();
    if key.is_empty() {
        return Err(LlmFailure::new(
            "INVALID_CREDENTIAL",
            "llm-deepseek: the API key is blank; set it to the raw key",
        ));
    }
    if !key.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(LlmFailure::new(
            "INVALID_CREDENTIAL",
            "llm-deepseek: the API key contains characters no HTTP header can carry; set it to the raw key alone",
        ));
    }
    Ok(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keys_are_trimmed_and_checked() {
        assert_eq!(
            resolve_api_key(&StaticApiKey("  sk-x \n".into()))
                .await
                .unwrap(),
            "sk-x"
        );
        assert_eq!(
            resolve_api_key(&StaticApiKey(" ".into()))
                .await
                .unwrap_err()
                .code,
            "INVALID_CREDENTIAL"
        );
        assert_eq!(
            resolve_api_key(&StaticApiKey("sk ü".into()))
                .await
                .unwrap_err()
                .code,
            "INVALID_CREDENTIAL"
        );
        assert_eq!(
            resolve_api_key(&SharedApiKey::default())
                .await
                .unwrap_err()
                .code,
            "MISSING_CREDENTIAL"
        );
        let shared = SharedApiKey::default();
        let chain = ApiKeyChain(vec![
            Arc::new(shared.clone()),
            Arc::new(StaticApiKey("fallback".into())),
        ]);
        assert_eq!(resolve_api_key(&chain).await.unwrap(), "fallback");
        shared.set(Some("stored".into()));
        assert_eq!(resolve_api_key(&chain).await.unwrap(), "stored");
    }
}
