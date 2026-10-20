//! The HTTP client (dsh `llm-deepseek/src/adapter.ts`): one cancellable
//! streaming request per call, failures as a terminal `Finish`.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{
    AttachmentStore, FinishReason, GenerateRequest, LlmClient, LlmFailure, ModelInfo, StreamChunk,
};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::auth::{ApiKeySource, EnvApiKey, resolve_api_key};
use crate::config::{ConfigError, DeepSeekConfig};
use crate::images::{ImagePathResolver, RequestImage};
use crate::models::{PROVIDER, ResolvedModel};
use crate::serialize::{
    BuildContext, TOOL_CHANGES_BETA, WireRequest, project_messages, request_image_refs, serialize,
};
use crate::sse::{SseItem, SseParser, decode_event};
use crate::translate::Translator;
use crate::transport::{ErrorHeaders, http_error};

/// `anthropic-version` header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Request header carrying the session id.
pub const SESSION_ID_HEADER: &str = "x-deepseek-harness-session-id";

/// DeepSeek Messages adapter for route `deepseek-official`. Cheap to clone.
#[derive(Clone)]
pub struct DeepSeekClient {
    inner: Arc<Inner>,
}

struct Inner {
    config: DeepSeekConfig,
    provider: String,
    api_key: Arc<dyn ApiKeySource>,
    http: reqwest::Client,
    attachments: Option<Arc<dyn AttachmentStore>>,
    image_path: Option<Arc<ImagePathResolver>>,
}

/// Builder for [`DeepSeekClient`].
pub struct DeepSeekClientBuilder {
    config: DeepSeekConfig,
    provider: String,
    api_key: Arc<dyn ApiKeySource>,
    attachments: Option<Arc<dyn AttachmentStore>>,
    image_path: Option<Arc<ImagePathResolver>>,
}

impl DeepSeekClientBuilder {
    /// Key source (default: `DEEPSEEK_API_KEY`, read per request).
    pub fn api_key(mut self, source: Arc<dyn ApiKeySource>) -> Self {
        self.api_key = source;
        self
    }
    /// Store that holds the bytes of image blocks (required for image input).
    pub fn attachments(mut self, store: Arc<dyn AttachmentStore>) -> Self {
        self.attachments = Some(store);
        self
    }
    /// Read-only path of an attachment as model tools see it; shown beside each image like dsh.
    pub fn image_path(mut self, resolver: Arc<ImagePathResolver>) -> Self {
        self.image_path = Some(resolver);
        self
    }
    /// Provider route name (default `deepseek-official`).
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }
    pub fn build(self) -> Result<DeepSeekClient, ConfigError> {
        self.config.validate()?;
        let http = reqwest::Client::builder()
            // Credentials never follow a redirect to another origin.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ConfigError(format!("HTTP client: {e}")))?;
        Ok(DeepSeekClient {
            inner: Arc::new(Inner {
                config: self.config,
                provider: self.provider,
                api_key: self.api_key,
                http,
                attachments: self.attachments,
                image_path: self.image_path,
            }),
        })
    }
}

impl DeepSeekClient {
    pub fn builder(config: DeepSeekConfig) -> DeepSeekClientBuilder {
        DeepSeekClientBuilder {
            config,
            provider: PROVIDER.into(),
            api_key: Arc::new(EnvApiKey::default()),
            attachments: None,
            image_path: None,
        }
    }

    /// Defaults, `DEEPSEEK_BASE_URL` and `DEEPSEEK_API_KEY`.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::builder(DeepSeekConfig::from_env()).build()
    }

    pub fn config(&self) -> &DeepSeekConfig {
        &self.inner.config
    }

    /// Exact-route metadata, including dsh's `systemPromptUpdate`/`toolUpdate`.
    pub fn resolve_model(&self, id: &str) -> ResolvedModel {
        let config = &self.inner.config;
        crate::models::resolve(
            config.model(id),
            id,
            config.default_context_window,
            config.max_tokens,
            config.thinking_disabled,
            config.reasoning_effort.as_deref(),
        )
    }

    /// The provider-owned retry policy the agent executes.
    pub fn retry_policy(&self) -> &crate::retry::RetryPolicy {
        &self.inner.config.retry_policy
    }

    /// Builds the exact request [`LlmClient::stream`] would send (images loaded).
    pub async fn build_request(
        &self,
        request: &GenerateRequest,
    ) -> Result<WireRequest, LlmFailure> {
        let inner = &self.inner;
        let model = inner.config.model(&request.model);
        let image_path = inner.image_path.as_deref();
        let history = project_messages(&request.messages, model, &inner.provider, image_path);
        let refs = request_image_refs(&history)?;
        let mut images = HashMap::new();
        if !refs.is_empty() {
            let store = inner.attachments.as_ref().filter(|_| model.is_some_and(|m| m.accepts_images())).ok_or_else(
                || {
                    LlmFailure::new(
                        "UNSUPPORTED_CONTENT",
                        "DeepSeek Messages image input requires a vision model and attachment service",
                    )
                },
            )?;
            for r in refs {
                let (data, media_type) = store.load(&r.attachment_id).await.map_err(|e| {
                    LlmFailure::new(
                        "INVALID_REQUEST",
                        format!(
                            "DeepSeek Messages request image {} is unavailable: {e}",
                            r.attachment_id
                        ),
                    )
                })?;
                let media_type = if media_type.is_empty() {
                    r.media_type.clone()
                } else {
                    media_type
                };
                images.insert(
                    r.attachment_id.clone(),
                    RequestImage {
                        data,
                        media_type,
                        width: r.width,
                        height: r.height,
                    },
                );
            }
        }
        let ctx = BuildContext {
            config: &inner.config,
            provider: &inner.provider,
            images: &images,
            image_path,
        };
        serialize(request, &history, &ctx)
    }
}

#[async_trait]
impl LlmClient for DeepSeekClient {
    fn provider(&self) -> &str {
        &self.inner.provider
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.inner
            .config
            .models
            .iter()
            .map(|m| self.resolve_model(&m.id).info)
            .collect()
    }

    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        let (tx, rx) = mpsc::channel(256);
        let client = self.clone();
        tokio::spawn(async move {
            let outcome = client.run(&request, &cancel, &tx).await;
            let reason = match outcome {
                Ok(()) | Err(Stop::ConsumerGone) => return,
                Err(Stop::Failed(failure))
                    if cancel.is_cancelled() || failure.code == "ABORTED" =>
                {
                    FinishReason::Aborted {
                        failure: aborted_or(failure),
                    }
                }
                Err(Stop::Failed(failure)) => FinishReason::Error { failure },
            };
            let _ = tx
                .send(StreamChunk::Finish {
                    reason,
                    replay_state: None,
                })
                .await;
        });
        futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }
}

enum Stop {
    Failed(LlmFailure),
    ConsumerGone,
}

impl From<LlmFailure> for Stop {
    fn from(failure: LlmFailure) -> Self {
        Stop::Failed(failure)
    }
}

fn aborted() -> LlmFailure {
    LlmFailure::new("ABORTED", "DeepSeek Messages request aborted")
}

fn aborted_or(failure: LlmFailure) -> LlmFailure {
    if failure.code == "ABORTED" {
        failure
    } else {
        aborted()
    }
}

fn idle_timeout() -> LlmFailure {
    LlmFailure::new("TIMEOUT", "DeepSeek Messages stream idle timeout")
}

fn transport(error: &reqwest::Error) -> LlmFailure {
    let mut cause = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(err) = source {
        let text = err.to_string();
        if !cause.contains(&text) {
            cause.push_str(": ");
            cause.push_str(&text);
        }
        source = err.source();
    }
    LlmFailure::new(
        "TRANSPORT",
        format!("DeepSeek Messages transport failed{cause}"),
    )
}

impl DeepSeekClient {
    async fn run(
        &self,
        request: &GenerateRequest,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<StreamChunk>,
    ) -> Result<(), Stop> {
        if cancel.is_cancelled() {
            return Err(aborted().into());
        }
        let inner = &self.inner;
        let idle = inner.config.stream_idle_timeout;
        let wire = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(aborted().into()),
            wire = self.build_request(request) => wire?,
        };
        let key = resolve_api_key(inner.api_key.as_ref()).await?;
        let mut http = inner
            .http
            .post(inner.config.messages_url())
            .header("user-agent", &inner.config.user_agent)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if wire.tool_changes {
            http = http.header("anthropic-beta", TOOL_CHANGES_BETA);
        }
        if inner.config.send_session_id
            && let Some(session) = &request.session_id
        {
            http = http.header(SESSION_ID_HEADER, session);
        }
        let http = http.body(crate::json::to_vec(&wire.body));

        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(aborted().into()),
            _ = tx.closed() => return Err(Stop::ConsumerGone),
            sent = tokio::time::timeout(idle, http.send()) => match sent {
                Err(_) => return Err(idle_timeout().into()),
                Ok(Err(e)) => return Err(transport(&e).into()),
                Ok(Ok(response)) => response,
            },
        };
        let status = response.status();
        if !status.is_success() {
            let headers = ErrorHeaders::from_map(response.headers());
            let text = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(aborted().into()),
                text = tokio::time::timeout(idle, response.text()) => match text {
                    Err(_) => return Err(idle_timeout().into()),
                    Ok(Err(e)) => return Err(transport(&e).into()),
                    Ok(Ok(text)) => text,
                },
            };
            return Err(http_error(status.as_u16(), &text, &headers).into());
        }

        let mut body = response.bytes_stream();
        let mut parser = SseParser::new();
        let mut translator = Translator::new(request.model.clone());
        let mut deadline = Instant::now() + idle;
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(aborted().into()),
                _ = tx.closed() => return Err(Stop::ConsumerGone),
                _ = tokio::time::sleep_until(deadline) => return Err(idle_timeout().into()),
                next = body.next() => next,
            };
            let bytes = match next {
                None => return translator.finish().map_err(Stop::from),
                Some(Err(e)) => return Err(transport(&e).into()),
                Some(Ok(bytes)) => bytes,
            };
            for item in parser.feed(&bytes) {
                // Events and heartbeat comments both prove the stream is alive.
                deadline = Instant::now() + idle;
                let SseItem::Event { event, data } = item else {
                    continue;
                };
                let event = decode_event(event.as_deref(), &data)?;
                for chunk in translator.push(&event)? {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(aborted().into()),
                        sent = tx.send(chunk) => if sent.is_err() { return Err(Stop::ConsumerGone) },
                    }
                }
                if translator.is_done() {
                    // Dropping the body closes the connection like dsh's abort.
                    return Ok(());
                }
            }
        }
    }
}
