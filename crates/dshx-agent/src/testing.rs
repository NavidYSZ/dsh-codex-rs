//! Test doubles: a scripted [`LlmClient`] that streams like dsh's DeepSeek
//! adapter, configurable fake tools, a fixed approver and a recording observer.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ContentBlock, FinishReason, GenerateRequest,
    ImageAttachmentRef, LlmClient, LlmFailure, ModelInfo, NewImage, StreamChunk, TokenUsage, Tool,
    ToolContext, ToolError, ToolOutput, ToolSchema,
};
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::observer::{AgentObserver, AgentStatus, StreamDelta};

/// One scripted model response.
#[derive(Debug, Clone)]
pub enum Response {
    /// Exactly these chunks.
    Chunks(Vec<StreamChunk>),
    /// These chunks, then the stream never ends (until the caller cancels).
    Hang(Vec<StreamChunk>),
    /// These chunks with a pause before each one.
    Slow(Vec<StreamChunk>, Duration),
}

/// One assistant block for [`chunks`].
#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    Reasoning(String, String),
    Tool(String, String, String),
}

fn usage(output: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: 100,
        output_tokens: output,
        total_tokens: Some(100 + output),
        ..Default::default()
    }
}

/// Chunks like dsh's DeepSeek adapter emits for `blocks`, with its replay envelope.
pub fn chunks(model: &str, blocks: &[Block], finish: FinishReason) -> Vec<StreamChunk> {
    let mut out = Vec::new();
    let mut replay_blocks = Vec::new();
    for (index, b) in blocks.iter().enumerate() {
        match b {
            Block::Text(t) => {
                out.push(StreamChunk::BlockStart {
                    index,
                    block_type: "text",
                });
                out.push(StreamChunk::TextDelta {
                    index,
                    text: t.clone(),
                });
                out.push(StreamChunk::BlockEnd {
                    index,
                    block: ContentBlock::text(t.clone()),
                });
                replay_blocks.push(json!({"type": "text"}));
            }
            Block::Reasoning(t, sig) => {
                out.push(StreamChunk::BlockStart {
                    index,
                    block_type: "reasoning",
                });
                out.push(StreamChunk::ReasoningDelta {
                    index,
                    text: t.clone(),
                });
                out.push(StreamChunk::BlockEnd {
                    index,
                    block: ContentBlock::Reasoning { text: t.clone() },
                });
                replay_blocks.push(json!({"type": "reasoning", "signature": sig}));
            }
            Block::Tool(id, name, args) => {
                out.push(StreamChunk::BlockStart {
                    index,
                    block_type: "tool-call",
                });
                out.push(StreamChunk::ToolCallDelta {
                    index,
                    id: id.clone(),
                    name: Some(name.clone()),
                    arguments_delta: String::new(),
                });
                out.push(StreamChunk::ToolCallDelta {
                    index,
                    id: id.clone(),
                    name: None,
                    arguments_delta: args.clone(),
                });
                out.push(StreamChunk::BlockEnd {
                    index,
                    block: ContentBlock::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: args.clone(),
                    },
                });
                replay_blocks.push(json!({"type": "tool-call"}));
            }
        }
    }
    let output = if blocks.iter().any(|b| matches!(b, Block::Tool(..))) {
        20
    } else {
        5
    };
    out.push(StreamChunk::Usage(usage(output)));
    out.push(StreamChunk::Finish {
        reason: finish,
        replay_state: Some(json!({
            "response": {"kind": "deepseek-messages", "version": 1, "model": model},
            "blocks": replay_blocks,
        })),
    });
    out
}

pub fn text(model: &str, t: &str) -> Response {
    Response::Chunks(chunks(model, &[Block::Text(t.into())], FinishReason::Stop))
}

pub fn tool_calls(model: &str, calls: &[(&str, &str, &str)]) -> Response {
    let blocks: Vec<Block> = calls
        .iter()
        .map(|(i, n, a)| Block::Tool((*i).into(), (*n).into(), (*a).into()))
        .collect();
    Response::Chunks(chunks(model, &blocks, FinishReason::ToolCalls))
}

pub fn failure(code: &str, message: &str) -> Response {
    Response::Chunks(vec![StreamChunk::Finish {
        reason: FinishReason::Error {
            failure: LlmFailure::new(code, message),
        },
        replay_state: None,
    }])
}

/// A fake [`LlmClient`] answering from a script. Title and compaction requests
/// are answered on the side (like `tools/dev/mock_deepseek.py`).
pub struct ScriptedLlm {
    script: Mutex<VecDeque<Response>>,
    /// Main-loop requests in order.
    pub requests: Mutex<Vec<GenerateRequest>>,
    /// Title and compaction requests.
    pub side_requests: Mutex<Vec<GenerateRequest>>,
    pub title: Mutex<String>,
    pub summary: Mutex<String>,
    /// Exact answer to compaction requests (default: `summary` as plain text).
    pub summary_response: Mutex<Option<Response>>,
    models: Vec<ModelInfo>,
}

impl ScriptedLlm {
    pub fn new(script: Vec<Response>) -> Arc<Self> {
        Arc::new(ScriptedLlm {
            script: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
            side_requests: Mutex::new(Vec::new()),
            title: Mutex::new("Testtitel".into()),
            summary: Mutex::new("## Primary Request and Intent\n- summary".into()),
            summary_response: Mutex::new(None),
            models: vec![
                ModelInfo {
                    id: "deepseek-flash".into(),
                    name: "DeepSeek-V41-Flash".into(),
                    context_window: 1_000_000,
                    input_modalities: vec!["text".into(), "image".into()],
                    reasoning_efforts: vec![
                        "off".into(),
                        "low".into(),
                        "high".into(),
                        "max".into(),
                    ],
                    default_reasoning_effort: Some("high".into()),
                },
                ModelInfo {
                    id: "deepseek-v4-pro".into(),
                    name: "DeepSeek-V4-Pro".into(),
                    context_window: 1_000_000,
                    input_modalities: vec!["text".into()],
                    reasoning_efforts: vec![
                        "off".into(),
                        "low".into(),
                        "high".into(),
                        "max".into(),
                    ],
                    default_reasoning_effort: Some("high".into()),
                },
            ],
        })
    }

    pub fn push(&self, r: Response) {
        self.script.lock().unwrap().push_back(r);
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn is_title(r: &GenerateRequest) -> bool {
    r.messages.first().is_some_and(|m| {
        m.role == dshx_core::Role::System && m.plain_text().contains("concise title")
    })
}

fn is_compaction(r: &GenerateRequest) -> bool {
    r.messages
        .last()
        .is_some_and(|m| m.plain_text().contains("compaction engine"))
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    fn provider(&self) -> &str {
        "deepseek-official"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    async fn stream(
        &self,
        request: GenerateRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        let model = request.model.clone();
        if is_title(&request) {
            self.side_requests.lock().unwrap().push(request);
            let t = self.title.lock().unwrap().clone();
            return stream::iter(chunks(&model, &[Block::Text(t)], FinishReason::Stop)).boxed();
        }
        if is_compaction(&request) {
            self.side_requests.lock().unwrap().push(request);
            if let Some(Response::Chunks(c)) = self.summary_response.lock().unwrap().clone() {
                return stream::iter(c).boxed();
            }
            let t = self.summary.lock().unwrap().clone();
            return stream::iter(chunks(&model, &[Block::Text(t)], FinishReason::Stop)).boxed();
        }
        self.requests.lock().unwrap().push(request);
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Response::Chunks(c)) => stream::iter(c).boxed(),
            Some(Response::Hang(c)) => stream::iter(c).chain(stream::pending()).boxed(),
            Some(Response::Slow(c, d)) => stream::iter(c)
                .then(move |chunk| async move {
                    tokio::time::sleep(d).await;
                    chunk
                })
                .boxed(),
            None => stream::iter(chunks(
                &model,
                &[Block::Text("FERTIG".into())],
                FinishReason::Stop,
            ))
            .boxed(),
        }
    }
}

type Behavior = Arc<
    dyn Fn(Value, ToolContext) -> BoxFuture<'static, Result<ToolOutput, ToolError>> + Send + Sync,
>;

/// A configurable tool; `events` records `start:<name>`/`end:<name>` in order.
pub struct FakeTool {
    pub schema: ToolSchema,
    pub safe: bool,
    pub timeout: Option<Duration>,
    pub behavior: Behavior,
    pub events: Arc<Mutex<Vec<String>>>,
}

impl FakeTool {
    pub fn new(
        name: &str,
        parameters: Value,
        safe: bool,
        events: Arc<Mutex<Vec<String>>>,
        behavior: Behavior,
    ) -> Arc<Self> {
        Arc::new(FakeTool {
            schema: ToolSchema {
                name: name.into(),
                description: format!("fake {name}"),
                parameters,
            },
            safe,
            timeout: None,
            behavior,
            events,
        })
    }

    /// Echoes after `delay`, honoring cancellation.
    pub fn sleeper(
        name: &str,
        safe: bool,
        delay: Duration,
        events: Arc<Mutex<Vec<String>>>,
    ) -> Arc<Self> {
        Self::new(
            name,
            json!({"type": "object", "properties": {"tag": {"type": "string"}}}),
            safe,
            events,
            Arc::new(move |args, ctx| {
                Box::pin(async move {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                    }
                    Ok(ToolOutput::text(format!(
                        "done {}",
                        args["tag"].as_str().unwrap_or("")
                    )))
                })
            }),
        )
    }
}

#[async_trait]
impl Tool for FakeTool {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
    fn concurrency_safe(&self) -> bool {
        self.safe
    }
    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let tag = args
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or(&self.schema.name)
            .to_owned();
        self.events.lock().unwrap().push(format!("start:{tag}"));
        let r = (self.behavior)(args, ctx).await;
        self.events.lock().unwrap().push(format!("end:{tag}"));
        r
    }
}

/// Answers every approval with a fixed decision.
pub struct FixedApprover(pub ApprovalDecision);

#[async_trait]
impl Approver for FixedApprover {
    async fn approve(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> ApprovalDecision {
        self.0
    }
}

/// Attachment store that keeps images in memory.
#[derive(Default)]
pub struct MemoryAttachments {
    images: Mutex<Vec<(String, Vec<u8>, String)>>,
}

#[async_trait]
impl AttachmentStore for MemoryAttachments {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        let id = format!("att-{}", dshx_core::new_id());
        let r = ImageAttachmentRef {
            attachment_id: id.clone(),
            media_type: image.media_type.clone(),
            bytes: image.data.len() as u64,
            width: image.width,
            height: image.height,
            name: image.name,
            original_dimensions: None,
        };
        self.images
            .lock()
            .unwrap()
            .push((id, image.data, image.media_type));
        Ok(r)
    }

    async fn load(&self, attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.images
            .lock()
            .unwrap()
            .iter()
            .find(|(id, ..)| id == attachment_id)
            .map(|(_, d, t)| (d.clone(), t.clone()))
            .ok_or_else(|| anyhow::anyhow!("unknown attachment {attachment_id}"))
    }
}

/// Records deltas and status changes.
#[derive(Default)]
pub struct RecordingObserver {
    pub deltas: Mutex<Vec<StreamDelta>>,
    pub statuses: Mutex<Vec<AgentStatus>>,
    pub attempts: Mutex<Vec<(u64, u64, u32)>>,
}

impl AgentObserver for RecordingObserver {
    fn on_delta(&self, delta: &StreamDelta) {
        self.deltas.lock().unwrap().push(delta.clone());
    }
    fn on_status(&self, status: &AgentStatus) {
        self.statuses.lock().unwrap().push(status.clone());
    }
    fn on_attempt(&self, turn: u64, step: u64, attempt: u32) {
        self.attempts.lock().unwrap().push((turn, step, attempt));
    }
}
