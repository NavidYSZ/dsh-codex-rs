//! Routes of the Models settings page: model providers, their keys and
//! their published model lists (docs/rust/SPEC.md §6).
//!
//! A [`Backend`](crate::Backend) offers them through
//! [`Backend::providers`](crate::Backend::providers); without it every route
//! answers 404. Keys go in, never out: responses only say whether a key is
//! configured and where it comes from.
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiResult, AppState, NotFound};

fn unsupported() -> anyhow::Error {
    NotFound("model providers are not available".into()).into()
}

/// Everything the Models page can ask for. Values are JSON as documented
/// in docs/rust/SPEC.md §6.
#[async_trait]
pub trait Providers: Send + Sync + 'static {
    /// `{providers: [Provider], catalog: [CatalogEntry]}`.
    async fn list(&self) -> anyhow::Result<Value>;
    /// Creates or updates a provider from `{displayName?, api?, baseUrl?,
    /// apiKeyEnv?, titleModel?, headers?, models?, compat?, apiKey?}`: the
    /// fields sent replace (`null` clears one), the others stay →
    /// `Provider`. `deepseek-official` takes only `baseUrl` and `apiKey`.
    async fn save(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        let _ = (id, body);
        Err(unsupported())
    }
    /// Removes the provider and its stored key.
    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let _ = id;
        Err(unsupported())
    }
    /// Stores (or with `None` removes) the provider's key → `Provider`.
    async fn set_key(&self, id: &str, key: Option<String>) -> anyhow::Result<Value> {
        let _ = (id, key);
        Err(unsupported())
    }
    /// Asks the provider for its models; `{api?, baseUrl?, apiKey?}`
    /// overrides unsaved settings → `{models: [RemoteModel]}`.
    async fn fetch_models(&self, id: &str, probe: Value) -> anyhow::Result<Value> {
        let _ = (id, probe);
        Err(unsupported())
    }
}

/// Routes under `/api` (behind the token check).
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/providers", get(list))
        .route("/providers/{id}", put(save).delete(remove))
        .route("/providers/{id}/key", put(set_key))
        .route("/providers/{id}/models/fetch", post(fetch_models))
}

fn providers(s: &AppState) -> Result<Arc<dyn Providers>, anyhow::Error> {
    s.backend.providers().ok_or_else(unsupported)
}

async fn list(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(providers(&s)?.list().await?))
}

async fn save(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !body.is_object() {
        return Err(anyhow::anyhow!("expected a JSON object").into());
    }
    Ok(Json(providers(&s)?.save(&id, body).await?))
}

async fn remove(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    providers(&s)?.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct KeyBody {
    key: Option<String>,
}

async fn set_key(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<KeyBody>,
) -> ApiResult<Json<Value>> {
    let key = body.key.filter(|k| !k.trim().is_empty());
    Ok(Json(providers(&s)?.set_key(&id, key).await?))
}

async fn fetch_models(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let probe = body.map(|Json(v)| v).unwrap_or(Value::Null);
    Ok(Json(providers(&s)?.fetch_models(&id, probe).await?))
}
