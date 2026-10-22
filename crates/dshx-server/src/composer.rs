//! Routes behind the composer's `/` and `@` menus and its commands: the
//! suggestions, MCP servers a chat activated, manual compaction, the log
//! export, queued prompts, file uploads, and the skill and MCP settings the
//! menus depend on (docs/rust/SPEC.md §6).
//!
//! A [`Backend`](crate::Backend) offers them through
//! [`Backend::composer`](crate::Backend::composer); without it every route
//! answers 404.
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiResult, AppState, NotFound};

/// Largest file `POST /api/files` accepts.
pub const MAX_FILE: usize = 100 * 1024 * 1024;

fn unsupported() -> anyhow::Error {
    NotFound("composer routes are not available".into()).into()
}

/// `GET /api/composer/suggest` query.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SuggestQuery {
    /// `slash` (commands and skills) or `mention` (apps and files).
    pub kind: String,
    /// The text after `/` or `@`.
    #[serde(default)]
    pub q: String,
    /// Project folder for skills and files; default: the session's.
    pub cwd: Option<String>,
    /// The chat the composer belongs to (active servers, its folder).
    pub session: Option<String>,
    /// The composer's model (whether activating a server costs a cache miss).
    pub model: Option<String>,
    /// The mode of the chat the start page will create (`standard`,
    /// `minimal`); a session's own mode wins.
    pub preset: Option<String>,
}

/// Everything the composer can ask for. Values are JSON as documented in
/// docs/rust/SPEC.md §6.
#[async_trait]
pub trait Composer: Send + Sync + 'static {
    /// `{kind, query, sections: [{id, items}], cacheMiss}`.
    async fn suggest(&self, query: SuggestQuery) -> anyhow::Result<Value>;
    /// `{active: [server]}`: the MCP servers a chat offers.
    async fn session_mcp(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(unsupported())
    }
    /// Switches an MCP server on or off for a chat → `{active}`.
    async fn set_session_mcp(&self, id: &str, server: &str, on: bool) -> anyhow::Result<Value> {
        let _ = (id, server, on);
        Err(unsupported())
    }
    /// Compacts older history now → `{compacted, items?, tokens?}`.
    async fn compact(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(unsupported())
    }
    /// The session log as JSONL and a file name.
    async fn export(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let _ = id;
        Err(unsupported())
    }
    /// `{action: "edit"|"remove"|"steer", text?}` on one queued prompt.
    async fn update_queue(&self, id: &str, item: &str, body: Value) -> anyhow::Result<()> {
        let _ = (id, item, body);
        Err(unsupported())
    }
    /// Stores an uploaded file → `{path, name, bytes, sha256}`.
    async fn save_file(&self, name: &str, data: Vec<u8>) -> anyhow::Result<Value> {
        let _ = (name, data);
        Err(unsupported())
    }
    /// `{enabled?, model?}` → the skill row of `GET /api/skills`.
    async fn skill_settings(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (name, body);
        Err(unsupported())
    }
    /// `{always?, disabledTools?}` → the server row of `GET /api/mcp/servers`.
    async fn mcp_settings(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (name, body);
        Err(unsupported())
    }
}

/// Routes under `/api` (behind the token check).
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/composer/suggest", get(suggest))
        .route("/sessions/{id}/mcp", get(session_mcp))
        .route(
            "/sessions/{id}/mcp/{server}",
            post(activate).delete(deactivate),
        )
        .route("/sessions/{id}/compact", post(compact))
        .route("/sessions/{id}/export", get(export))
        .route("/sessions/{id}/queue/{item}", post(update_queue))
        .route(
            "/files",
            post(save_file).layer(DefaultBodyLimit::max(MAX_FILE + 1024)),
        )
        .route("/skills/{name}/settings", put(skill_settings))
        .route("/mcp/servers/{name}/settings", put(mcp_settings))
}

fn composer(s: &AppState) -> Result<Arc<dyn Composer>, anyhow::Error> {
    s.backend.composer().ok_or_else(unsupported)
}

fn object(body: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(body.is_object(), "expected a JSON object");
    Ok(())
}

async fn suggest(
    State(s): State<AppState>,
    Query(q): Query<SuggestQuery>,
) -> ApiResult<Json<Value>> {
    if !matches!(q.kind.as_str(), "slash" | "mention") {
        return Err(anyhow::anyhow!("kind must be \"slash\" or \"mention\"").into());
    }
    Ok(Json(composer(&s)?.suggest(q).await?))
}

async fn session_mcp(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(composer(&s)?.session_mcp(&id).await?))
}

async fn activate(
    State(s): State<AppState>,
    Path((id, server)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        composer(&s)?.set_session_mcp(&id, &server, true).await?,
    ))
}

async fn deactivate(
    State(s): State<AppState>,
    Path((id, server)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        composer(&s)?.set_session_mcp(&id, &server, false).await?,
    ))
}

async fn compact(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(composer(&s)?.compact(&id).await?))
}

async fn export(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let (data, name) = composer(&s)?.export(&id).await?;
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', ""));
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        data,
    )
        .into_response())
}

async fn update_queue(
    State(s): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<StatusCode> {
    object(&body)?;
    composer(&s)?.update_queue(&id, &item, body).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FileQuery {
    name: String,
}

async fn save_file(
    State(s): State<AppState>,
    Query(q): Query<FileQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    if body.len() > MAX_FILE {
        return Err(anyhow::anyhow!("file larger than {} MB", MAX_FILE / 1024 / 1024).into());
    }
    Ok(Json(composer(&s)?.save_file(&q.name, body.to_vec()).await?))
}

async fn skill_settings(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    object(&body)?;
    Ok(Json(composer(&s)?.skill_settings(&name, body).await?))
}

async fn mcp_settings(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    object(&body)?;
    Ok(Json(composer(&s)?.mcp_settings(&name, body).await?))
}
