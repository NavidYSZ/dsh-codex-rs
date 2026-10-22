//! [`ProviderClient`]: one provider route over Anthropic Messages or OpenAI
//! Chat Completions, with the same streaming contract as
//! [`crate::DeepSeekClient`]. It sends the route's own headers and nothing
//! extra (no session id, no harness headers).
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{AttachmentStore, GenerateRequest, LlmClient, LlmFailure, ModelInfo, StreamChunk};
use futures::stream::BoxStream;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::{ApiKeySource, resolve_provider_key};
use crate::config::{ConfigError, DEFAULT_STREAM_IDLE_TIMEOUT, user_agent};
use crate::images::ImagePathResolver;
use crate::models::{CatalogModel, ResolvedModel};
use crate::provider::{Api, DEFAULT_REQUEST_MAX_TOKENS, ProviderSpec};
use crate::retry::RetryPolicy;
use crate::serialize::{project_messages, request_image_refs};
use crate::{anthropic, openai, runner};

/// Adapter of one provider route. Cheap to clone.
#[derive(Clone)]
pub struct ProviderClient {
    inner: Arc<Inner>,
}

struct Inner {
    spec: ProviderSpec,
    api_key: Arc<dyn ApiKeySource>,
    http: reqwest::Client,
    attachments: Option<Arc<dyn AttachmentStore>>,
    image_path: Option<Arc<ImagePathResolver>>,
    idle: Duration,
    retry_policy: RetryPolicy,
}

impl ProviderClient {
    /// `api_key` is asked per request, so a key stored later works at once.
    pub fn new(
        spec: ProviderSpec,
        api_key: Arc<dyn ApiKeySource>,
        attachments: Option<Arc<dyn AttachmentStore>>,
        image_path: Option<Arc<ImagePathResolver>>,
    ) -> Result<Self, ConfigError> {
        let http = reqwest::Client::builder()
            // Credentials never follow a redirect to another origin.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ConfigError(format!("HTTP client: {e}")))?;
        Ok(ProviderClient {
            inner: Arc::new(Inner {
                spec,
                api_key,
                http,
                attachments,
                image_path,
                idle: DEFAULT_STREAM_IDLE_TIMEOUT,
                retry_policy: RetryPolicy::default(),
            }),
        })
    }

    pub fn spec(&self) -> &ProviderSpec {
        &self.inner.spec
    }

    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.inner.retry_policy
    }

    fn label(&self) -> &'static str {
        self.inner.spec.api.label()
    }

    /// Route metadata of a model of this provider; unknown ids resolve as
    /// text-only with the protocol defaults.
    pub fn resolve_model(&self, id: &str) -> ResolvedModel {
        let model = self.inner.spec.model(id);
        let info = model.map(|m| m.info()).unwrap_or_else(|| ModelInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            context_window: crate::provider::DEFAULT_CONTEXT_WINDOW,
            input_modalities: vec!["text".into()],
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
        });
        ResolvedModel {
            info,
            description: None,
            default_max_tokens: model
                .map(|m| m.max_tokens.min(DEFAULT_REQUEST_MAX_TOKENS))
                .unwrap_or(DEFAULT_REQUEST_MAX_TOKENS),
            system_prompt_update: None,
            tool_update: None,
            cost: model.and_then(|m| m.cost),
        }
    }

    /// Builds the exact JSON body [`LlmClient::stream`] would send (images loaded).
    pub async fn build_request(&self, request: &GenerateRequest) -> Result<Value, LlmFailure> {
        let inner = &self.inner;
        let spec = &inner.spec;
        let model = spec.model(&request.model);
        let accepts = model.is_some_and(|m| m.accepts_images());
        // The DeepSeek projection with this route's facts: no tool updates,
        // images only for vision models, foreign replay data dropped.
        let projected_model = CatalogModel {
            id: request.model.clone(),
            name: None,
            description: None,
            context_window: None,
            max_tokens: None,
            input_modalities: if accepts {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            },
            system_prompt_update: None,
            tool_update: None,
        };
        let image_path = inner.image_path.as_deref();
        let history = project_messages(
            &request.messages,
            Some(&projected_model),
            &spec.id,
            image_path,
        );
        let refs = request_image_refs(&history).map_err(|mut e| {
            e.message = e.message.replace("DeepSeek Messages", self.label());
            e
        })?;
        let images =
            runner::load_images(self.label(), inner.attachments.as_deref(), refs, accepts).await?;
        match spec.api {
            Api::AnthropicMessages => anthropic::serialize(
                request,
                &history,
                &anthropic::Build {
                    spec,
                    images: &images,
                    image_path,
                },
            ),
            Api::OpenAiCompletions => openai::serialize(
                request,
                &history,
                &openai::Build {
                    spec,
                    images: &images,
                    image_path,
                },
            ),
        }
    }

    fn url(&self) -> String {
        let base = &self.inner.spec.base_url;
        match self.inner.spec.api {
            Api::AnthropicMessages => anthropic::messages_url(base),
            Api::OpenAiCompletions => openai::completions_url(base),
        }
    }

    async fn run(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
        tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), runner::Stop> {
        let label = self.label();
        if cancel.is_cancelled() {
            return Err(runner::aborted(label).into());
        }
        let inner = &self.inner;
        let spec = &inner.spec;
        let body = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(runner::aborted(label).into()),
            body = self.build_request(&request) => body?,
        };
        let key = resolve_provider_key(
            inner.api_key.as_ref(),
            &spec.id,
            spec.api_key_env.as_deref(),
            spec.key_required,
        )
        .await?;
        let mut http = inner
            .http
            .post(self.url())
            .header("user-agent", user_agent())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if spec.api == Api::AnthropicMessages {
            http = http.header("anthropic-version", anthropic::ANTHROPIC_VERSION);
        }
        if let Some(key) = &key {
            http = if spec.compat.bearer {
                http.bearer_auth(key)
            } else {
                http.header("x-api-key", key)
            };
        }
        for (name, value) in &spec.headers {
            http = http.header(name, value);
        }
        let http = http.body(crate::json::to_vec(&body));
        match spec.api {
            Api::AnthropicMessages => {
                let translator = anthropic::Translator::new(spec.id.clone(), request.model.clone());
                runner::run(label, http, inner.idle, &cancel, &tx, translator).await
            }
            Api::OpenAiCompletions => {
                runner::run(
                    label,
                    http,
                    inner.idle,
                    &cancel,
                    &tx,
                    openai::Translator::new(),
                )
                .await
            }
        }
    }
}

#[async_trait]
impl LlmClient for ProviderClient {
    fn provider(&self) -> &str {
        &self.inner.spec.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.inner.spec.models.iter().map(|m| m.info()).collect()
    }

    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        let client = self.clone();
        runner::spawn(self.label(), cancel, move |tx, cancel| async move {
            client.run(request, cancel, tx).await
        })
    }
}

/// Stands in for a provider that is not (or no longer) configured: every
/// request fails with `PROVIDER_UNAVAILABLE` instead of reaching another route.
pub struct UnavailableRoute {
    provider: String,
}

impl UnavailableRoute {
    pub fn new(provider: impl Into<String>) -> Self {
        UnavailableRoute {
            provider: provider.into(),
        }
    }
}

#[async_trait]
impl LlmClient for UnavailableRoute {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }

    async fn stream(
        &self,
        _request: GenerateRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        let failure = LlmFailure::new(
            "PROVIDER_UNAVAILABLE",
            format!(
                "model provider \"{}\" is not configured; add it in Settings → Models or pick another model",
                self.provider
            ),
        );
        Box::pin(futures::stream::iter([StreamChunk::Finish {
            reason: dshx_core::FinishReason::Error { failure },
            replay_state: None,
        }]))
    }
}
