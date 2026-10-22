//! Local HTTP/WebSocket server behind the web UI (protocol: docs/rust/SPEC.md §6).
//!
//! The server only translates HTTP to calls on a [`Backend`]; the binary wires the
//! backend to the agent, the session store and the settings.

mod preview;

use std::net::SocketAddr;
use std::path::PathBuf;
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

mod composer;
mod extensions;
mod folders;
mod providers;

pub use composer::{Composer, SuggestQuery};
pub use extensions::{Extensions, OAuthCallback};
pub use providers::Providers;

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
    /// Live view of browser/computer work (frames, steps); never persisted
    /// and sent through [`Backend::live`], where a slow client just drops frames.
    Live {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(flatten)]
        data: Value,
    },
    /// The current goal of a session changed (phase, revision, rounds or
    /// activation); `goal` is `null` once cleared.
    Goal {
        #[serde(rename = "sessionId")]
        session_id: String,
        goal: Option<Value>,
    },
    /// Some scheduled task was created, changed, delivered or deleted
    /// (dsh `schedule/changed`); reload `/api/schedules`.
    ScheduleChanged,
}

impl StreamMessage {
    fn session_id(&self) -> &str {
        match self {
            StreamMessage::Event { session_id, .. }
            | StreamMessage::Delta { session_id, .. }
            | StreamMessage::Status { session_id, .. }
            | StreamMessage::Approval { session_id, .. }
            | StreamMessage::Live { session_id, .. }
            | StreamMessage::Goal { session_id, .. } => session_id,
            StreamMessage::ScheduleChanged => "",
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
    /// While a turn runs: hand the text to its next step instead of queueing
    /// a turn of its own (dsh `steer`).
    #[serde(default)]
    pub steer: bool,
    /// The browser's IANA time zone; the time context tells the model (dsh
    /// `clientTimeZone`).
    #[serde(default, rename = "timeZone")]
    pub time_zone: Option<String>,
}

/// `GET /api/usage` query: `since`/`until` in ms (default: the last 30 days),
/// `tz` = the reader's UTC offset in minutes (days are local days), `top` =
/// how many chats `topChats` lists (default 5).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageQuery {
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub tz: Option<i64>,
    pub top: Option<usize>,
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
    /// [`Backend::create_session`] with an agent preset (`standard`,
    /// `minimal`); `None` = the configured default. Without preset support
    /// only `standard` is accepted.
    async fn create_session_with_preset(
        &self,
        cwd: String,
        preset: Option<String>,
    ) -> anyhow::Result<String> {
        if preset.as_deref().is_some_and(|p| p != "standard") {
            anyhow::bail!("agent presets are not supported");
        }
        self.create_session(cwd).await
    }
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
    /// Branches a chat after `{turn}` (0 = before the first turn) into a new
    /// session in the same folder and mode, named `{title?}`; → `{id}`.
    async fn fork_session(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (id, body);
        Err(NotFound("fork".into()).into())
    }
    /// Feed of [`StreamMessage::Live`] messages, separate from [`Backend::stream`]
    /// so bulky frames cannot push session events out of the buffer.
    fn live(&self) -> Option<broadcast::Receiver<StreamMessage>> {
        None
    }
    /// The last live message of a session, for a viewer that just connected.
    fn live_latest(&self, id: &str) -> Option<StreamMessage> {
        let _ = id;
        None
    }
    /// The chat's working folder; the side panel previews files inside it.
    async fn session_root(&self, id: &str) -> anyhow::Result<PathBuf> {
        let _ = id;
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
    /// Plugins, marketplaces, skills, MCP servers and connector logins;
    /// `None` = those routes answer 404.
    fn extensions(&self) -> Option<Arc<dyn Extensions>> {
        None
    }
    /// Model providers and their keys (Models settings page); `None` = those
    /// routes answer 404.
    fn providers(&self) -> Option<Arc<dyn Providers>> {
        None
    }
    /// The composer's suggestions, chat MCP servers, compaction, export,
    /// queue and file uploads; `None` = those routes answer 404.
    fn composer(&self) -> Option<Arc<dyn Composer>> {
        None
    }

    // Automation (goals, schedules, Agent Teams); unsupported by default.

    /// `{goal: GoalView | null}` of a session.
    async fn goal(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(NotFound("goals".into()).into())
    }
    /// `{action: "create"|"edit"|"pause"|"resume"|"clear", objective?}` → `{goal}`.
    async fn goal_command(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (id, body);
        Err(NotFound("goals".into()).into())
    }
    /// Every scheduled task with its session binding (dsh `catalog`).
    async fn schedules(&self) -> anyhow::Result<Value> {
        Err(NotFound("schedules".into()).into())
    }
    /// The active reminders of one session.
    async fn session_schedules(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(NotFound("schedules".into()).into())
    }
    /// `{title?, prompt?, at?|every_seconds?|daily?|weekly?|cron?}` → the updated reminder.
    async fn update_schedule(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (id, body);
        Err(NotFound("schedules".into()).into())
    }
    /// → `{id, deleted}`.
    async fn delete_schedule(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(NotFound("schedules".into()).into())
    }
    /// Newest-first delivery records (`limit` 1-100, default 20; `before` = message id cursor).
    async fn schedule_history(
        &self,
        id: &str,
        limit: Option<i64>,
        before: Option<String>,
    ) -> anyhow::Result<Value> {
        let _ = (id, limit, before);
        Err(NotFound("schedules".into()).into())
    }
    /// `{members, tasks, roster, failure?}` of the Team a Lead session leads.
    async fn team(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(NotFound("team".into()).into())
    }

    /// Tokens, costs (USD) and timings of a chat; `from_turn` limits the
    /// per-turn list. Unsupported by default.
    async fn session_stats(&self, id: &str, from_turn: Option<u64>) -> anyhow::Result<Value> {
        let _ = (id, from_turn);
        Err(NotFound("stats".into()).into())
    }
    /// Tokens and costs of all chats per day and model, plus the top chats.
    async fn usage(&self, query: UsageQuery) -> anyhow::Result<Value> {
        let _ = query;
        Err(NotFound("usage".into()).into())
    }
    /// Folders the project folder browser (`/api/fs/…`) may list; nested and
    /// missing ones are dropped by the server. Default: none (404).
    async fn folder_roots(&self) -> anyhow::Result<Vec<PathBuf>> {
        Err(NotFound("folders".into()).into())
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
    previews: Arc<preview::Grants>,
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
        previews: Arc::default(),
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
        .route("/sessions/{id}/fork", post(fork_session))
        .route("/sessions/{id}/preview", post(preview_grant))
        .route("/sessions/{id}/files", get(list_files))
        .route("/approvals/{call_id}", post(approve))
        .route(
            "/attachments",
            post(save_attachment).layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD + 1024)),
        )
        .route("/attachments/{id}", get(load_attachment))
        // Automation: goals, schedules, Agent Teams.
        .route("/sessions/{id}/goal", get(goal).post(goal_command))
        .route("/sessions/{id}/schedules", get(session_schedules))
        .route("/sessions/{id}/team", get(team))
        .route("/sessions/{id}/stats", get(session_stats))
        .route("/usage", get(usage))
        .route("/schedules", get(schedules))
        .route(
            "/schedules/{id}",
            put(update_schedule).delete(delete_schedule),
        )
        .route("/schedules/{id}/history", get(schedule_history))
        .route("/stream", get(stream))
        .merge(extensions::routes())
        .merge(folders::routes())
        .merge(providers::routes())
        .merge(composer::routes())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));
    Router::new()
        .nest("/api", api)
        .route("/preview/{grant}/", get(preview_index))
        .route("/preview/{grant}/{*path}", get(preview_file))
        .route("/api/oauth/callback", get(extensions::oauth_callback))
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
    let host_ok = host_is_loopback(&headers);
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

/// Only localhost may talk to us (DNS-rebinding guard).
fn host_is_loopback(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            let name = h.rsplit_once(':').map_or(h, |(n, _)| n);
            matches!(name, "127.0.0.1" | "localhost" | "[::1]")
        })
        .unwrap_or(false)
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
    // Debug builds read ui/ from disk: never let the browser keep a stale copy.
    let cache = if cfg!(debug_assertions) {
        "no-cache"
    } else {
        "public, max-age=300"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_owned()),
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
    let info = s.backend.info().await?;
    Ok(Json(extensions::with_summary(&s, info).await))
}

async fn update_settings(
    State(s): State<AppState>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    let info = s.backend.update_settings(patch).await?;
    Ok(Json(extensions::with_summary(&s, info).await))
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
    #[serde(default)]
    preset: Option<String>,
}

async fn create_session(
    State(s): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let id = s
        .backend
        .create_session_with_preset(body.cwd.unwrap_or_default(), body.preset)
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

async fn fork_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.fork_session(&id, body).await?))
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

async fn preview_grant(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let root = s.backend.session_root(&id).await?.canonicalize()?;
    let grant = s.previews.grant(&root);
    Ok(Json(
        json!({ "base": format!("/preview/{grant}/"), "root": root.display().to_string() }),
    ))
}

#[derive(Deserialize)]
struct DirQuery {
    #[serde(default)]
    dir: String,
}

async fn list_files(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DirQuery>,
) -> ApiResult<Json<Value>> {
    let root = s.backend.session_root(&id).await?;
    let dir = preview::resolve_inside(&root, &q.dir).ok_or_else(|| NotFound(q.dir.clone()))?;
    let (entries, truncated) =
        tokio::task::spawn_blocking(move || preview::list(&root, &dir)).await??;
    Ok(Json(
        json!({ "dir": q.dir.trim_matches('/'), "entries": entries, "truncated": truncated }),
    ))
}

async fn preview_index(
    State(s): State<AppState>,
    Path(grant): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve_preview(&s, &grant, "", &headers).await
}

async fn preview_file(
    State(s): State<AppState>,
    Path((grant, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    serve_preview(&s, &grant, &path, &headers).await
}

/// One file of a granted folder; folders serve their `index.html`.
async fn serve_preview(s: &AppState, grant: &str, rel: &str, headers: &HeaderMap) -> Response {
    let not_found = || StatusCode::NOT_FOUND.into_response();
    if !host_is_loopback(headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(root) = s.previews.root(grant) else {
        return not_found();
    };
    let Some(mut path) = preview::resolve_inside(&root, rel) else {
        return not_found();
    };
    if path.is_dir() {
        path.push("index.html");
        if !path.is_file() {
            return not_found();
        }
    }
    match tokio::fs::metadata(&path).await {
        Ok(m) if m.len() <= preview::MAX_FILE => {}
        Ok(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return not_found(),
    }
    let Ok(data) = tokio::fs::read(&path).await else {
        return not_found();
    };
    let (mime, active) = preview::content_type(&path);
    let mut response = (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::REFERRER_POLICY, "no-referrer".to_owned()),
        ],
        data,
    )
        .into_response();
    if active {
        // Scripts may run, but in an opaque origin: no access to this app.
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(
                "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads",
            ),
        );
    }
    response
}

async fn goal(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.goal(&id).await?))
}

async fn goal_command(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.goal_command(&id, body).await?))
}

async fn schedules(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.schedules().await?))
}

async fn session_schedules(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.session_schedules(&id).await?))
}

async fn update_schedule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.update_schedule(&id, body).await?))
}

async fn delete_schedule(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.delete_schedule(&id).await?))
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
    before: Option<String>,
}

async fn schedule_history(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        s.backend.schedule_history(&id, q.limit, q.before).await?,
    ))
}

async fn team(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.team(&id).await?))
}

#[derive(Deserialize)]
struct StatsQuery {
    #[serde(rename = "fromTurn")]
    from_turn: Option<u64>,
}

async fn session_stats(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.session_stats(&id, q.from_turn).await?))
}

async fn usage(State(s): State<AppState>, Query(q): Query<UsageQuery>) -> ApiResult<Json<Value>> {
    Ok(Json(s.backend.usage(q).await?))
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
    let mut frames = session.as_ref().and_then(|_| backend.live());
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
        if let Some(latest) = backend.live_latest(id)
            && send(&mut socket, &latest).await.is_err()
        {
            return;
        }
    }
    loop {
        let next_frame = async {
            match frames.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            frame = next_frame => match frame {
                Ok(msg) => {
                    if session.as_deref() == Some(msg.session_id()) && send(&mut socket, &msg).await.is_err() {
                        return;
                    }
                }
                // Frames are disposable: skip what was missed.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => frames = None,
            },
            msg = live.recv() => match msg {
                Ok(msg) => {
                    let wanted = match &session {
                        _ if matches!(msg, StreamMessage::ScheduleChanged) => true,
                        Some(id) => msg.session_id() == id,
                        None => matches!(msg, StreamMessage::Status { .. } | StreamMessage::Approval { .. } | StreamMessage::Goal { .. }),
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
