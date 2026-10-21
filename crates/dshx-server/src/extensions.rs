//! Routes of the Plugins page: plugins, marketplaces, skills, MCP servers,
//! the connector catalog and connector logins (docs/rust/SPEC.md §6).
//!
//! A [`Backend`](crate::Backend) offers them through
//! [`Backend::extensions`](crate::Backend::extensions); without it every
//! route answers 404. The OAuth callback is the only route without the
//! bearer token: the authorization server redirects the user's browser to
//! it, and the pending login's `state` is what binds the request.
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ApiResult, AppState, NotFound, host_is_loopback};

/// Query parameters of the redirect back from an authorization server.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub iss: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

fn unsupported() -> anyhow::Error {
    NotFound("extensions are not available".into()).into()
}

/// Everything the Plugins page can ask for. Each method defaults to 404 so
/// a backend implements what it offers. Values are JSON as documented in
/// docs/rust/SPEC.md §6.
#[async_trait]
pub trait Extensions: Send + Sync + 'static {
    /// Counts for `/api/info` (`extensions`).
    async fn summary(&self) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
    async fn plugins(&self) -> anyhow::Result<Value> {
        Err(unsupported())
    }
    async fn plugin(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(unsupported())
    }
    async fn install_plugin(
        &self,
        marketplace: &str,
        name: &str,
        enable: bool,
    ) -> anyhow::Result<Value> {
        let _ = (marketplace, name, enable);
        Err(unsupported())
    }
    async fn update_plugin(&self, id: &str) -> anyhow::Result<Value> {
        let _ = id;
        Err(unsupported())
    }
    async fn remove_plugin(&self, id: &str) -> anyhow::Result<()> {
        let _ = id;
        Err(unsupported())
    }
    async fn set_plugin_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<Value> {
        let _ = (id, enabled);
        Err(unsupported())
    }
    /// A file of the plugin (logo, screenshots) and its media type.
    async fn plugin_asset(&self, id: &str, path: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let _ = (id, path);
        Err(unsupported())
    }
    async fn marketplaces(&self) -> anyhow::Result<Value> {
        Err(unsupported())
    }
    async fn add_marketplace(&self, source: &str, git_ref: Option<&str>) -> anyhow::Result<Value> {
        let _ = (source, git_ref);
        Err(unsupported())
    }
    async fn update_marketplace(&self, name: &str) -> anyhow::Result<Value> {
        let _ = name;
        Err(unsupported())
    }
    async fn remove_marketplace(&self, name: &str) -> anyhow::Result<()> {
        let _ = name;
        Err(unsupported())
    }
    async fn skills(&self, cwd: Option<String>) -> anyhow::Result<Value> {
        let _ = cwd;
        Err(unsupported())
    }
    async fn mcp_servers(&self) -> anyhow::Result<Value> {
        Err(unsupported())
    }
    /// `{name?, connector?, config?}` → the new server.
    async fn add_mcp_server(&self, body: Value) -> anyhow::Result<Value> {
        let _ = body;
        Err(unsupported())
    }
    async fn remove_mcp_server(&self, name: &str) -> anyhow::Result<()> {
        let _ = name;
        Err(unsupported())
    }
    async fn set_mcp_server_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<Value> {
        let _ = (name, enabled);
        Err(unsupported())
    }
    async fn restart_mcp_server(&self, name: &str) -> anyhow::Result<Value> {
        let _ = name;
        Err(unsupported())
    }
    async fn connectors(&self) -> anyhow::Result<Value> {
        Err(unsupported())
    }
    /// Starts a connector login → `{authorizationUrl, redirectUri}`.
    async fn start_login(&self, server: &str, redirect_uri: &str) -> anyhow::Result<Value> {
        let _ = (server, redirect_uri);
        Err(unsupported())
    }
    /// Finishes a login from the callback; returns the server name.
    async fn finish_login(&self, params: OAuthCallback) -> anyhow::Result<String> {
        let _ = params;
        Err(unsupported())
    }
    async fn logout(&self, server: &str) -> anyhow::Result<Value> {
        let _ = server;
        Err(unsupported())
    }
    /// Stores (or with `None` removes) a hand-entered access token.
    async fn set_token(&self, server: &str, token: Option<String>) -> anyhow::Result<Value> {
        let _ = (server, token);
        Err(unsupported())
    }
}

/// Routes under `/api` (behind the token check).
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/plugins", get(plugins).post(install_plugin))
        .route("/plugins/{id}", get(plugin).delete(remove_plugin))
        .route("/plugins/{id}/enable", post(enable_plugin))
        .route("/plugins/{id}/disable", post(disable_plugin))
        .route("/plugins/{id}/update", post(update_plugin))
        .route("/plugins/{id}/assets/{*path}", get(plugin_asset))
        .route("/marketplaces", get(marketplaces).post(add_marketplace))
        .route(
            "/marketplaces/{name}",
            axum::routing::delete(remove_marketplace),
        )
        .route("/marketplaces/{name}/update", post(update_marketplace))
        .route("/skills", get(skills))
        .route("/mcp/servers", get(mcp_servers).post(add_mcp_server))
        .route(
            "/mcp/servers/{name}",
            axum::routing::delete(remove_mcp_server),
        )
        .route("/mcp/servers/{name}/enable", post(enable_mcp_server))
        .route("/mcp/servers/{name}/disable", post(disable_mcp_server))
        .route("/mcp/servers/{name}/restart", post(restart_mcp_server))
        .route("/mcp/servers/{name}/login", post(start_login))
        .route("/mcp/servers/{name}/logout", post(logout))
        .route("/mcp/servers/{name}/token", put(set_token))
        .route("/connectors", get(connectors))
}

fn ext(s: &AppState) -> Result<Arc<dyn Extensions>, anyhow::Error> {
    s.backend.extensions().ok_or_else(unsupported)
}

/// Adds `extensions` (the backend's summary) to an `/api/info` value.
pub(crate) async fn with_summary(s: &AppState, mut info: Value) -> Value {
    if let Some(ext) = s.backend.extensions()
        && let Some(obj) = info.as_object_mut()
    {
        match ext.summary().await {
            Ok(summary) => {
                obj.insert("extensions".into(), summary);
            }
            Err(e) => tracing::warn!("extension summary failed: {e:#}"),
        }
    }
    info
}

async fn plugins(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.plugins().await?))
}

#[derive(Deserialize)]
struct InstallBody {
    marketplace: String,
    name: String,
    #[serde(default = "enabled_by_default")]
    enable: bool,
}

fn enabled_by_default() -> bool {
    true
}

async fn install_plugin(
    State(s): State<AppState>,
    Json(body): Json<InstallBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        ext(&s)?
            .install_plugin(&body.marketplace, &body.name, body.enable)
            .await?,
    ))
}

async fn plugin(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.plugin(&id).await?))
}

async fn remove_plugin(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    ext(&s)?.remove_plugin(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.set_plugin_enabled(&id, true).await?))
}

async fn disable_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.set_plugin_enabled(&id, false).await?))
}

async fn update_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.update_plugin(&id).await?))
}

async fn plugin_asset(
    State(s): State<AppState>,
    Path((id, path)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (data, media_type) = ext(&s)?.plugin_asset(&id, &path).await?;
    Ok((
        [
            (header::CONTENT_TYPE, media_type),
            (header::CACHE_CONTROL, "private, max-age=300".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            // SVGs must not run scripts when opened directly.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'".to_owned(),
            ),
        ],
        data,
    )
        .into_response())
}

async fn marketplaces(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.marketplaces().await?))
}

#[derive(Deserialize)]
struct MarketplaceBody {
    source: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

async fn add_marketplace(
    State(s): State<AppState>,
    Json(body): Json<MarketplaceBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        ext(&s)?
            .add_marketplace(&body.source, body.git_ref.as_deref())
            .await?,
    ))
}

async fn update_marketplace(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.update_marketplace(&name).await?))
}

async fn remove_marketplace(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    ext(&s)?.remove_marketplace(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CwdQuery {
    cwd: Option<String>,
}

async fn skills(State(s): State<AppState>, Query(q): Query<CwdQuery>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.skills(q.cwd).await?))
}

async fn mcp_servers(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.mcp_servers().await?))
}

async fn add_mcp_server(
    State(s): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.add_mcp_server(body).await?))
}

async fn remove_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    ext(&s)?.remove_mcp_server(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.set_mcp_server_enabled(&name, true).await?))
}

async fn disable_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.set_mcp_server_enabled(&name, false).await?))
}

async fn restart_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.restart_mcp_server(&name).await?))
}

/// The callback URL on the loopback port the browser used for the UI, so it
/// also works through an SSH tunnel that forwards that port.
fn redirect_uri(headers: &HeaderMap) -> anyhow::Result<String> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let port = host
        .rsplit_once(':')
        .map(|(_, p)| p)
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| {
            anyhow::anyhow!("cannot tell the UI's port from the Host header {host:?}")
        })?;
    Ok(format!("http://127.0.0.1:{port}/api/oauth/callback"))
}

async fn start_login(
    State(s): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let redirect = redirect_uri(&headers)?;
    Ok(Json(ext(&s)?.start_login(&name, &redirect).await?))
}

async fn logout(State(s): State<AppState>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.logout(&name).await?))
}

#[derive(Deserialize)]
struct TokenBody {
    token: Option<String>,
}

async fn set_token(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<TokenBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.set_token(&name, body.token).await?))
}

async fn connectors(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(ext(&s)?.connectors().await?))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn callback_page(status: StatusCode, title: &str, detail: &str) -> Response {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <title>{title}</title><style>body{{font:15px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1rem;color:#222}}\
         @media(prefers-color-scheme:dark){{body{{background:#111;color:#ddd}}}}h1{{font-size:1.25rem}}</style></head>\
         <body><h1>{title}</h1><p>{detail}</p></body></html>",
        title = escape(title),
        detail = escape(detail),
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'",
            ),
        ],
        body,
    )
        .into_response()
}

/// `GET /api/oauth/callback`: no bearer token (the browser arrives from the
/// authorization server), loopback Host only, bound to a pending `state`.
pub(crate) async fn oauth_callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<OAuthCallback>,
) -> Response {
    if !host_is_loopback(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    let Some(ext) = s.backend.extensions() else {
        return callback_page(
            StatusCode::NOT_FOUND,
            "Not available",
            "This server offers no logins.",
        );
    };
    match ext.finish_login(params).await {
        Ok(server) => callback_page(
            StatusCode::OK,
            &format!("Connected: {server}"),
            "You can close this window and return to dsh-codex. / Du kannst dieses Fenster schließen.",
        ),
        Err(e) => callback_page(StatusCode::BAD_REQUEST, "Login failed", &format!("{e:#}")),
    }
}
