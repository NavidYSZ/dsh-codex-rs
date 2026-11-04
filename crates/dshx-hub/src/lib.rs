//! Aether accounts hub (contract: docs/aether/SPEC.md §5).
//!
//! Sign-up and sign-in, a tier per user, the user's servers and a relay
//! through which the Aether app reaches a server that is not on its network.
//! The hub is a library ([`Hub`], [`router`], [`serve`]) with a thin binary,
//! `aether-hub`; [`relay_client`] is the piece that runs inside `dsh-codex web`
//! and [`enroll`] registers a server with a code from the app.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! let hub = dshx_hub::Hub::open("/tmp/aether-hub")?;
//! let (addr, _task) = dshx_hub::serve(hub, "127.0.0.1:8787".parse()?).await?;
//! println!("hub at http://{addr}");
//! # Ok(())
//! # }
//! ```

pub mod auth;
pub mod db;
mod error;
pub mod relay;
pub mod relay_client;
pub mod servers;
pub mod tiers;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use serde_json::{Value, json};

pub use auth::User;
pub use error::{ApiError, ApiResult};
pub use relay_client::{RelayConfig, RelayStatus, ServerInfo, enroll};
pub use servers::Server;
pub use tiers::Tier;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why a hub could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("data directory {0}: {1}")]
    DataDir(PathBuf, std::io::Error),
    #[error("database {0}: {1}")]
    Database(PathBuf, rusqlite::Error),
    #[error("password hashing: {0}")]
    Hashing(String),
}

/// Settings of a hub beyond its data directory.
#[derive(Debug, Clone, Default)]
pub struct HubOptions {
    /// How the hub is reached from outside (reported by `/v1/health`).
    pub public_url: Option<String>,
    /// Bearer for `/v1/admin/*`; `None` switches those routes off.
    pub admin_token: Option<String>,
    /// Development mode: verification codes are logged and returned by signup.
    pub dev: bool,
}

struct Inner {
    db: db::Db,
    registry: relay::Registry,
    options: HubOptions,
    logins: auth::RateLimiter,
    enrolments: auth::RateLimiter,
    /// Verified against for unknown emails so a login takes the same time
    /// whether or not the account exists.
    decoy_hash: String,
}

/// One hub: the database, the relay registry and the options. Cheap to
/// clone; every clone is the same hub.
#[derive(Clone)]
pub struct Hub(Arc<Inner>);

impl Hub {
    /// Opens the hub in `data_dir` (created with mode 0700; `hub.db` inside)
    /// with default options.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Hub, Error> {
        Self::open_with(data_dir, HubOptions::default())
    }

    pub fn open_with(data_dir: impl AsRef<Path>, options: HubOptions) -> Result<Hub, Error> {
        let dir = data_dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| Error::DataDir(dir.to_path_buf(), e))?;
        restrict(dir, 0o700);
        let path = dir.join("hub.db");
        let db = db::Db::open(&path).map_err(|e| Error::Database(path.clone(), e))?;
        restrict(&path, 0o600);
        let _ = db.purge_expired(now_ms());
        let decoy_hash = auth::hash_password("decoy password, never accepted")
            .map_err(|e| Error::Hashing(e.to_string()))?;
        Ok(Hub(Arc::new(Inner {
            db,
            registry: relay::Registry::default(),
            options,
            logins: auth::RateLimiter::new(auth::LOGIN_ATTEMPTS_PER_MINUTE),
            enrolments: auth::RateLimiter::new(servers::ENROL_ATTEMPTS_PER_MINUTE),
            decoy_hash,
        })))
    }

    pub fn options(&self) -> &HubOptions {
        &self.0.options
    }

    pub(crate) fn db(&self) -> &db::Db {
        &self.0.db
    }

    pub(crate) fn registry(&self) -> &relay::Registry {
        &self.0.registry
    }

    pub(crate) fn logins(&self) -> &auth::RateLimiter {
        &self.0.logins
    }

    pub(crate) fn enrolments(&self) -> &auth::RateLimiter {
        &self.0.enrolments
    }

    pub(crate) fn decoy_hash(&self) -> &str {
        &self.0.decoy_hash
    }

    /// The router of this hub (`/v1/...`).
    pub fn router(&self) -> Router {
        router(self.clone())
    }
}

/// Builds the router of `hub`.
pub fn router(hub: Hub) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/auth/signup", post(auth::signup))
        .route("/v1/auth/login", post(auth::login))
        .route("/v1/auth/logout", post(auth::logout))
        .route("/v1/auth/verify", post(auth::verify))
        .route("/v1/auth/password", post(auth::change_password))
        .route("/v1/me", get(auth::me).patch(auth::update_me))
        .route("/v1/tiers", get(tiers::list))
        .route("/v1/servers", get(servers::list))
        .route("/v1/servers/enroll-code", post(servers::enroll_code))
        .route("/v1/servers/enroll", post(servers::enroll))
        .route(
            "/v1/servers/{id}",
            axum::routing::patch(servers::rename).delete(servers::delete),
        )
        .route("/v1/relay/server", get(relay::server_socket))
        .route("/v1/relay/{server_id}/api/stream", get(relay::app_stream))
        .route("/v1/relay/{server_id}/api/{*rest}", any(relay::app_request))
        .route("/v1/admin/users/{id}/tier", post(auth::admin_set_tier))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(hub)
}

/// Binds `listen` and serves until the task is dropped or aborted.
pub async fn serve(
    hub: Hub,
    listen: SocketAddr,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let addr = listener.local_addr()?;
    let app = router(hub);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("hub stopped: {e}");
        }
    });
    Ok((addr, handle))
}

async fn health(State(hub): State<Hub>) -> Json<Value> {
    let mut body = json!({ "ok": true, "version": VERSION });
    if let Some(url) = &hub.options().public_url {
        body["publicUrl"] = Value::String(url.clone());
    }
    Json(body)
}

async fn not_found() -> ApiError {
    ApiError::not_found("no such route")
}

async fn method_not_allowed() -> impl IntoResponse {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method-not-allowed",
        "the route does not take this method",
    )
}

/// Milliseconds since the Unix epoch, the time unit of every timestamp here.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `prefix` + 32 random bytes as base64url without padding: sessions
/// (`ahs_`), server keys (`ask_`).
pub(crate) fn random_token(prefix: &str) -> String {
    use base64::Engine as _;
    let bytes: [u8; 32] = rand::random();
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub(crate) fn sha256_hex(text: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Best effort: the directory and database hold hashes and sessions.
fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}
