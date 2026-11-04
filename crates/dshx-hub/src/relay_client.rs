//! The relay client: runs inside `dsh-codex web`, keeps a WebSocket to the
//! hub and performs the hub's forwarded requests against the loopback
//! listener (SPEC §5.1). It adds the server's own start token and
//! `Host: 127.0.0.1:<port>`; the hub never sees that token.
//!
//! ```no_run
//! use dshx_hub::{RelayConfig, ServerInfo};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let server = ServerInfo {
//!     id: Some("srv_8f1c".into()),
//!     name: "VPS 2".into(),
//!     hostname: "oracle-vps-02".into(),
//!     platform: "linux".into(),
//!     version: "0.3.0".into(),
//! };
//! // Once, with a code from the app (Settings → account → "Server hinzufügen"):
//! let (_enrolled, server_key) =
//!     dshx_hub::enroll("https://hub.example", "ABCD-1234", &server).await?;
//!
//! // On every start of `dsh-codex web`; the task runs until it is aborted and
//! // reconnects with backoff (1 s … 60 s).
//! let status = std::sync::Arc::new(dshx_hub::RelayStatus::default());
//! let config = RelayConfig {
//!     hub_url: "https://hub.example".into(),
//!     server_key,
//!     local_base: "http://127.0.0.1:3080".into(),
//!     local_token: "<the start token>".into(),
//!     server,
//! };
//! let relay = tokio::spawn(dshx_hub::relay_client::run_with_status(config, status.clone()));
//! // `/api/info.hub.connected` reads `status.connected()`.
//! relay.abort();
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::relay::{Frame, collect_headers, skip_request_header, skip_response_header};
use crate::servers::Server;

pub const MIN_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A connection that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(30);
/// The hub pings every 30 s; silence this long means the socket is dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(100);
const LOCAL_TIMEOUT: Duration = Duration::from_secs(120);

/// How a server introduces itself: in the enrolment and in the `hello` frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// `srv_…` from `server.json`; `None` lets the hub choose one.
    #[serde(rename = "serverId", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub version: String,
}

/// Everything [`run`] needs.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// `https://hub.example` (or `http://127.0.0.1:8787` for a local hub).
    pub hub_url: String,
    /// `ask_…` from the enrolment.
    pub server_key: String,
    /// The loopback listener, `http://127.0.0.1:<port>`.
    pub local_base: String,
    /// The start token of that listener.
    pub local_token: String,
    pub server: ServerInfo,
}

/// What `/api/info.hub` reports: shared with the running relay task.
#[derive(Debug, Default)]
pub struct RelayStatus {
    connected: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl RelayStatus {
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Why the last connection ended, until the next one is up.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.last_error).clone()
    }

    fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
        if connected {
            *lock(&self.last_error) = None;
        }
    }

    fn set_error(&self, error: String) {
        *lock(&self.last_error) = Some(error);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The hub answered with `{error, code}`.
    #[error("{message} ({code})")]
    Hub {
        status: u16,
        code: String,
        message: String,
    },
    #[error("{0}")]
    Transport(#[from] reqwest::Error),
    #[error("{0}")]
    Protocol(String),
}

/// Keeps the server connected to the hub, forever: reconnects with backoff
/// from 1 s to 60 s. Drop or abort the future to stop.
pub fn run(config: RelayConfig) -> impl std::future::Future<Output = ()> + Send {
    run_with_status(config, Arc::new(RelayStatus::default()))
}

/// [`run`] with a shared [`RelayStatus`] for `/api/info`.
pub async fn run_with_status(config: RelayConfig, status: Arc<RelayStatus>) {
    let config = Arc::new(config);
    let http = match local_client() {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("hub relay: cannot build the HTTP client: {e}");
            status.set_error(e.to_string());
            return;
        }
    };
    let mut backoff = MIN_BACKOFF;
    loop {
        let (uptime, error) = connect_once(&config, &status, &http).await;
        status.set_connected(false);
        status.set_error(error.to_string());
        if uptime.is_some_and(|u| u >= STABLE_AFTER) {
            backoff = MIN_BACKOFF;
        }
        tracing::warn!(
            "hub relay: {error}; reconnecting in {} s",
            backoff.as_secs()
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Enrols this server with a code from the app: `(server, server_key)`.
/// The key is shown once; keep it in `credentials.toml`.
pub async fn enroll(
    hub_url: &str,
    code: &str,
    server: &ServerInfo,
) -> Result<(Server, String), Error> {
    let base = hub_url.trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(Error::Protocol(
            "the hub URL must start with http:// or https://".into(),
        ));
    }
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut body = serde_json::to_value(server).unwrap_or_default();
    body["code"] = Value::String(code.trim().to_owned());
    let response = http
        .post(format!("{base}/v1/servers/enroll"))
        .json(&body)
        .send()
        .await?;
    let status = response.status().as_u16();
    let json: Value = response.json().await.unwrap_or(Value::Null);
    if status != 201 {
        return Err(Error::Hub {
            status,
            code: json["code"].as_str().unwrap_or("error").to_owned(),
            message: json["error"]
                .as_str()
                .unwrap_or("the hub refused the enrolment")
                .to_owned(),
        });
    }
    let server: Server = serde_json::from_value(json["server"].clone())
        .map_err(|e| Error::Protocol(format!("unexpected enrol response: {e}")))?;
    let key = json["serverKey"]
        .as_str()
        .filter(|k| k.starts_with("ask_"))
        .ok_or_else(|| Error::Protocol("the enrol response has no server key".into()))?
        .to_owned();
    Ok((server, key))
}

type Sockets = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<LocalCmd>>>>;

enum LocalCmd {
    Send(String),
    Close(Option<u16>),
}

/// One connection, from the handshake until it ends: how long it was up
/// (`None` when it never came up) and why it ended.
async fn connect_once(
    config: &Arc<RelayConfig>,
    status: &RelayStatus,
    http: &reqwest::Client,
) -> (Option<Duration>, Error) {
    let request = match hub_socket_request(config) {
        Ok(request) => request,
        Err(e) => return (None, e),
    };
    let (ws, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(connected) => connected,
        Err(e) => {
            return (
                None,
                Error::Protocol(format!("connect to {}: {e}", config.hub_url)),
            );
        }
    };
    let started = Instant::now();
    status.set_connected(true);
    tracing::info!("hub relay: connected to {}", config.hub_url);
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Frame>();
    let sockets: Sockets = Arc::new(Mutex::new(HashMap::new()));
    let _ = out_tx.send(Frame::Hello {
        server: config.server.clone(),
    });
    let error = loop {
        tokio::select! {
            frame = out_rx.recv() => {
                let Some(frame) = frame else {
                    break Error::Protocol("relay stopped".into());
                };
                let text = serde_json::to_string(&frame).unwrap_or_default();
                if let Err(e) = sink.send(Message::text(text)).await {
                    break Error::Protocol(format!("send: {e}"));
                }
            }
            incoming = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => match incoming {
                Err(_) => break Error::Protocol(format!(
                    "nothing from the hub for {} s", IDLE_TIMEOUT.as_secs()
                )),
                Ok(None) | Ok(Some(Ok(Message::Close(_)))) => {
                    break Error::Protocol("the hub closed the connection".into());
                }
                Ok(Some(Err(e))) => break Error::Protocol(format!("receive: {e}")),
                Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<Frame>(&text) {
                    Ok(frame) => handle_hub_frame(config, http, &out_tx, &sockets, frame),
                    Err(e) => tracing::debug!("hub relay: unreadable frame: {e}"),
                },
                Ok(Some(Ok(_))) => {}
            }
        }
    };
    // The local streams end when their command senders go.
    lock(&sockets).clear();
    (Some(started.elapsed()), error)
}

fn handle_hub_frame(
    config: &Arc<RelayConfig>,
    http: &reqwest::Client,
    out: &mpsc::UnboundedSender<Frame>,
    sockets: &Sockets,
    frame: Frame,
) {
    match frame {
        Frame::Req {
            id,
            method,
            path,
            headers,
            body,
        } => {
            let incoming = Incoming {
                id,
                method,
                path,
                headers,
                body,
            };
            tokio::spawn(forward_request(
                config.clone(),
                http.clone(),
                out.clone(),
                incoming,
            ));
        }
        Frame::WsOpen { id, path } => {
            let (tx, rx) = mpsc::unbounded_channel();
            lock(sockets).insert(id.clone(), tx);
            tokio::spawn(forward_socket(
                config.clone(),
                out.clone(),
                sockets.clone(),
                id,
                path,
                rx,
            ));
        }
        Frame::WsMsg { id, text } => {
            if let Some(tx) = lock(sockets).get(&id) {
                let _ = tx.send(LocalCmd::Send(text));
            }
        }
        Frame::WsClose { id, code } => {
            if let Some(tx) = lock(sockets).remove(&id) {
                let _ = tx.send(LocalCmd::Close(code));
            }
        }
        Frame::Ping => {
            let _ = out.send(Frame::Pong);
        }
        // Never sent by the hub.
        Frame::Pong
        | Frame::Res { .. }
        | Frame::WsOpened { .. }
        | Frame::WsError { .. }
        | Frame::Hello { .. } => {}
    }
}

struct Incoming {
    id: String,
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
}

async fn forward_request(
    config: Arc<RelayConfig>,
    http: reqwest::Client,
    out: mpsc::UnboundedSender<Frame>,
    req: Incoming,
) {
    let response = match local_request(&config, &http, &req).await {
        Ok(frame) => frame,
        Err(e) => {
            tracing::debug!("hub relay: {} {} failed locally: {e}", req.method, req.path);
            error_response(&req.id, e)
        }
    };
    let _ = out.send(response);
}

async fn local_request(
    config: &RelayConfig,
    http: &reqwest::Client,
    req: &Incoming,
) -> Result<Frame, String> {
    let method =
        reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| format!("method: {e}"))?;
    let url = format!("{}{}", config.local_base.trim_end_matches('/'), req.path);
    let mut builder = http.request(method, &url);
    for (name, value) in &req.headers {
        if !skip_request_header(name) {
            builder = builder.header(name.as_str(), value.as_str());
        }
    }
    builder = builder
        .header(reqwest::header::HOST, local_host(&config.local_base))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.local_token),
        );
    if let Some(body) = &req.body {
        let bytes = BASE64.decode(body).map_err(|e| format!("body: {e}"))?;
        builder = builder.body(bytes);
    }
    let response = builder.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let headers = collect_headers(response.headers(), skip_response_header);
    let bytes = response.bytes().await.map_err(|e| format!("body: {e}"))?;
    Ok(Frame::Res {
        id: req.id.clone(),
        status,
        headers,
        body: (!bytes.is_empty()).then(|| BASE64.encode(&bytes)),
    })
}

/// A request that could not be made locally answers 502 with `{error}`.
fn error_response(id: &str, error: String) -> Frame {
    let body = serde_json::json!({ "error": error }).to_string();
    Frame::Res {
        id: id.to_owned(),
        status: 502,
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: Some(BASE64.encode(body)),
    }
}

type LocalSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn forward_socket(
    config: Arc<RelayConfig>,
    out: mpsc::UnboundedSender<Frame>,
    sockets: Sockets,
    id: String,
    path: String,
    mut rx: mpsc::UnboundedReceiver<LocalCmd>,
) {
    let ws = match open_local_socket(&config, &path).await {
        Ok(ws) => ws,
        Err(error) => {
            lock(&sockets).remove(&id);
            let _ = out.send(Frame::WsError { id, error });
            return;
        }
    };
    if out.send(Frame::WsOpened { id: id.clone() }).is_err() {
        return;
    }
    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(LocalCmd::Send(text)) => {
                    if sink.send(Message::text(text)).await.is_err() {
                        let _ = out.send(Frame::WsClose { id: id.clone(), code: Some(1006) });
                        break;
                    }
                }
                Some(LocalCmd::Close(code)) => {
                    let frame = CloseFrame {
                        code: CloseCode::from(code.unwrap_or(1000)),
                        reason: "".into(),
                    };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    break;
                }
                // The hub connection ended.
                None => {
                    let _ = sink.close().await;
                    break;
                }
            },
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    let frame = Frame::WsMsg { id: id.clone(), text: text.as_str().to_owned() };
                    if out.send(frame).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let code = frame.map_or(1000, |f| u16::from(f.code));
                    let _ = out.send(Frame::WsClose { id: id.clone(), code: Some(code) });
                    break;
                }
                None | Some(Err(_)) => {
                    let _ = out.send(Frame::WsClose { id: id.clone(), code: Some(1006) });
                    break;
                }
                Some(Ok(_)) => {}
            }
        }
    }
    lock(&sockets).remove(&id);
}

async fn open_local_socket(config: &RelayConfig, path: &str) -> Result<LocalSocket, String> {
    let base = ws_scheme(&config.local_base)?;
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.local_token))
            .map_err(|e| e.to_string())?,
    );
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

fn hub_socket_request(
    config: &RelayConfig,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, Error> {
    let base = ws_scheme(&config.hub_url).map_err(Error::Protocol)?;
    let url = format!("{}/v1/relay/server", base.trim_end_matches('/'));
    let mut request = url
        .into_client_request()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.server_key))
            .map_err(|e| Error::Protocol(e.to_string()))?,
    );
    Ok(request)
}

/// `http://` → `ws://`, `https://` → `wss://`.
fn ws_scheme(url: &str) -> Result<String, String> {
    if let Some(rest) = url.strip_prefix("http://") {
        Ok(format!("ws://{rest}"))
    } else if let Some(rest) = url.strip_prefix("https://") {
        Ok(format!("wss://{rest}"))
    } else if url.starts_with("ws://") || url.starts_with("wss://") {
        Ok(url.to_owned())
    } else {
        Err(format!("{url:?} must start with http:// or https://"))
    }
}

/// The authority of `local_base`: `127.0.0.1:3080`.
fn local_host(local_base: &str) -> String {
    let rest = local_base
        .split_once("://")
        .map_or(local_base, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

fn local_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(LOCAL_TIMEOUT)
        .build()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            ws_scheme("http://127.0.0.1:8787").unwrap(),
            "ws://127.0.0.1:8787"
        );
        assert_eq!(
            ws_scheme("https://hub.example/").unwrap(),
            "wss://hub.example/"
        );
        assert!(ws_scheme("hub.example").is_err());
        assert_eq!(local_host("http://127.0.0.1:3080"), "127.0.0.1:3080");
        assert_eq!(local_host("http://127.0.0.1:3080/"), "127.0.0.1:3080");
    }

    #[test]
    fn server_info_serializes_like_the_enrol_body() {
        let info = ServerInfo {
            id: Some("srv_1".into()),
            name: "Mac".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["serverId"], "srv_1");
        assert_eq!(json["hostname"], "");
        let without_id = ServerInfo::default();
        assert!(
            serde_json::to_value(&without_id)
                .unwrap()
                .get("serverId")
                .is_none()
        );
    }
}
