//! One cancellable streaming request of a provider route (the same rules as
//! [`crate::DeepSeekClient`]'s loop: idle timeout per read, failures as a
//! terminal `Finish`, dropping the body on cancel), shared by the Anthropic
//! Messages and OpenAI Chat Completions adapters.
use std::collections::HashMap;
use std::time::Duration;

use dshx_core::{AttachmentStore, FinishReason, ImageAttachmentRef, LlmFailure, StreamChunk};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::images::RequestImage;
use crate::sse::{SseItem, SseParser};
use crate::transport::{ErrorHeaders, labeled_http_error};

/// Turns SSE frames of one response into chunks.
pub(crate) trait Translate: Send {
    /// One complete frame.
    fn frame(&mut self, event: Option<&str>, data: &str) -> Result<Vec<StreamChunk>, LlmFailure>;
    /// The response is complete; later bytes are ignored.
    fn is_done(&self) -> bool;
    /// The body ended: the final chunks, or the failure of a cut-off stream.
    fn end(&mut self) -> Result<Vec<StreamChunk>, LlmFailure>;
}

pub(crate) enum Stop {
    Failed(LlmFailure),
    ConsumerGone,
}

impl From<LlmFailure> for Stop {
    fn from(failure: LlmFailure) -> Self {
        Stop::Failed(failure)
    }
}

/// Runs `work` in the background and streams its chunks; a failure becomes
/// the terminal `Finish` (aborted when cancelled).
pub(crate) fn spawn<F, Fut>(
    label: &'static str,
    cancel: CancellationToken,
    work: F,
) -> BoxStream<'static, StreamChunk>
where
    F: FnOnce(mpsc::Sender<StreamChunk>, CancellationToken) -> Fut,
    Fut: std::future::Future<Output = Result<(), Stop>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(256);
    let fut = work(tx.clone(), cancel.clone());
    tokio::spawn(async move {
        let reason = match fut.await {
            Ok(()) | Err(Stop::ConsumerGone) => return,
            Err(Stop::Failed(failure)) if cancel.is_cancelled() || failure.code == "ABORTED" => {
                FinishReason::Aborted {
                    failure: aborted(label),
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

pub(crate) fn aborted(label: &str) -> LlmFailure {
    LlmFailure::new("ABORTED", format!("{label} request aborted"))
}

fn idle_timeout(label: &str) -> LlmFailure {
    LlmFailure::new("TIMEOUT", format!("{label} stream idle timeout"))
}

fn transport(label: &str, error: &reqwest::Error) -> LlmFailure {
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
    LlmFailure::new("TRANSPORT", format!("{label} transport failed{cause}"))
}

/// Sends `request` and feeds the SSE body through `translator` into `tx`.
pub(crate) async fn run(
    label: &str,
    request: reqwest::RequestBuilder,
    idle: Duration,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<StreamChunk>,
    mut translator: impl Translate,
) -> Result<(), Stop> {
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(aborted(label).into()),
        _ = tx.closed() => return Err(Stop::ConsumerGone),
        sent = tokio::time::timeout(idle, request.send()) => match sent {
            Err(_) => return Err(idle_timeout(label).into()),
            Ok(Err(e)) => return Err(transport(label, &e).into()),
            Ok(Ok(response)) => response,
        },
    };
    let status = response.status();
    if !status.is_success() {
        let headers = ErrorHeaders::from_map(response.headers());
        let text = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(aborted(label).into()),
            text = tokio::time::timeout(idle, response.text()) => match text {
                Err(_) => return Err(idle_timeout(label).into()),
                Ok(Err(e)) => return Err(transport(label, &e).into()),
                Ok(Ok(text)) => text,
            },
        };
        return Err(labeled_http_error(label, status.as_u16(), &text, &headers).into());
    }

    let mut body = response.bytes_stream();
    let mut parser = SseParser::new();
    let mut deadline = Instant::now() + idle;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(aborted(label).into()),
            _ = tx.closed() => return Err(Stop::ConsumerGone),
            _ = tokio::time::sleep_until(deadline) => return Err(idle_timeout(label).into()),
            next = body.next() => next,
        };
        let bytes = match next {
            None => {
                let chunks = translator.end()?;
                return send_all(chunks, cancel, tx, label).await;
            }
            Some(Err(e)) => return Err(transport(label, &e).into()),
            Some(Ok(bytes)) => bytes,
        };
        for item in parser.feed(&bytes) {
            // Events and heartbeat comments both prove the stream is alive.
            deadline = Instant::now() + idle;
            let SseItem::Event { event, data } = item else {
                continue;
            };
            let chunks = translator.frame(event.as_deref(), &data)?;
            send_all(chunks, cancel, tx, label).await?;
            if translator.is_done() {
                return Ok(());
            }
        }
    }
}

async fn send_all(
    chunks: Vec<StreamChunk>,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<StreamChunk>,
    label: &str,
) -> Result<(), Stop> {
    for chunk in chunks {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(aborted(label).into()),
            sent = tx.send(chunk) => if sent.is_err() { return Err(Stop::ConsumerGone) },
        }
    }
    Ok(())
}

/// Loads the bytes of every retained image; `accepts` = the model takes images.
pub(crate) async fn load_images(
    label: &str,
    store: Option<&dyn AttachmentStore>,
    refs: Vec<ImageAttachmentRef>,
    accepts: bool,
) -> Result<HashMap<String, RequestImage>, LlmFailure> {
    let mut images = HashMap::new();
    if refs.is_empty() {
        return Ok(images);
    }
    let store = store.filter(|_| accepts).ok_or_else(|| {
        LlmFailure::new(
            "UNSUPPORTED_CONTENT",
            format!("{label} image input requires a vision model and attachment service"),
        )
    })?;
    for r in refs {
        let (data, media_type) = store.load(&r.attachment_id).await.map_err(|e| {
            LlmFailure::new(
                "INVALID_REQUEST",
                format!(
                    "{label} request image {} is unavailable: {e}",
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
    Ok(images)
}
