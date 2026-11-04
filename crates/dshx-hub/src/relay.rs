//! The relay, hub side (SPEC §5.1): every server keeps one WebSocket to the
//! hub; the app's requests to `/v1/relay/:serverId/api/*` travel over it as
//! JSON frames and are answered from the server's loopback listener by the
//! [`relay_client`](crate::relay_client).
//!
//! Frames on the server socket (bodies base64, standard alphabet, padded):
//!
//! - hub → server `{"t":"req","id":"r12","method":"GET","path":"/api/info","headers":{…},"body":null}`
//! - server → hub `{"t":"res","id":"r12","status":200,"headers":{…},"body":"<b64>"}`
//! - hub → server `{"t":"ws-open","id":"w3","path":"/api/stream?session=…"}`;
//!   server → hub `{"t":"ws-opened","id":"w3"}` or `{"t":"ws-error","id":"w3","error":"…"}`
//! - both ways `{"t":"ws-msg","id":"w3","text":"…"}` and `{"t":"ws-close","id":"w3","code":1000}`
//! - keepalive `{"t":"ping"}` / `{"t":"pong"}` every 30 s; three missed pongs = offline
//! - server → hub `{"t":"hello","server":{…}}` once after connecting (an
//!   addition: hostname, platform and version of the server are refreshed)

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::body::Body as HttpBody;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::auth::{self, Session};
use crate::relay_client::ServerInfo;
use crate::{ApiError, ApiResult, Hub, now_ms, sha256_hex};

/// Largest request body the relay forwards.
pub const MAX_BODY: usize = 100 * 1024 * 1024;
pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_MISSED_PINGS: u32 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const WS_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// `lastSeenAt` is written at most once a minute while a server is connected.
const SEEN_THROTTLE_MS: i64 = 60_000;

/// One frame of the server socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Frame {
    Req {
        id: String,
        method: String,
        path: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        body: Option<String>,
    },
    Res {
        id: String,
        status: u16,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        body: Option<String>,
    },
    WsOpen {
        id: String,
        path: String,
    },
    WsOpened {
        id: String,
    },
    WsError {
        id: String,
        error: String,
    },
    WsMsg {
        id: String,
        text: String,
    },
    WsClose {
        id: String,
        #[serde(default)]
        code: Option<u16>,
    },
    Ping,
    Pong,
    Hello {
        server: ServerInfo,
    },
}

/// Headers that describe one hop and never cross the relay.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The app's `Authorization` (the hub session) and `Host` stay at the hub;
/// `Content-Length` is recomputed from the body.
pub(crate) fn skip_request_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    HOP_BY_HOP.contains(&name.as_str())
        || matches!(name.as_str(), "host" | "authorization" | "content-length")
}

pub(crate) fn skip_response_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    HOP_BY_HOP.contains(&name.as_str()) || name == "content-length"
}

/// Headers as the frames carry them; repeated headers join with ", ".
pub(crate) fn collect_headers(
    headers: &HeaderMap,
    skip: fn(&str) -> bool,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let name = name.as_str();
        let Ok(value) = value.to_str() else {
            continue;
        };
        if skip(name) {
            continue;
        }
        out.entry(name.to_owned())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(value);
            })
            .or_insert_with(|| value.to_owned());
    }
    out
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the server side reports about one of the app's streams.
pub(crate) enum SocketEvent {
    Opened,
    Error(String),
    Msg(String),
    Close(Option<u16>),
}

/// A server's live connection: the way to its socket and what waits on it.
pub(crate) struct Conn {
    generation: u64,
    pub server_id: String,
    tx: mpsc::UnboundedSender<Frame>,
    pending: Mutex<HashMap<String, oneshot::Sender<Frame>>>,
    sockets: Mutex<HashMap<String, mpsc::UnboundedSender<SocketEvent>>>,
    next_id: AtomicU64,
    closed: CancellationToken,
}

impl Conn {
    fn send(&self, frame: Frame) -> Result<(), ()> {
        self.tx.send(frame).map_err(|_| ())
    }

    fn next_id(&self, prefix: char) -> String {
        format!(
            "{prefix}{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    /// Ends the connection: in-flight requests fail, the app's streams close.
    fn close(&self) {
        self.closed.cancel();
        lock(&self.pending).clear();
        lock(&self.sockets).clear();
    }
}

/// The connected servers, by id.
#[derive(Default)]
pub struct Registry {
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    generations: AtomicU64,
}

impl Registry {
    pub fn is_online(&self, server_id: &str) -> bool {
        lock(&self.conns).contains_key(server_id)
    }

    /// Closes a server's connection (deletion, re-enrolment).
    pub fn disconnect(&self, server_id: &str) {
        if let Some(conn) = lock(&self.conns).remove(server_id) {
            conn.close();
        }
    }

    fn get(&self, server_id: &str) -> Option<Arc<Conn>> {
        lock(&self.conns).get(server_id).cloned()
    }

    /// Registers a fresh connection; a connection it replaces comes back.
    fn register(
        &self,
        server_id: &str,
    ) -> (Arc<Conn>, mpsc::UnboundedReceiver<Frame>, Option<Arc<Conn>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = Arc::new(Conn {
            generation: self.generations.fetch_add(1, Ordering::Relaxed),
            server_id: server_id.to_owned(),
            tx,
            pending: Mutex::new(HashMap::new()),
            sockets: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            closed: CancellationToken::new(),
        });
        let replaced = lock(&self.conns).insert(server_id.to_owned(), conn.clone());
        (conn, rx, replaced)
    }

    /// Removes the connection unless a newer one took its place.
    fn unregister(&self, conn: &Conn) {
        let mut conns = lock(&self.conns);
        if conns
            .get(&conn.server_id)
            .is_some_and(|current| current.generation == conn.generation)
        {
            conns.remove(&conn.server_id);
        }
    }
}

// Server side

/// `GET /v1/relay/server` with `Authorization: Bearer ask_…`.
pub async fn server_socket(
    State(hub): State<Hub>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(key) = auth::bearer(&headers).filter(|k| k.starts_with("ask_")) else {
        return ApiError::unauthorized().into_response();
    };
    let server = match hub.db().server_by_key(&sha256_hex(&key)) {
        Ok(Some(server)) => server,
        Ok(None) => return ApiError::unauthorized().into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    ws.on_upgrade(move |socket| serve_server(hub, server.id, socket))
}

async fn serve_server(hub: Hub, server_id: String, mut socket: WebSocket) {
    let (conn, mut out_rx, replaced) = hub.registry().register(&server_id);
    match replaced {
        Some(old) => {
            old.close();
            tracing::info!("relay: server {server_id} connected again; the older connection ends");
        }
        None => tracing::info!("relay: server {server_id} online"),
    }
    let _ = hub.db().touch_server(&server_id, now_ms());
    let mut last_seen = now_ms();
    let mut missed = 0u32;
    let mut ticks =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            frame = out_rx.recv() => {
                let Some(frame) = frame else { break };
                let text = serde_json::to_string(&frame).unwrap_or_default();
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    missed = 0;
                    let now = now_ms();
                    if now - last_seen >= SEEN_THROTTLE_MS {
                        last_seen = now;
                        let _ = hub.db().touch_server(&server_id, now);
                    }
                    match serde_json::from_str::<Frame>(&text) {
                        Ok(frame) => handle_server_frame(&hub, &conn, frame),
                        Err(e) => tracing::debug!("relay: {server_id}: unreadable frame: {e}"),
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = ticks.tick() => {
                if missed >= MAX_MISSED_PINGS {
                    tracing::warn!("relay: server {server_id} missed {missed} pings; offline");
                    break;
                }
                missed += 1;
                if conn.send(Frame::Ping).is_err() {
                    break;
                }
            }
            _ = conn.closed.cancelled() => break,
        }
    }
    conn.close();
    hub.registry().unregister(&conn);
    let _ = hub.db().touch_server(&server_id, now_ms());
    let _ = socket.send(Message::Close(None)).await;
    tracing::info!("relay: server {server_id} offline");
}

fn handle_server_frame(hub: &Hub, conn: &Conn, frame: Frame) {
    if let Frame::Res { id, .. } = &frame {
        let waiter = lock(&conn.pending).remove(id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(frame);
        }
        return;
    }
    match frame {
        Frame::WsOpened { id } => deliver(conn, &id, SocketEvent::Opened),
        Frame::WsMsg { id, text } => deliver(conn, &id, SocketEvent::Msg(text)),
        Frame::WsError { id, error } => {
            if let Some(tx) = lock(&conn.sockets).remove(&id) {
                let _ = tx.send(SocketEvent::Error(error));
            }
        }
        Frame::WsClose { id, code } => {
            if let Some(tx) = lock(&conn.sockets).remove(&id) {
                let _ = tx.send(SocketEvent::Close(code));
            }
        }
        Frame::Ping => {
            let _ = conn.send(Frame::Pong);
        }
        Frame::Hello { server } => {
            let _ = hub.db().update_server_meta(
                &conn.server_id,
                server.hostname.trim(),
                server.platform.trim(),
                server.version.trim(),
            );
        }
        // Answered above, or never sent by a server.
        Frame::Pong | Frame::Res { .. } | Frame::Req { .. } | Frame::WsOpen { .. } => {}
    }
}

fn deliver(conn: &Conn, id: &str, event: SocketEvent) {
    if let Some(tx) = lock(&conn.sockets).get(id) {
        let _ = tx.send(event);
    }
}

// App side

/// The server's connection, once it is the user's, relay-enabled and online.
fn target(hub: &Hub, session: &Session, server_id: &str) -> ApiResult<Arc<Conn>> {
    let row = crate::servers::owned_server(hub, session, server_id)?;
    let tier = crate::tiers::of_user(hub, &session.user)?;
    if !tier.limits.relay {
        return Err(ApiError::forbidden(
            "relay-disabled",
            format!("the {} tier does not include the relay", tier.name),
        ));
    }
    hub.registry()
        .get(&row.id)
        .ok_or_else(ApiError::server_offline)
}

/// `ANY /v1/relay/:serverId/api/*`: forwarded as one `req` frame, answered
/// by one `res` frame.
pub async fn app_request(
    State(hub): State<Hub>,
    session: Session,
    Path((server_id, rest)): Path<(String, String)>,
    request: Request,
) -> ApiResult<Response> {
    let conn = target(&hub, &session, &server_id)?;
    let path = forwarded_path(request.uri(), &server_id, &rest);
    let method = request.method().to_string();
    let headers = collect_headers(request.headers(), skip_request_header);
    let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload-too-large",
                "the body could not be read or exceeds 100 MB",
            )
        })?;
    let body = (!body.is_empty()).then(|| BASE64.encode(&body));
    let id = conn.next_id('r');
    let (tx, rx) = oneshot::channel();
    lock(&conn.pending).insert(id.clone(), tx);
    let frame = Frame::Req {
        id: id.clone(),
        method,
        path,
        headers,
        body,
    };
    if conn.send(frame).is_err() {
        lock(&conn.pending).remove(&id);
        return Err(ApiError::server_offline());
    }
    match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
        Ok(Ok(Frame::Res {
            status,
            headers,
            body,
            ..
        })) => Ok(build_response(status, headers, body)),
        Ok(Ok(_)) => Err(anyhow::anyhow!("relay: unexpected frame for {id}").into()),
        // The connection went away while the request was in flight.
        Ok(Err(_)) => Err(ApiError::server_offline()),
        Err(_) => {
            lock(&conn.pending).remove(&id);
            Err(ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "relay-timeout",
                "the server did not answer in time",
            ))
        }
    }
}

/// The raw path after `/v1/relay/<id>` (percent-encoding intact), falling
/// back to the decoded wildcard when the id itself was encoded.
fn forwarded_path(uri: &Uri, server_id: &str, rest: &str) -> String {
    let prefix = format!("/v1/relay/{server_id}");
    let path = match uri.path().strip_prefix(&prefix) {
        Some(p) if p.starts_with("/api/") => p.to_owned(),
        _ => format!("/api/{rest}"),
    };
    match uri.query() {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path,
    }
}

fn build_response(
    status: u16,
    headers: BTreeMap<String, String>,
    body: Option<String>,
) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = match body {
        Some(b64) => match BASE64.decode(b64) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("relay: response body is not base64: {e}");
                return ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "relay-error",
                    "the server sent an unreadable response",
                )
                .into_response();
            }
        },
        None => Vec::new(),
    };
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if skip_response_header(&name) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            response = response.header(name, value);
        }
    }
    response
        .body(HttpBody::from(bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// `GET /v1/relay/:serverId/api/stream` (WebSocket): the hub session comes as
/// bearer or `?token=`; the token is dropped from the forwarded path.
pub async fn app_stream(
    State(hub): State<Hub>,
    Path(server_id): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = auth::bearer(&headers).or_else(|| query_param(uri.query(), "token"));
    let session = match token.map(|t| auth::authenticate(&hub, &t)) {
        Some(Ok(Some(session))) => session,
        Some(Err(e)) => return ApiError::from(e).into_response(),
        _ => return ApiError::unauthorized().into_response(),
    };
    let conn = match target(&hub, &session, &server_id) {
        Ok(conn) => conn,
        Err(e) => return e.into_response(),
    };
    let path = match without_token(uri.query()) {
        Some(query) => format!("/api/stream?{query}"),
        None => "/api/stream".to_owned(),
    };
    ws.on_upgrade(move |socket| pump_app_socket(conn, socket, path))
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .map(|v| v.to_owned())
}

fn without_token(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query?
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("token="))
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

async fn pump_app_socket(conn: Arc<Conn>, mut socket: WebSocket, path: String) {
    let id = conn.next_id('w');
    let (tx, mut rx) = mpsc::unbounded_channel();
    lock(&conn.sockets).insert(id.clone(), tx);
    let server_done = pump(&conn, &id, &mut socket, &mut rx, path).await;
    lock(&conn.sockets).remove(&id);
    if !server_done {
        let _ = conn.send(Frame::WsClose {
            id,
            code: Some(1000),
        });
    }
}

/// Runs the stream; true when the server side already ended it (no
/// `ws-close` needs to follow).
async fn pump(
    conn: &Conn,
    id: &str,
    socket: &mut WebSocket,
    rx: &mut mpsc::UnboundedReceiver<SocketEvent>,
    path: String,
) -> bool {
    if conn
        .send(Frame::WsOpen {
            id: id.to_owned(),
            path,
        })
        .is_err()
    {
        close_app(socket, 1012, "server offline").await;
        return true;
    }
    match tokio::time::timeout(WS_OPEN_TIMEOUT, rx.recv()).await {
        Ok(Some(SocketEvent::Opened)) => {}
        Ok(Some(SocketEvent::Msg(text))) => {
            if socket.send(Message::Text(text.into())).await.is_err() {
                return false;
            }
        }
        Ok(Some(SocketEvent::Error(error))) => {
            close_app(socket, 1011, &error).await;
            return true;
        }
        Ok(Some(SocketEvent::Close(code))) => {
            close_app(socket, code.unwrap_or(1000), "").await;
            return true;
        }
        Ok(None) => {
            close_app(socket, 1012, "server offline").await;
            return true;
        }
        Err(_) => {
            close_app(socket, 1011, "the server did not open the stream").await;
            return false;
        }
    }
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(SocketEvent::Msg(text)) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        return false;
                    }
                }
                Some(SocketEvent::Opened) => {}
                Some(SocketEvent::Error(error)) => {
                    close_app(socket, 1011, &error).await;
                    return true;
                }
                Some(SocketEvent::Close(code)) => {
                    close_app(socket, code.unwrap_or(1000), "").await;
                    return true;
                }
                None => {
                    close_app(socket, 1012, "server offline").await;
                    return true;
                }
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let frame = Frame::WsMsg { id: id.to_owned(), text: text.as_str().to_owned() };
                    if conn.send(frame).is_err() {
                        return true;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let _ = conn.send(Frame::WsClose { id: id.to_owned(), code: frame.map(|f| f.code) });
                    return true;
                }
                None | Some(Err(_)) => return false,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn close_app(socket: &mut WebSocket, code: u16, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_owned().into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_the_documented_shape() {
        let req = Frame::Req {
            id: "r12".into(),
            method: "GET".into(),
            path: "/api/info".into(),
            headers: BTreeMap::from([("accept".to_owned(), "application/json".to_owned())]),
            body: None,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"t":"req","id":"r12","method":"GET","path":"/api/info","headers":{"accept":"application/json"},"body":null}"#
        );
        let parsed: Frame =
            serde_json::from_str(r#"{"t":"ws-close","id":"w3","code":1000}"#).unwrap();
        assert_eq!(
            parsed,
            Frame::WsClose {
                id: "w3".into(),
                code: Some(1000)
            }
        );
        assert_eq!(
            serde_json::to_string(&Frame::Ping).unwrap(),
            r#"{"t":"ping"}"#
        );
        assert!(serde_json::from_str::<Frame>(r#"{"t":"nope"}"#).is_err());
    }

    #[test]
    fn stream_query_drops_the_token() {
        assert_eq!(
            without_token(Some("session=s1&token=ahs_x&after=0")).as_deref(),
            Some("session=s1&after=0")
        );
        assert_eq!(without_token(Some("token=ahs_x")), None);
        assert_eq!(without_token(None), None);
        assert_eq!(
            query_param(Some("session=s1&token=ahs_x"), "token").as_deref(),
            Some("ahs_x")
        );
    }

    #[test]
    fn forwarded_paths_keep_the_raw_form() {
        let uri: Uri = "/v1/relay/srv_1/api/sessions/a%20b?after=0"
            .parse()
            .unwrap();
        assert_eq!(
            forwarded_path(&uri, "srv_1", "sessions/a b"),
            "/api/sessions/a%20b?after=0"
        );
        let uri: Uri = "/v1/relay/srv%5F1/api/info".parse().unwrap();
        assert_eq!(forwarded_path(&uri, "srv_1", "info"), "/api/info");
    }

    #[test]
    fn header_filters() {
        assert!(skip_request_header("Authorization"));
        assert!(skip_request_header("host"));
        assert!(skip_request_header("Transfer-Encoding"));
        assert!(!skip_request_header("content-type"));
        assert!(skip_response_header("content-length"));
        assert!(!skip_response_header("cache-control"));
    }
}
