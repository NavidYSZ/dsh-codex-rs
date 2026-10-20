//! Scripted stand-in for DeepSeek's Anthropic-compatible Messages API.
//!
//! [`MockDeepSeek`] listens on `127.0.0.1:0`, answers `POST …/messages` with
//! SSE exactly like the real endpoint (the official dsh parses it too) and
//! records every request for assertions. Each request takes the next
//! [`Reply`] from the script; when the script is empty an optional
//! responder decides, otherwise the mock answers HTTP 500.
//!
//! ```no_run
//! # async fn demo() {
//! use dshx_testkit::{MockDeepSeek, Reply};
//! use serde_json::json;
//! let mock = MockDeepSeek::start().await;
//! mock.push(Reply::stream().thinking("Look first.", "sig-1").tool_call("toolu_1", "bash", json!({"command": "ls"})));
//! mock.push(Reply::rate_limited(1));
//! mock.push(Reply::text("Done."));
//! // DEEPSEEK_BASE_URL = mock.base_url()   (http://127.0.0.1:PORT/anthropic)
//! // ... run the code under test ...
//! let first = &mock.requests()[0];
//! assert_eq!(first.body["model"], "deepseek-flash");
//! # }
//! ```
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};

pub mod scenarios;

/// Decides a reply from the request when the script is empty.
pub type Responder = dyn Fn(&RecordedRequest) -> Reply + Send + Sync;

/// One request as received.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// Header names in lowercase, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Exact body bytes (for byte-level comparisons).
    pub raw_body: Vec<u8>,
    /// The body parsed as JSON (`Null` if it is not JSON).
    pub body: Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
    /// `body.messages` (empty if absent).
    pub fn messages(&self) -> &[Value] {
        self.body
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    /// `body.system` as a string.
    pub fn system(&self) -> Option<&str> {
        self.body.get("system").and_then(Value::as_str)
    }
    /// Number of `tool_result` blocks in the history, i.e. completed tool steps.
    pub fn tool_results(&self) -> usize {
        self.messages()
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_array))
            .flatten()
            .filter(|b| b["type"] == "tool_result")
            .count()
    }
    /// Whether this is a Messages call (`POST …/messages`).
    pub fn is_messages(&self) -> bool {
        self.method == "POST" && self.path.trim_end_matches('/').ends_with("/messages")
    }
}

/// What the mock answers to one request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// `200 text/event-stream` with scripted events.
    Stream(StreamReply),
    /// Any other HTTP response (errors).
    Http {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// `200 text/event-stream` with this exact body text.
    RawSse(String),
}

impl From<StreamReply> for Reply {
    fn from(stream: StreamReply) -> Self {
        Reply::Stream(stream)
    }
}

impl Reply {
    /// Starts a streamed response; add blocks with the builder methods.
    pub fn stream() -> StreamReply {
        StreamReply::default()
    }
    /// A streamed answer with one text block.
    pub fn text(text: impl Into<String>) -> Reply {
        Reply::stream().text(text).into()
    }
    /// A streamed answer with one tool call (`stop_reason: tool_use`).
    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, input: Value) -> Reply {
        Reply::stream().tool_call(id, name, input).into()
    }
    /// An error response with an Anthropic error envelope.
    pub fn error(status: u16, kind: &str, message: &str) -> Reply {
        let body =
            json!({"type": "error", "error": {"type": kind, "message": message}}).to_string();
        Reply::Http {
            status,
            headers: vec![],
            body,
        }
    }
    /// `429 rate_limit_error` with `retry-after: <secs>`.
    pub fn rate_limited(retry_after_secs: u64) -> Reply {
        Reply::error(429, "rate_limit_error", "Rate limit reached")
            .header("retry-after", retry_after_secs.to_string())
    }
    /// `500 api_error`.
    pub fn server_error() -> Reply {
        Reply::error(500, "api_error", "Internal server error")
    }
    /// `529 overloaded_error`.
    pub fn overloaded() -> Reply {
        Reply::error(529, "overloaded_error", "Overloaded")
    }
    /// `400` that dsh classifies as `CONTEXT_WINDOW_EXCEEDED`.
    pub fn context_overflow() -> Reply {
        Reply::error(
            400,
            "invalid_request_error",
            "This model's maximum context length is 1000000 tokens. However, you requested 1200000 tokens.",
        )
    }
    /// A raw response, e.g. a non-JSON gateway error.
    pub fn http(status: u16, body: impl Into<String>) -> Reply {
        Reply::Http {
            status,
            headers: vec![],
            body: body.into(),
        }
    }
    /// Adds a header to an `Http` reply (no effect on streams).
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Reply {
        if let Reply::Http { headers, .. } = &mut self {
            headers.push((name.to_string(), value.into()));
        }
        self
    }
}

#[derive(Debug, Clone)]
enum Block {
    Text(Vec<String>),
    Thinking {
        chunks: Vec<String>,
        signature: Vec<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// Drop the TCP connection mid-body (a transport error for the client).
    Disconnect(usize),
    /// End the body cleanly before `message_stop`.
    End(usize),
    /// Keep the connection open without sending anything more.
    Stall(usize),
}

/// Builder of one streamed response. Blocks appear in call order.
#[derive(Debug, Clone)]
pub struct StreamReply {
    blocks: Vec<Block>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    stop_reason: Option<String>,
    model: Option<String>,
    ping: bool,
    delay: Duration,
    split_bytes: Option<usize>,
    cut: Option<Cut>,
    events: Option<Vec<Value>>,
    raw: Option<String>,
}

impl Default for StreamReply {
    fn default() -> Self {
        StreamReply {
            blocks: vec![],
            input_tokens: 100,
            output_tokens: 10,
            cache_read: None,
            cache_write: None,
            stop_reason: None,
            model: None,
            ping: false,
            delay: Duration::ZERO,
            split_bytes: None,
            cut: None,
            events: None,
            raw: None,
        }
    }
}

/// Splits `text` into about `parts` pieces at char boundaries.
fn pieces(text: &str, parts: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 || parts < 2 {
        return vec![text.to_string()];
    }
    let size = chars.len().div_ceil(parts);
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

impl StreamReply {
    /// A text block sent as one delta.
    pub fn text(self, text: impl Into<String>) -> Self {
        self.text_chunks([text.into()])
    }
    /// A text block streamed as these deltas.
    pub fn text_chunks<I: IntoIterator<Item = S>, S: Into<String>>(mut self, chunks: I) -> Self {
        self.blocks
            .push(Block::Text(chunks.into_iter().map(Into::into).collect()));
        self
    }
    /// A thinking block whose signature arrives as `signature_delta`s
    /// (none for an empty signature).
    pub fn thinking(self, text: impl Into<String>, signature: impl Into<String>) -> Self {
        let text = text.into();
        let signature = signature.into();
        let chunks = pieces(&text, 2);
        let signature = if signature.is_empty() {
            vec![]
        } else {
            pieces(&signature, 2)
        };
        self.thinking_chunks(chunks, signature)
    }
    /// A thinking block with explicit deltas.
    pub fn thinking_chunks<I, S, J, T>(mut self, chunks: I, signature: J) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        J: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.blocks.push(Block::Thinking {
            chunks: chunks.into_iter().map(Into::into).collect(),
            signature: signature.into_iter().map(Into::into).collect(),
        });
        self
    }
    /// A tool call whose input JSON streams as three `input_json_delta`s.
    pub fn tool_call(self, id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        let json = pieces(&input.to_string(), 3);
        self.tool_call_chunks(id, name, json)
    }
    /// A tool call with explicit `partial_json` deltas (may be invalid JSON).
    pub fn tool_call_chunks<I: IntoIterator<Item = S>, S: Into<String>>(
        mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        json: I,
    ) -> Self {
        self.blocks.push(Block::ToolUse {
            id: id.into(),
            name: name.into(),
            json: json.into_iter().map(Into::into).collect(),
        });
        self
    }
    /// Uncached input and output tokens (defaults 100 / 10).
    pub fn usage(mut self, input: u64, output: u64) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }
    /// Prefix-cache hits and writes (`cache_read_input_tokens`, `cache_creation_input_tokens`).
    pub fn cache(mut self, read: u64, write: u64) -> Self {
        self.cache_read = Some(read);
        self.cache_write = Some(write);
        self
    }
    /// Overrides the stop reason (default `tool_use` with tool calls, else `end_turn`).
    pub fn stop_reason(mut self, reason: impl Into<String>) -> Self {
        self.stop_reason = Some(reason.into());
        self
    }
    /// Model id echoed in `message_start` (default: the requested one).
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
    /// Sends a `ping` event after `message_start`.
    pub fn ping(mut self) -> Self {
        self.ping = true;
        self
    }
    /// Waits this long before every event (slow streams, idle timeouts).
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
    /// Writes the SSE text in pieces of `n` bytes (each its own HTTP chunk).
    pub fn split_bytes(mut self, n: usize) -> Self {
        self.split_bytes = Some(n.max(1));
        self
    }
    /// Drops the connection abruptly after `n` events.
    pub fn disconnect_after(mut self, n: usize) -> Self {
        self.cut = Some(Cut::Disconnect(n));
        self
    }
    /// Ends the body cleanly after `n` events (no `message_stop`).
    pub fn end_after(mut self, n: usize) -> Self {
        self.cut = Some(Cut::End(n));
        self
    }
    /// Sends `n` events, then nothing until the client goes away.
    pub fn stall_after(mut self, n: usize) -> Self {
        self.cut = Some(Cut::Stall(n));
        self
    }
    /// Replaces the generated events with these JSON events (`event:` = their `type`).
    pub fn events(mut self, events: Vec<Value>) -> Self {
        self.events = Some(events);
        self
    }

    /// The JSON events this reply sends for a request of `model`.
    pub fn build_events(&self, model: &str) -> Vec<Value> {
        if let Some(events) = &self.events {
            return events.clone();
        }
        let mut usage = json!({"input_tokens": self.input_tokens, "output_tokens": 0});
        if let Some(read) = self.cache_read {
            usage["cache_read_input_tokens"] = read.into();
        }
        if let Some(write) = self.cache_write {
            usage["cache_creation_input_tokens"] = write.into();
        }
        let model = self.model.clone().unwrap_or_else(|| model.to_string());
        let mut events = vec![json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_mock_{:08x}", next_id()),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage,
            },
        })];
        if self.ping {
            events.push(json!({"type": "ping"}));
        }
        for (index, block) in self.blocks.iter().enumerate() {
            match block {
                Block::Text(chunks) => {
                    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}));
                    for text in chunks {
                        events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}));
                    }
                }
                Block::Thinking { chunks, signature } => {
                    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "thinking", "thinking": ""}}));
                    for text in chunks {
                        events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": text}}));
                    }
                    for piece in signature {
                        events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "signature_delta", "signature": piece}}));
                    }
                }
                Block::ToolUse { id, name, json } => {
                    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}));
                    for partial in json {
                        events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": partial}}));
                    }
                }
            }
            events.push(json!({"type": "content_block_stop", "index": index}));
        }
        let stop = self.stop_reason.clone().unwrap_or_else(|| {
            if self
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ToolUse { .. }))
            {
                "tool_use".into()
            } else {
                "end_turn".into()
            }
        });
        events.push(json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": self.output_tokens}}));
        events.push(json!({"type": "message_stop"}));
        events
    }
}

fn next_id() -> u32 {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `event: <type>\ndata: <json>\n\n` for each event.
pub fn sse_text(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| {
            format!(
                "event: {}\ndata: {}\n\n",
                e["type"].as_str().unwrap_or("message"),
                e
            )
        })
        .collect()
}

#[derive(Default)]
struct Shared {
    script: Mutex<VecDeque<Reply>>,
    responder: Mutex<Option<Arc<Responder>>>,
    requests: Mutex<Vec<RecordedRequest>>,
    arrived: Notify,
}

/// The mock server. Dropping it stops the listener and every connection.
pub struct MockDeepSeek {
    addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
}

impl MockDeepSeek {
    /// Binds `127.0.0.1:0` and serves in the background (needs a tokio runtime).
    pub async fn start() -> MockDeepSeek {
        Self::start_on("127.0.0.1:0")
            .await
            .expect("bind mock DeepSeek")
    }

    /// Binds a specific address, e.g. `127.0.0.1:18999` for manual runs.
    pub async fn start_on(addr: &str) -> std::io::Result<MockDeepSeek> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let (shutdown, stop) = watch::channel(false);
        let accept_shared = shared.clone();
        tokio::spawn(async move {
            let mut stop_accept = stop.clone();
            loop {
                let accepted = tokio::select! {
                    _ = stop_accept.changed() => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((socket, _)) = accepted else { continue };
                let _ = socket.set_nodelay(true);
                tokio::spawn(serve(socket, accept_shared.clone(), stop.clone()));
            }
        });
        Ok(MockDeepSeek {
            addr,
            shared,
            shutdown,
        })
    }

    /// Starts with a script.
    pub async fn with_script(replies: impl IntoIterator<Item = Reply>) -> MockDeepSeek {
        let mock = Self::start().await;
        for reply in replies {
            mock.push(reply);
        }
        mock
    }

    /// `http://127.0.0.1:PORT/anthropic`, the value for `DEEPSEEK_BASE_URL`.
    pub fn base_url(&self) -> String {
        format!("http://{}/anthropic", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Appends a reply to the script.
    pub fn push(&self, reply: impl Into<Reply>) {
        self.shared
            .script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(reply.into());
    }

    /// Replies not yet used.
    pub fn remaining(&self) -> usize {
        self.shared
            .script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Answers requests that find the script empty (e.g. title requests).
    pub fn set_responder(
        &self,
        responder: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    ) {
        *self
            .shared
            .responder
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(responder));
    }

    /// Messages requests received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.all_requests()
            .into_iter()
            .filter(RecordedRequest::is_messages)
            .collect()
    }

    /// Every request received, including other paths.
    pub fn all_requests(&self) -> Vec<RecordedRequest> {
        self.shared
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Waits until `n` Messages requests arrived (panics after 10 s).
    pub async fn wait_for_requests(&self, n: usize) -> Vec<RecordedRequest> {
        let wait = async {
            loop {
                let arrived = self.shared.arrived.notified();
                let requests = self.requests();
                if requests.len() >= n {
                    return requests;
                }
                arrived.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .expect("mock DeepSeek: requests did not arrive")
    }
}

impl Drop for MockDeepSeek {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn serve(socket: TcpStream, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let (read, mut write) = socket.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let request = tokio::select! {
            _ = stop.changed() => return,
            request = read_request(&mut reader) => match request {
                Ok(Some(request)) => request,
                _ => return,
            },
        };
        let reply = if !request.is_messages() {
            if request.method == "GET" {
                Reply::Http {
                    status: 200,
                    headers: vec![],
                    body: r#"{"data":[]}"#.into(),
                }
            } else {
                Reply::error(404, "not_found_error", "mock DeepSeek: unknown path")
            }
        } else {
            let scripted = shared
                .script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front();
            let responder = shared
                .responder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match (scripted, responder) {
                (Some(reply), _) => reply,
                (None, Some(responder)) => responder(&request),
                (None, None) => Reply::error(500, "api_error", "mock script exhausted"),
            }
        };
        let model = request
            .body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("deepseek-flash")
            .to_string();
        shared
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request);
        shared.arrived.notify_waiters();
        let keep_alive = tokio::select! {
            _ = stop.changed() => return,
            keep = write_reply(&mut write, &mut reader, reply, &model) => keep,
        };
        if !keep_alive.unwrap_or(false) {
            return;
        }
    }
}

async fn read_request(
    reader: &mut BufReader<OwnedReadHalf>,
) -> std::io::Result<Option<RecordedRequest>> {
    let mut line = String::new();
    // Skip stray blank lines between pipelined requests.
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = vec![];
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let mut raw_body = vec![];
    if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            line.clear();
            reader.read_line(&mut line).await?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if size == 0 {
                // Trailer section ends with an empty line.
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await? == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let start = raw_body.len();
            raw_body.resize(start + size, 0);
            reader.read_exact(&mut raw_body[start..]).await?;
            line.clear();
            reader.read_line(&mut line).await?;
        }
    } else if let Some(length) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        raw_body.resize(length, 0);
        reader.read_exact(&mut raw_body).await?;
    }
    let body = serde_json::from_slice(&raw_body).unwrap_or(Value::Null);
    Ok(Some(RecordedRequest {
        method,
        path,
        headers,
        raw_body,
        body,
    }))
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Site Overloaded",
        _ => "Status",
    }
}

async fn write_chunk(write: &mut OwnedWriteHalf, data: &[u8]) -> std::io::Result<()> {
    let mut frame = format!("{:x}\r\n", data.len()).into_bytes();
    frame.extend_from_slice(data);
    frame.extend_from_slice(b"\r\n");
    write.write_all(&frame).await?;
    write.flush().await
}

/// Writes one reply; `Ok(true)` keeps the connection for the next request.
async fn write_reply(
    write: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    reply: Reply,
    model: &str,
) -> std::io::Result<bool> {
    let stream = match reply {
        Reply::Http {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
                reason_phrase(status),
                body.len()
            );
            for (name, value) in headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            write.write_all(head.as_bytes()).await?;
            write.write_all(body.as_bytes()).await?;
            write.flush().await?;
            return Ok(true);
        }
        Reply::RawSse(text) => StreamReply::default().raw_text(text),
        Reply::Stream(stream) => stream,
    };
    write
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n\r\n")
        .await?;
    write.flush().await?;
    let frames: Vec<String> = match &stream.raw {
        Some(raw) => vec![raw.clone()],
        None => stream
            .build_events(model)
            .iter()
            .map(|e| sse_text(std::slice::from_ref(e)))
            .collect(),
    };
    for (index, frame) in frames.iter().enumerate() {
        match stream.cut {
            Some(Cut::Disconnect(n)) if index == n => return Ok(false),
            Some(Cut::End(n)) if index == n => break,
            Some(Cut::Stall(n)) if index == n => {
                // Hold the response open until the client hangs up.
                let mut buf = [0u8; 256];
                while reader.read(&mut buf).await? > 0 {}
                return Ok(false);
            }
            _ => {}
        }
        if !stream.delay.is_zero() {
            tokio::time::sleep(stream.delay).await;
        }
        match stream.split_bytes {
            Some(size) => {
                for piece in frame.as_bytes().chunks(size) {
                    write_chunk(write, piece).await?;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            None => write_chunk(write, frame.as_bytes()).await?,
        }
    }
    write.write_all(b"0\r\n\r\n").await?;
    write.flush().await?;
    Ok(true)
}

impl StreamReply {
    /// Sends `text` verbatim as the SSE body (`split_bytes`/`delay` still apply).
    pub fn raw_text(mut self, text: impl Into<String>) -> Self {
        self.raw = Some(text.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_events_follow_the_messages_protocol() {
        let events = Reply::stream()
            .thinking("abcd", "sig")
            .text("hi")
            .tool_call("t1", "bash", json!({"command": "ls"}))
            .cache(80, 5)
            .build_events("deepseek-flash");
        let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(kinds.first(), Some(&"message_start"));
        assert_eq!(kinds.last(), Some(&"message_stop"));
        assert_eq!(events[0]["message"]["usage"]["cache_read_input_tokens"], 80);
        let stop = events
            .iter()
            .find(|e| e["type"] == "message_delta")
            .unwrap();
        assert_eq!(stop["delta"]["stop_reason"], "tool_use");
        let json: String = events
            .iter()
            .filter(|e| e["delta"]["type"] == "input_json_delta")
            .map(|e| e["delta"]["partial_json"].as_str().unwrap())
            .collect();
        assert_eq!(json, r#"{"command":"ls"}"#);
    }
}
