//! Streamable HTTP transport (MCP 2025-03-26 and later): every message is a
//! POST; the reply is JSON or an SSE stream that may also carry server
//! requests and notifications. `Mcp-Session-Id` from `initialize` rides on
//! every later request, `MCP-Protocol-Version` after the handshake; a 404 for
//! a known session ends the connection so the supervisor re-initializes.
//!
//! With a [`TokenProvider`] every request carries its bearer token; a 401
//! gets one retry with a refreshed token, then fails as
//! [`McpError::Unauthorized`] (and ends a live connection, so the
//! supervisor reports that a login is needed). A 403 `insufficient_scope`
//! is recorded for step-up authorization.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::McpError;
use crate::oauth::{AuthChallenge, TokenProvider, parse_www_authenticate};
use crate::peer::{Incoming, Transport};

const SESSION_HEADER: &str = "mcp-session-id";
const VERSION_HEADER: &str = "mcp-protocol-version";

pub(crate) struct HttpTransport {
    client: reqwest::Client,
    url: url::Url,
    headers: HeaderMap,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    inbox: mpsc::UnboundedSender<Incoming>,
    /// Ends the SSE readers when the connection closes.
    stop: CancellationToken,
    label: String,
    auth: Option<Arc<dyn TokenProvider>>,
    /// The last authorization challenge (shared with the supervisor).
    challenge: Arc<Mutex<Option<AuthChallenge>>>,
}

fn challenge_of(response: &reqwest::Response) -> AuthChallenge {
    parse_www_authenticate(
        response.status().as_u16(),
        response
            .headers()
            .get_all(reqwest::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok()),
    )
}

impl HttpTransport {
    pub fn new(
        url: url::Url,
        headers: &BTreeMap<String, String>,
        label: &str,
        auth: Option<Arc<dyn TokenProvider>>,
        challenge: Arc<Mutex<Option<AuthChallenge>>>,
    ) -> Result<(Arc<HttpTransport>, mpsc::UnboundedReceiver<Incoming>), McpError> {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|_| McpError::Transport(format!("invalid header name {k:?}")))?;
            let value = HeaderValue::from_str(v)
                .map_err(|_| McpError::Transport(format!("invalid value for header {k:?}")))?;
            map.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let (inbox, rx) = mpsc::unbounded_channel();
        Ok((
            Arc::new(HttpTransport {
                client,
                url,
                headers: map,
                session: Mutex::new(None),
                version: Mutex::new(None),
                inbox,
                stop: CancellationToken::new(),
                label: label.to_owned(),
                auth,
                challenge,
            }),
            rx,
        ))
    }

    async fn token(&self) -> Option<String> {
        match &self.auth {
            Some(auth) => auth.token().await,
            None => None,
        }
    }

    fn request(&self, method: reqwest::Method, token: Option<&str>) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, self.url.clone())
            .headers(self.headers.clone());
        if let Some(token) = token {
            builder = builder.bearer_auth(token);
        }
        if let Some(session) = self.session.lock().unwrap().clone() {
            builder = builder.header(SESSION_HEADER, session);
        }
        if let Some(version) = self.version.lock().unwrap().clone() {
            builder = builder.header(VERSION_HEADER, version);
        }
        builder
    }

    /// Forwards the events of an SSE body to the inbox until it ends.
    fn pump_sse(&self, response: reqwest::Response) {
        let inbox = self.inbox.clone();
        let label = self.label.clone();
        let stop = self.stop.clone();
        tokio::spawn(async move {
            let mut parser = SseParser::default();
            let mut body = response.bytes_stream();
            loop {
                let chunk = tokio::select! {
                    _ = stop.cancelled() => return,
                    chunk = body.next() => chunk,
                };
                let Some(chunk) = chunk else { return };
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        tracing::debug!("{label}: SSE stream failed: {e}");
                        return;
                    }
                };
                for data in parser.push(&chunk) {
                    match serde_json::from_str::<Value>(&data) {
                        Ok(message) => {
                            let _ = inbox.send(Incoming::Message(message));
                        }
                        Err(e) => tracing::debug!("{label}: ignoring non-JSON SSE event: {e}"),
                    }
                }
            }
        });
    }

    /// Opens the optional GET stream for server-initiated messages.
    fn listen(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let token = this.token().await;
            let response = this
                .request(reqwest::Method::GET, token.as_deref())
                .header(ACCEPT, "text/event-stream")
                .send()
                .await;
            match response {
                Ok(r) if r.status().is_success() && is_sse(&r) => this.pump_sse(r),
                Ok(_) | Err(_) => {}
            }
        });
    }
}

fn is_sse(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, message: Value) -> Result<(), McpError> {
        let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
        let had_session = self.session.lock().unwrap().is_some();
        let body = serde_json::to_vec(&message).map_err(|e| McpError::Protocol(e.to_string()))?;
        let token = self.token().await;
        let mut response = self.post(&body, token.as_deref()).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let fresh = match (&self.auth, &token) {
                (Some(auth), Some(rejected)) => auth.refresh(rejected).await,
                _ => None,
            };
            if let Some(fresh) = fresh {
                response = self.post(&body, Some(&fresh)).await?;
            }
        }
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(self.unauthorized(challenge_of(&response)));
        }
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            let challenge = challenge_of(&response);
            if challenge.insufficient_scope() {
                *self.challenge.lock().unwrap() = Some(challenge.clone());
                return Err(McpError::Unauthorized(Box::new(challenge)));
            }
        }
        if is_initialize
            && let Some(session) = response
                .headers()
                .get(SESSION_HEADER)
                .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap() = Some(session.to_owned());
        }
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND && had_session {
            let _ = self
                .inbox
                .send(Incoming::Closed("MCP session expired".into()));
            return Err(McpError::Closed);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body = body.trim();
            return Err(McpError::Transport(if body.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {body}")
            }));
        }
        if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(());
        }
        if is_sse(&response) {
            self.pump_sse(response);
            return Ok(());
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP response failed: {e}")))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|e| McpError::Protocol(format!("invalid JSON response: {e}")))?;
        let _ = self.inbox.send(Incoming::Message(reply));
        Ok(())
    }

    fn initialized(&self, protocol_version: &str) {
        *self.version.lock().unwrap() = Some(protocol_version.to_owned());
    }

    async fn close(&self) -> bool {
        self.stop.cancel();
        if self.session.lock().unwrap().is_some() {
            let token = self.token().await;
            let release = self
                .request(reqwest::Method::DELETE, token.as_deref())
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;
            if let Err(e) = release {
                tracing::debug!("{}: session release failed: {e}", self.label);
            }
            *self.session.lock().unwrap() = None;
        }
        let _ = self
            .inbox
            .send(Incoming::Closed("connection closed by the client".into()));
        true
    }
}

impl HttpTransport {
    async fn post(&self, body: &[u8], token: Option<&str>) -> Result<reqwest::Response, McpError> {
        self.request(reqwest::Method::POST, token)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP request failed: {e}")))
    }

    /// Records a 401; a live connection ends so the supervisor notices.
    fn unauthorized(&self, challenge: AuthChallenge) -> McpError {
        *self.challenge.lock().unwrap() = Some(challenge.clone());
        if self.version.lock().unwrap().is_some() {
            let _ = self
                .inbox
                .send(Incoming::Closed("authorization required".into()));
        }
        McpError::Unauthorized(Box::new(challenge))
    }

    /// Starts the GET listener once the session is initialized.
    pub fn start_listening(self: &Arc<Self>) {
        self.listen();
    }
}

/// Incremental `text/event-stream` parser yielding the `data` of each event
/// of type `message` (or untyped).
#[derive(Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                let data = std::mem::take(&mut self.data);
                let event = self.event.take();
                if !data.is_empty() && matches!(event.as_deref(), None | Some("message")) {
                    out.push(data.join("\n"));
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f.to_owned(), v.strip_prefix(' ').unwrap_or(v).to_owned()),
                None => (line, String::new()),
            };
            match field.as_str() {
                "data" => self.data.push(value),
                "event" => self.event = Some(value),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events() {
        let mut p = SseParser::default();
        assert!(
            p.push(b": keepalive\n\nevent: message\ndata: {\"a\"")
                .is_empty()
        );
        assert_eq!(p.push(b":1}\n\n"), vec!["{\"a\":1}"]);
        assert_eq!(p.push(b"data: x\r\ndata: y\r\n\r\n"), vec!["x\ny"]);
        assert!(p.push(b"event: ping\ndata: z\n\n").is_empty());
    }
}
