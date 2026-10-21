//! Local HTTP/WebSocket server behind the web UI (protocol: docs/rust/SPEC.md §6).
//!
//! The server only translates HTTP to calls on a [`Backend`]; the binary wires the
//! backend to the agent, the session store and the settings.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use dshx_core::{ImageAttachmentRef, SessionEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Live messages pushed to UI clients over the WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StreamMessage {
    Event {
        #[serde(rename = "sessionId")]
        session_id: String,
        event: SessionEvent,
    },
    Delta {
        #[serde(rename = "sessionId")]
        session_id: String,
        turn: u64,
        step: u64,
        index: usize,
        #[serde(rename = "blockType")]
        block_type: String,
        text: String,
    },
    Status {
        #[serde(rename = "sessionId")]
        session_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Approval {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        tool: String,
        summary: String,
        justification: Option<String>,
    },
}

impl StreamMessage {
    fn session_id(&self) -> &str {
        match self {
            StreamMessage::Event { session_id, .. }
            | StreamMessage::Delta { session_id, .. }
            | StreamMessage::Status { session_id, .. }
            | StreamMessage::Approval { session_id, .. } => session_id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptRequest {
    pub text: String,
    #[serde(default)]
    pub images: Vec<ImageAttachmentRef>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub sandbox: Option<String>,
}

/// Everything the UI can ask for. Errors map to HTTP 400 unless they are [`NotFound`].
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    async fn info(&self) -> anyhow::Result<Value>;
    /// Applies `{model?, effort?, sandbox?, approval?, apiKey?}` and returns the new info.
    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value>;
    async fn workspaces(&self) -> anyhow::Result<Value>;
    async fn add_workspace(&self, path: String) -> anyhow::Result<Value>;
    async fn list_sessions(&self, cwd: Option<String>) -> anyhow::Result<Value>;
    /// Returns the new session id. An empty `cwd` starts a chat without a project folder.
    async fn create_session(&self, cwd: String) -> anyhow::Result<String>;
    /// `{header, events, status}`.
    async fn session(&self, id: &str) -> anyhow::Result<Value>;
    /// Events with `seq > after`, for WebSocket catch-up.
    async fn events_after(&self, id: &str, after: Option<u64>)
    -> anyhow::Result<Vec<SessionEvent>>;
    async fn prompt(&self, id: &str, request: PromptRequest) -> anyhow::Result<()>;
    async fn cancel(&self, id: &str) -> anyhow::Result<()>;
    async fn delete_session(&self, id: &str) -> anyhow::Result<()>;
    /// Applies `{title?, pinned?, archived?, seen?, unread?}` (UI state, not the
    /// session log) and returns the session's list row.
    async fn update_session(&self, id: &str, patch: Value) -> anyhow::Result<Value> {
        let _ = (id, patch);
        anyhow::bail!("not supported")
    }
    async fn approve(&self, call_id: &str, decision: &str) -> anyhow::Result<()>;
    async fn save_attachment(
        &self,
        data: Vec<u8>,
        media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef>;
    async fn load_attachment(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)>;
    /// One feed for all sessions; the server filters per client.
    fn stream(&self) -> broadcast::Receiver<StreamMessage>;
    /// Current status of a session (`idle`, `running`, ...), sent after the backlog.
    async fn status(&self, id: &str) -> Option<String> {
        let _ = id;
        None
    }
}

/// Error for unknown sessions, attachments or approvals (→ HTTP 404).
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::error::Error for NotFound {}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not found: {}", self.0)
    }
}

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/"]
#[exclude = "screenshots/*"]
#[exclude = "dev/*"]
#[exclude = "fixtures/*"]
struct UiAssets;

#[derive(Clone)]
struct AppState {
    backend: Arc<dyn Backend>,
    token: Arc<str>,
}

struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = if self.0.downcast_ref::<NotFound>().is_some() {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(json!({ "error": format!("{:#}", self.0) }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Builds the router. `token` guards every `/api` route.
pub fn router(backend: Arc<dyn Backend>, token: String) -> Router {
    let state = AppState {
        backend,
        token: token.into(),
    };
    let api = Router::new()
        .route("/info", get(info))
        .route("/settings", put(update_settings))
        .route("/workspaces", get(workspaces).post(add_workspace))
        .route("/sessions", get(list_sessions).post(create_session))
        .route(
            "/sessions/{id}",
            get(session).delete(delete_session).patch(update_session),
        )
        .route("/sessions/{id}/prompt", post(prompt))
        .route("/sessions/{id}/cancel", post(cancel))
        .route("/approvals/{call_id}", post(approve))
        .route("/attachments", post(save_attachment))
        .route("/attachments/{id}", get(load_attachment))
        .route("/stream", get(stream))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));
    Router::new()
        .nest("/api", api)
        .fallback(get(static_file))
        .with_state(state)
}

/// Binds 127.0.0.1:`port` (0 = any free port) and serves until the future is dropped.
pub async fn serve(
    backend: Arc<dyn Backend>,
    token: String,
    port: u16,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let app = router(backend, token);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("web server stopped: {e}");
        }
    });
    Ok((addr, handle))
}

async fn require_token(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // Only localhost may talk to us (DNS-rebinding guard).
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            let name = h.rsplit_once(':').map_or(h, |(n, _)| n);
            matches!(name, "127.0.0.1" | "localhost" | "[::1]")
        })
        .unwrap_or(false);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let query = uri
        .query()
        .unwrap_or("")
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="));
    let presented = bearer.or(query).unwrap_or("");
    if !host_ok || !constant_time_eq(presented.as_bytes(), state.token.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    next.run(request).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Unknown paths fall back to the app shell (client-side routing).
    let (file, path) = match UiAssets::get(path) {
        Some(f) => (f, path),
        None => match UiAssets::get("index.html") {
            Some(f) => (f, "index.html"),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let mime = file.metadata.mimetype().to_owned();
    if path == "index.html" {
        let csp = csp_for(&file.data);
        return (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-cache".to_owned()),
                (header::CONTENT_SECURITY_POLICY, csp),
            ],
            file.data,
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=300".to_owned()),
        ],
        file.data,
    )
        .into_response()
}

/// Strict CSP: only our own files, plus the inline import map by its hash.
fn csp_for(html: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let text = String::from_utf8_lossy(html);
    let hashes: String = text
        .split("<script type=\"importmap\">")
        .skip(1)
        .filter_map(|rest| rest.split_once("</script>").map(|(body, _)| body))
        .map(|body| {
            let digest = sha2::Sha256::digest(body.as_bytes());
            let b64 = base64::engine::general_purpose::STANDARD.encode(digest);
            format!(" 'sha256-{b64}'")
        })
        .collect();
    format!(
        "default-src 'self'; script-src 'self'{hashes}; style-src 'self' 'unsafe-inline'; \
         img-src 'self' blob: data:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
         frame-ancestors 'none'"
    )
}

async fn info(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.info().await?))
}

async fn update_settings(
    State(s): State<AppState>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.update_settings(patch).await?))
}

async fn workspaces(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.workspaces().await?))
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

async fn add_workspace(
    State(s): State<AppState>,
    Json(body): Json<PathBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.add_workspace(body.path).await?))
}

#[derive(Deserialize)]
struct CwdQuery {
    cwd: Option<String>,
}

async fn list_sessions(
    State(s): State<AppState>,
    Query(q): Query<CwdQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.list_sessions(q.cwd).await?))
}

#[derive(Deserialize)]
struct CreateBody {
    #[serde(default)]
    cwd: Option<String>,
}

async fn create_session(
    State(s): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let id = s
        .backend
        .create_session(body.cwd.unwrap_or_default())
        .await?;
    Ok(Json(json!({ "id": id })))
}

async fn update_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.update_session(&id, patch).await?))
}

async fn session(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.session(&id).await?))
}

async fn delete_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.backend.delete_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn prompt(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PromptRequest>,
) -> ApiResult<StatusCode> {
    s.backend.prompt(&id, body).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn cancel(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    s.backend.cancel(&id).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct DecisionBody {
    decision: String,
}

async fn approve(
    State(s): State<AppState>,
    Path(call_id): Path<String>,
    Json(body): Json<DecisionBody>,
) -> ApiResult<StatusCode> {
    if !matches!(body.decision.as_str(), "allowed-once" | "rejected") {
        return Err(anyhow::anyhow!("decision must be \"allowed-once\" or \"rejected\"").into());
    }
    s.backend.approve(&call_id, &body.decision).await?;
    Ok(StatusCode::NO_CONTENT)
}

const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const MAX_UPLOAD: usize = 20 * 1024 * 1024;

async fn save_attachment(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<ImageAttachmentRef>> {
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !IMAGE_TYPES.contains(&media_type.as_str()) {
        return Err(anyhow::anyhow!("unsupported image type {media_type:?}").into());
    }
    if body.len() > MAX_UPLOAD {
        return Err(anyhow::anyhow!("image larger than 20 MB").into());
    }
    Ok(Json(
        s.backend.save_attachment(body.to_vec(), media_type).await?,
    ))
}

async fn load_attachment(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let (data, media_type) = s.backend.load_attachment(&id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, media_type),
            (header::CACHE_CONTROL, "private, max-age=86400".to_owned()),
        ],
        data,
    )
        .into_response())
}

#[derive(Deserialize)]
struct StreamQuery {
    session: Option<String>,
    /// `-1` (the UI's "nothing yet") means from the start.
    after: Option<i64>,
}

async fn stream(
    State(s): State<AppState>,
    Query(q): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let after = q.after.and_then(|a| u64::try_from(a).ok());
    ws.on_upgrade(move |socket| pump(socket, s.backend, q.session, after))
}

/// Sends the backlog after `after`, then live messages; with no session, only
/// status/approval messages of all sessions (for the sidebar).
async fn pump(
    mut socket: WebSocket,
    backend: Arc<dyn Backend>,
    session: Option<String>,
    after: Option<u64>,
) {
    // Subscribe before reading the backlog so nothing falls between the two.
    let mut live = backend.stream();
    let mut last_seq = after;
    if let Some(id) = &session {
        match backend.events_after(id, after).await {
            Ok(events) => {
                for event in events {
                    last_seq = Some(event.seq);
                    let msg = StreamMessage::Event {
                        session_id: id.clone(),
                        event,
                    };
                    if send(&mut socket, &msg).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = socket
                    .send(WsMessage::Text(
                        json!({ "kind": "error", "error": e.to_string() })
                            .to_string()
                            .into(),
                    ))
                    .await;
                return;
            }
        }
    }
    if let Some(id) = &session
        && let Some(status) = backend.status(id).await
    {
        let msg = StreamMessage::Status {
            session_id: id.clone(),
            status,
            error: None,
        };
        if send(&mut socket, &msg).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            msg = live.recv() => match msg {
                Ok(msg) => {
                    let wanted = match &session {
                        Some(id) => msg.session_id() == id,
                        None => matches!(msg, StreamMessage::Status { .. } | StreamMessage::Approval { .. }),
                    };
                    if !wanted {
                        continue;
                    }
                    if let StreamMessage::Event { event, .. } = &msg {
                        if last_seq.is_some_and(|s| event.seq <= s) {
                            continue;
                        }
                        last_seq = Some(event.seq);
                    }
                    if send(&mut socket, &msg).await.is_err() {
                        return;
                    }
                }
                // A slow client missed messages: tell it to reload the session.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = socket.send(WsMessage::Text(json!({ "kind": "resync" }).to_string().into())).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn send(socket: &mut WebSocket, msg: &StreamMessage) -> Result<(), axum::Error> {
    socket
        .send(WsMessage::Text(
            serde_json::to_string(msg).unwrap_or_default().into(),
        ))
        .await
}
