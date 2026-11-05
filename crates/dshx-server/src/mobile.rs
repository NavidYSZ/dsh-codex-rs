//! The Aether app (docs/aether/SPEC.md §3): client tokens next to the start
//! token, a TLS listener on every interface for the phone, pairing payloads
//! with a QR code, and the server's identity.
//!
//! The pieces `dsh-codex pair` shares are public (the client store, the
//! identity, the certificate, the addresses, the payload and the QR
//! renderers); the routes and the listener belong to the server.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Extension, Path as RoutePath, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use dshx_core::config::MobileConfig;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{ApiResult, AppState, NotFound};

pub mod clients;
pub mod identity;
pub mod net;
pub mod pairing;
pub mod qr;
pub mod tls;

pub use clients::{Client, ClientStore};
pub use identity::ServerInfo;
pub use pairing::{ClientRef, Payload, Transport};

/// What the binary gives the server so it can run the Aether listener: the
/// home folder (client list, certificate, identity), the `[codex.mobile]`
/// settings and a way to write them back.
pub trait MobileHost: Send + Sync + 'static {
    /// `$DSHX_HOME`: `mobile/clients.json`, `mobile/cert.pem` and
    /// `server.json` live under it.
    fn home(&self) -> PathBuf;
    fn settings(&self) -> MobileConfig;
    /// Writes `[codex.mobile]`; only the keys that changed touch the file.
    fn update_settings(&self, settings: MobileConfig) -> anyhow::Result<()>;
    /// The binary's version, for `server.version`.
    fn version(&self) -> String;
}

/// Request extension: the request arrived on the Aether listener, so the
/// loopback Host check does not apply and loopback-only routes answer 404.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MobileConn;

/// Request extension: the bearer was the token of this client.
#[derive(Clone, Debug)]
pub(crate) struct ClientAuth {
    pub id: String,
}

/// The listener while it runs; dropping it stops the accept loop.
struct Listener {
    port: u16,
    cert_sha256: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The server's side of the Aether app, shared by both listeners.
pub(crate) struct Mobile {
    host: Option<Arc<dyn MobileHost>>,
    clients: Option<ClientStore>,
    listener: Mutex<Option<Listener>>,
    /// The router with the `MobileConn` marker, set once it is built.
    router: OnceLock<Router>,
    /// The loopback listener's port, for the `http` transport.
    loopback_port: AtomicU16,
    /// Ids of revoked clients, so their WebSockets close.
    revoked: broadcast::Sender<String>,
}

impl Mobile {
    pub(crate) fn new(host: Option<Arc<dyn MobileHost>>) -> Self {
        let clients = host.as_ref().map(|h| ClientStore::new(&h.home()));
        Mobile {
            host,
            clients,
            listener: Mutex::new(None),
            router: OnceLock::new(),
            loopback_port: AtomicU16::new(0),
            revoked: broadcast::channel(64).0,
        }
    }

    pub(crate) fn set_router(&self, router: Router) {
        let _ = self.router.set(router);
    }

    pub(crate) fn set_loopback_port(&self, port: u16) {
        self.loopback_port.store(port, Ordering::Relaxed);
    }

    /// The client whose token this is, if any.
    pub(crate) fn authenticate(&self, token: &str) -> Option<String> {
        self.clients.as_ref()?.authenticate(token)
    }

    /// Resolves once client `id` is revoked (or the feed fell behind, in
    /// which case the socket closes and the app reconnects).
    pub(crate) fn revoked(&self, id: String) -> impl Future<Output = ()> + Send + use<> {
        let mut rx = self.revoked.subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(revoked) if revoked == id => return,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => return,
                    Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
                }
            }
        }
    }

    /// Whether `[codex.mobile] enabled` asks for the listener at start.
    pub(crate) fn wants_start(&self) -> bool {
        self.host.as_ref().is_some_and(|h| h.settings().enabled)
    }

    /// Port and fingerprint while the listener runs.
    pub(crate) fn listening(&self) -> Option<(u16, String)> {
        self.listener
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| (l.port, l.cert_sha256.clone()))
    }

    /// Starts the listener on `port` (0 = any), replacing one on another
    /// port; a listener already on `port` stays. A taken port is an error
    /// and leaves a running listener alone.
    pub(crate) fn start(&self, port: u16) -> anyhow::Result<()> {
        let host = self
            .host
            .as_ref()
            .ok_or_else(|| NotFound("mobile".into()))?;
        let router = self
            .router
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the listener has no router yet"))?;
        if let Some((running, _)) = self.listening()
            && (running == port || port == 0)
        {
            return Ok(());
        }
        let name = identity::server_name(&host.settings().name);
        let cert = tls::ensure_cert(&host.home(), &name)?;
        let acceptor = tls::acceptor(&cert)?;
        let listener = tls::bind(port).map_err(|e| anyhow::anyhow!("port {port}: {e}"))?;
        let port = listener.local_addr()?.port();
        let task = tls::spawn_accept_loop(listener, acceptor, router);
        *self.listener.lock().unwrap() = Some(Listener {
            port,
            cert_sha256: cert.sha256.clone(),
            task,
        });
        let shown = net::urls(&net::addresses(), port)
            .into_iter()
            .next()
            .unwrap_or_else(|| format!("https://0.0.0.0:{port}"));
        eprintln!(
            "aether: listening on {shown} (fingerprint {}…)",
            &cert.sha256[..16]
        );
        Ok(())
    }

    pub(crate) fn stop(&self) {
        self.listener.lock().unwrap().take();
    }
}

/// Writes `bytes` to `path` with mode 0600, replacing the file.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

/// Marks a request as one of the Aether listener (its router is the
/// loopback router behind this layer).
pub(crate) async fn mark_mobile(mut request: Request) -> Request {
    request.extensions_mut().insert(MobileConn);
    request
}

/// Routes that exist only on the loopback listener (previews, the OAuth
/// callback) answer 404 on the Aether one.
pub(crate) async fn loopback_only(request: Request, next: Next) -> Response {
    if request.extensions().get::<MobileConn>().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/mobile", get(get_mobile).put(put_mobile))
        .route("/mobile/pair", post(pair))
        .route(
            "/mobile/clients/{id}",
            patch(rename_client).delete(revoke_client),
        )
}

/// `/api/info` gains `server` and `mobile`.
pub(crate) fn with_info(s: &AppState, mut info: Value) -> Value {
    if let Some(host) = &s.mobile.host
        && let Some(obj) = info.as_object_mut()
    {
        let settings = host.settings();
        match identity::server_info(&host.home(), &settings.name, &host.version()) {
            Ok(server) => {
                obj.insert("server".into(), json!(server));
            }
            Err(e) => tracing::warn!("server identity: {e:#}"),
        }
        let listening = s.mobile.listening();
        obj.insert(
            "mobile".into(),
            json!({
                "enabled": settings.enabled,
                "port": listening.as_ref().map_or(settings.port, |(p, _)| *p),
                "listening": listening.is_some(),
            }),
        );
    }
    info
}

fn parts(s: &AppState) -> ApiResult<(&dyn MobileHost, &ClientStore)> {
    match (&s.mobile.host, &s.mobile.clients) {
        (Some(host), Some(clients)) => Ok((host.as_ref(), clients)),
        _ => Err(NotFound("mobile".into()).into()),
    }
}

/// The user this process runs as: on Linux its effective uid in
/// `/etc/passwd`, else `$USER`.
fn process_user() -> Option<String> {
    let from_passwd = || {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let uid = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))?
            .split_whitespace()
            .nth(1)?
            .to_owned();
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        passwd.lines().find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            (fields.nth(1)? == uid).then(|| name.to_owned())
        })
    };
    from_passwd().or_else(|| std::env::var("USER").ok().filter(|u| !u.is_empty()))
}

/// The systemd unit this process runs in (`/proc/self/cgroup`), without
/// `.service`.
fn systemd_unit() -> Option<String> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    cgroup.lines().find_map(|line| {
        let path = line.rsplit(':').next()?;
        let unit = path.rsplit('/').find(|part| part.ends_with(".service"))?;
        let name = unit.strip_suffix(".service")?;
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// What `dsh-codex pair --ssh` needs to know about this server when the
/// phone reaches it through SSH (`--remote-user`, `--remote-home`,
/// `--remote-bin`, `--remote-port`, `--unit`); unknown values are `null`.
fn ssh_pair(s: &AppState, host: &dyn MobileHost) -> Value {
    let bin = std::env::current_exe().ok().map(|p| {
        let shown = p.display().to_string();
        shown
            .strip_suffix(" (deleted)")
            .map_or(shown.clone(), str::to_owned)
    });
    let port = s.mobile.loopback_port.load(Ordering::Relaxed);
    json!({
        "user": process_user(),
        "home": host.home(),
        "bin": bin,
        "port": (port != 0).then_some(port),
        "unit": systemd_unit(),
    })
}

/// The `GET /api/mobile` document; `current` is the requesting client.
/// `port` is the listening port while the listener runs (else the saved
/// one), `configuredPort` always the saved one (0 = any free port).
fn view(s: &AppState, current: Option<&str>) -> ApiResult<Value> {
    let (host, store) = parts(s)?;
    let settings = host.settings();
    let server = identity::server_info(&host.home(), &settings.name, &host.version())?;
    let clients: Vec<Value> = store
        .list()
        .iter()
        .map(|c| c.view(current == Some(c.id.as_str())))
        .collect();
    let mut out = json!({
        "enabled": settings.enabled,
        "port": settings.port,
        "configuredPort": settings.port,
        "server": server,
        "clients": clients,
        "sshPair": ssh_pair(s, host),
    });
    if let Some((port, sha)) = s.mobile.listening() {
        out["port"] = json!(port);
        out["addresses"] = json!(net::addresses());
        out["certSha256"] = json!(sha);
    }
    Ok(out)
}

fn current_id(client: &Option<Extension<ClientAuth>>) -> Option<&str> {
    client.as_ref().map(|c| c.id.as_str())
}

async fn get_mobile(
    State(s): State<AppState>,
    client: Option<Extension<ClientAuth>>,
) -> ApiResult<Json<Value>> {
    Ok(Json(view(&s, current_id(&client))?))
}

#[derive(Deserialize)]
struct SettingsPatch {
    enabled: Option<bool>,
    port: Option<u16>,
    name: Option<String>,
}

async fn put_mobile(
    State(s): State<AppState>,
    client: Option<Extension<ClientAuth>>,
    Json(patch): Json<SettingsPatch>,
) -> ApiResult<Json<Value>> {
    let (host, _) = parts(&s)?;
    let mut next = host.settings();
    if let Some(enabled) = patch.enabled {
        next.enabled = enabled;
    }
    if let Some(port) = patch.port {
        next.port = port;
    }
    if let Some(name) = patch.name {
        next.name = name.trim().chars().take(80).collect();
    }
    if next.enabled {
        s.mobile.start(next.port)?;
    } else {
        s.mobile.stop();
    }
    host.update_settings(next)?;
    Ok(Json(view(&s, current_id(&client))?))
}

#[derive(Deserialize, Default)]
struct PairBody {
    name: Option<String>,
    /// `https` (default; only while the listener runs) and `http` (the
    /// loopback URL, for development).
    transports: Option<Vec<String>>,
}

async fn pair(
    State(s): State<AppState>,
    Json(body): Json<PairBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (host, store) = parts(&s)?;
    let name = body
        .name
        .map(|n| n.trim().chars().take(80).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "iPhone".into());
    let wanted = body.transports.unwrap_or_else(|| vec!["https".into()]);
    let settings = host.settings();
    let server = identity::server_info(&host.home(), &settings.name, &host.version())?;
    let (client, token) = store.create(&name, "ios", "ui")?;
    let mut transports = Vec::new();
    if wanted.iter().any(|t| t == "https")
        && let Some((port, sha)) = s.mobile.listening()
    {
        let urls = net::urls(&net::addresses(), port);
        if !urls.is_empty() {
            transports.push(Transport::Https {
                urls,
                cert_sha256: Some(sha),
            });
        }
    }
    if wanted.iter().any(|t| t == "http") {
        let port = s.mobile.loopback_port.load(Ordering::Relaxed);
        if port != 0 {
            transports.push(Transport::Http {
                urls: vec![format!("http://127.0.0.1:{port}")],
            });
        }
    }
    let payload = Payload {
        v: pairing::VERSION,
        server,
        client: ClientRef {
            id: client.id.clone(),
            name: client.name.clone(),
        },
        token: Some(token.clone()),
        transports,
    };
    let link = payload.link();
    let qr_svg = qr::svg(&link)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "client": client.view(false),
            "token": token,
            "payload": payload,
            "link": link,
            "qrSvg": qr_svg,
        })),
    ))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename_client(
    State(s): State<AppState>,
    RoutePath(id): RoutePath<String>,
    client: Option<Extension<ClientAuth>>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<Value>> {
    let (_, store) = parts(&s)?;
    let name: String = body.name.trim().chars().take(80).collect();
    if name.is_empty() {
        return Err(anyhow::anyhow!("the name must not be empty").into());
    }
    let renamed = store
        .rename(&id, &name)?
        .ok_or_else(|| NotFound(id.clone()))?;
    Ok(Json(renamed.view(current_id(&client) == Some(id.as_str()))))
}

async fn revoke_client(
    State(s): State<AppState>,
    RoutePath(id): RoutePath<String>,
) -> ApiResult<StatusCode> {
    let (_, store) = parts(&s)?;
    if !store.revoke(&id)? {
        return Err(NotFound(id).into());
    }
    let _ = s.mobile.revoked.send(id);
    Ok(StatusCode::NO_CONTENT)
}
