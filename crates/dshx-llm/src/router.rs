//! [`ProviderRouter`]: the one [`LlmClient`] an agent holds. It serves the
//! DeepSeek route itself (unchanged, byte for byte) and hands an agent
//! configured for another provider that provider's client through
//! [`LlmClient::for_provider`]. Routes can be replaced while agents run.
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dshx_core::{GenerateRequest, LlmClient, ModelInfo, StreamChunk};
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::client::DeepSeekClient;
use crate::models::{PROVIDER, ResolvedModel};
use crate::provider::split_model_ref;
use crate::provider_client::{ProviderClient, UnavailableRoute};

pub struct ProviderRouter {
    deepseek: RwLock<Arc<DeepSeekClient>>,
    routes: RwLock<HashMap<String, Arc<ProviderClient>>>,
}

/// One model reference, resolved to its route.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRoute {
    pub provider: String,
    pub model: ResolvedModel,
}

impl ProviderRouter {
    pub fn new(deepseek: Arc<DeepSeekClient>) -> Self {
        ProviderRouter {
            deepseek: RwLock::new(deepseek),
            routes: RwLock::new(HashMap::new()),
        }
    }

    pub fn deepseek(&self) -> Arc<DeepSeekClient> {
        self.deepseek
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the DeepSeek client (e.g. after its base URL changed).
    pub fn set_deepseek(&self, client: Arc<DeepSeekClient>) {
        *self.deepseek.write().unwrap_or_else(|e| e.into_inner()) = client;
    }

    /// Replaces every other route; agents pick the new clients at their next request.
    pub fn set_routes(&self, clients: Vec<ProviderClient>) {
        let routes = clients
            .into_iter()
            .map(|c| (c.spec().id.clone(), Arc::new(c)))
            .collect();
        *self.routes.write().unwrap_or_else(|e| e.into_inner()) = routes;
    }

    pub fn route(&self, provider: &str) -> Option<Arc<ProviderClient>> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
            .cloned()
    }

    /// Configured routes besides DeepSeek, by id.
    pub fn routes(&self) -> Vec<Arc<ProviderClient>> {
        let mut routes: Vec<_> = self
            .routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        routes.sort_by(|a, b| a.spec().id.cmp(&b.spec().id));
        routes
    }

    /// `(provider, model id)` of a reference. A bare id that is no DeepSeek
    /// model but the model of exactly one other route resolves there.
    pub fn split(&self, reference: &str) -> (String, String) {
        let routes = self.routes.read().unwrap_or_else(|e| e.into_inner());
        let (provider, model) = split_model_ref(reference, |p| routes.contains_key(p));
        if provider == PROVIDER && self.deepseek().config().model(model).is_none() {
            let owners: Vec<&String> = routes
                .iter()
                .filter(|(_, c)| c.spec().model(model).is_some())
                .map(|(id, _)| id)
                .collect();
            if let [owner] = owners.as_slice() {
                return ((*owner).clone(), model.to_owned());
            }
        }
        (provider.to_owned(), model.to_owned())
    }

    /// Route metadata of a model reference, prices included where known.
    pub fn resolve(&self, reference: &str) -> ResolvedRoute {
        let (provider, model) = self.split(reference);
        let resolved = match self.route(&provider) {
            Some(client) => client.resolve_model(&model),
            None => self.deepseek().resolve_model(&model),
        };
        ResolvedRoute {
            provider,
            model: resolved,
        }
    }
}

#[async_trait]
impl LlmClient for ProviderRouter {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.deepseek().models()
    }

    /// Without a provider an agent talks to DeepSeek, as before providers.
    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        let deepseek = self.deepseek();
        deepseek.stream(request, cancel).await
    }

    fn for_provider(&self, provider: &str) -> Option<Arc<dyn LlmClient>> {
        if provider == PROVIDER {
            return Some(self.deepseek() as Arc<dyn LlmClient>);
        }
        Some(match self.route(provider) {
            Some(client) => client as Arc<dyn LlmClient>,
            None => Arc::new(UnavailableRoute::new(provider)),
        })
    }
}
