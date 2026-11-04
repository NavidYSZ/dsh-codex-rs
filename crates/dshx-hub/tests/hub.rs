//! The hub end to end: accounts, tiers, enrolment and the relay round trip
//! against a tiny stand-in for `dsh-codex web`.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dshx_hub::{Hub, HubOptions, RelayConfig, RelayStatus, ServerInfo};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite;

const PASSWORD: &str = "correct horse battery";
const LOCAL_TOKEN: &str = "local-start-token";

/// A running hub and the client the tests talk with.
struct TestHub {
    base: String,
    http: reqwest::Client,
    dir: tempfile::TempDir,
    _task: tokio::task::JoinHandle<()>,
}

async fn start_hub(options: HubOptions) -> TestHub {
    let dir = tempfile::tempdir().unwrap();
    let hub = Hub::open_with(dir.path(), options).unwrap();
    let (addr, task) = dshx_hub::serve(hub, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    TestHub {
        base: format!("http://{addr}"),
        http: reqwest::Client::new(),
        dir,
        _task: task,
    }
}

impl TestHub {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn db_path(&self) -> PathBuf {
        self.dir.path().join("hub.db")
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> (u16, Value) {
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        (status, body)
    }

    async fn signup(&self, email: &str) -> (String, Value) {
        let (status, body) = self
            .send(self.http.post(self.url("/v1/auth/signup")).json(&json!({
                "email": email, "password": PASSWORD, "name": "Ada",
            })))
            .await;
        assert_eq!(status, 201, "{body}");
        (body["session"].as_str().unwrap().to_owned(), body)
    }

    async fn me(&self, session: &str) -> (u16, Value) {
        self.send(self.http.get(self.url("/v1/me")).bearer_auth(session))
            .await
    }

    async fn enroll_code(&self, session: &str) -> (u16, Value) {
        self.send(
            self.http
                .post(self.url("/v1/servers/enroll-code"))
                .bearer_auth(session),
        )
        .await
    }

    async fn enroll(&self, code: &str, name: &str, id: Option<&str>) -> (u16, Value) {
        let mut body = json!({
            "code": code, "name": name, "hostname": "host", "platform": "test", "version": "0",
        });
        if let Some(id) = id {
            body["serverId"] = Value::String(id.to_owned());
        }
        self.send(self.http.post(self.url("/v1/servers/enroll")).json(&body))
            .await
    }

    async fn servers(&self, session: &str) -> Value {
        self.send(self.http.get(self.url("/v1/servers")).bearer_auth(session))
            .await
            .1
    }

    async fn wait_online(&self, session: &str, id: &str, online: bool) {
        for _ in 0..100 {
            let list = self.servers(session).await;
            if list
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["id"] == id && s["online"] == online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("server {id} did not reach online={online}");
    }
}

// A stand-in for `dsh-codex web`: bearer and loopback Host checks, an echo
// route and a WebSocket echo at /api/stream.

#[derive(Clone)]
struct FakeState {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn start_fake() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = FakeState {
        port: addr.port(),
        seen: seen.clone(),
    };
    let app = Router::new()
        .route("/api/info", get(fake_info))
        .route("/api/echo", post(fake_echo))
        .route("/api/stream", get(fake_stream))
        .with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, seen)
}

fn authorized(state: &FakeState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    host == Some(format!("127.0.0.1:{}", state.port).as_str())
        && (bearer == Some(LOCAL_TOKEN) || query_token == Some(LOCAL_TOKEN))
}

async fn fake_info(State(state): State<FakeState>, headers: HeaderMap) -> Response {
    state.seen.lock().unwrap().push(format!(
        "info host={:?} auth={:?} extra={:?}",
        headers.get(header::HOST).and_then(|h| h.to_str().ok()),
        headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok()),
        headers.get("x-extra").and_then(|h| h.to_str().ok()),
    ));
    if !authorized(&state, &headers, None) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "version": "fake", "server": { "id": "srv_fake" } })),
    )
        .into_response()
}

async fn fake_echo(
    State(state): State<FakeState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !authorized(&state, &headers, None) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    (
        StatusCode::CREATED,
        [
            (header::CONTENT_TYPE, content_type),
            (
                HeaderName::from_static("x-echo-length"),
                body.len().to_string(),
            ),
        ],
        body,
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    token: Option<String>,
    session: Option<String>,
}

async fn fake_stream(
    State(state): State<FakeState>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    state.seen.lock().unwrap().push(format!(
        "stream session={:?} token_in_query={}",
        q.session,
        q.token.is_some()
    ));
    if !authorized(&state, &headers, q.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(|mut socket: WebSocket| async move {
        let _ = socket.send(WsMessage::Text("ready".into())).await;
        while let Some(Ok(msg)) = socket.recv().await {
            match msg {
                WsMessage::Text(text) => {
                    let reply = format!("echo:{}", text.as_str());
                    if socket.send(WsMessage::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
    })
}

fn relay_config(hub: &TestHub, key: &str, fake: SocketAddr, server: ServerInfo) -> RelayConfig {
    RelayConfig {
        hub_url: hub.base.clone(),
        server_key: key.to_owned(),
        local_base: format!("http://{fake}"),
        local_token: LOCAL_TOKEN.into(),
        server,
    }
}

// Accounts

#[tokio::test]
async fn signup_me_logout() {
    let hub = start_hub(HubOptions::default()).await;
    let (session, body) = hub.signup("Ada@Example.com ").await;
    assert!(session.starts_with("ahs_"));
    assert_eq!(body["user"]["email"], "ada@example.com");
    assert_eq!(body["user"]["emailVerified"], false);
    assert!(body["user"]["id"].as_str().unwrap().starts_with("usr_"));
    assert!(body.get("verification").is_none(), "no code outside --dev");

    let (status, me) = hub.me(&session).await;
    assert_eq!(status, 200);
    assert_eq!(me["user"]["name"], "Ada");
    assert_eq!(me["tier"]["id"], "free");
    assert_eq!(me["tier"]["limits"]["servers"], 2);
    assert_eq!(me["usage"]["servers"], 0);

    let (status, body) = hub
        .send(
            hub.http
                .patch(hub.url("/v1/me"))
                .bearer_auth(&session)
                .json(&json!({ "name": "  Ada L. " })),
        )
        .await;
    assert_eq!(
        (status, body["user"]["name"].as_str()),
        (200, Some("Ada L."))
    );

    let (status, _) = hub
        .send(
            hub.http
                .post(hub.url("/v1/auth/logout"))
                .bearer_auth(&session),
        )
        .await;
    assert_eq!(status, 204);
    let (status, body) = hub.me(&session).await;
    assert_eq!((status, body["code"].as_str()), (401, Some("unauthorized")));
    let (status, _) = hub.send(hub.http.get(hub.url("/v1/me"))).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn login_checks_the_password_and_limits_attempts() {
    let hub = start_hub(HubOptions::default()).await;
    hub.signup("bob@example.com").await;
    let login = |email: &str, password: &str| {
        hub.send(
            hub.http
                .post(hub.url("/v1/auth/login"))
                .json(&json!({ "email": email, "password": password })),
        )
    };
    let (status, body) = login("BOB@example.com", "wrong password!").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("invalid-credentials"))
    );
    let (status, body) = login("BOB@example.com", PASSWORD).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["session"].as_str().unwrap().starts_with("ahs_"));
    assert_eq!(body["user"]["email"], "bob@example.com");
    // An unknown email gets the same answer as a wrong password.
    let (status, body) = login("nobody@example.com", PASSWORD).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("invalid-credentials"))
    );
    // Ten attempts a minute per email: two are spent, eight more pass.
    for _ in 0..8 {
        assert_eq!(login("bob@example.com", "wrong password!").await.0, 401);
    }
    let (status, body) = login("bob@example.com", PASSWORD).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (429, Some("too-many-attempts"))
    );
    assert_eq!(login("nobody@example.com", PASSWORD).await.0, 401);
}

#[tokio::test]
async fn verification_code_in_dev_mode() {
    let hub = start_hub(HubOptions {
        dev: true,
        ..Default::default()
    })
    .await;
    let (session, body) = hub.signup("eve@example.com").await;
    let code = body["verification"]["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
    assert!(body["verification"]["expiresAt"].is_number());
    let verify = |code: &str| {
        hub.send(
            hub.http
                .post(hub.url("/v1/auth/verify"))
                .bearer_auth(&session)
                .json(&json!({ "code": code })),
        )
    };
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let (status, body) = verify(wrong).await;
    assert_eq!((status, body["code"].as_str()), (400, Some("invalid-code")));
    let (status, body) = verify(&code).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["emailVerified"], true);
    let (_, me) = hub.me(&session).await;
    assert_eq!(me["user"]["emailVerified"], true);
}

#[tokio::test]
async fn signup_validation_and_error_shape() {
    let hub = start_hub(HubOptions::default()).await;
    let signup = |body: Value| hub.send(hub.http.post(hub.url("/v1/auth/signup")).json(&body));
    let (status, body) = signup(json!({ "email": "not-an-email", "password": PASSWORD })).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("invalid-email"))
    );
    assert!(body["error"].as_str().unwrap().contains("email"));
    let (status, body) = signup(json!({ "email": "a@example.com", "password": "short" })).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("weak-password"))
    );
    let (status, body) = signup(json!({
        "email": "a@example.com", "password": PASSWORD, "name": "x".repeat(81),
    }))
    .await;
    assert_eq!((status, body["code"].as_str()), (400, Some("invalid-name")));
    hub.signup("dup@example.com").await;
    let (status, body) = signup(json!({ "email": "DUP@example.com", "password": PASSWORD })).await;
    assert_eq!((status, body["code"].as_str()), (409, Some("email-taken")));

    let response = hub
        .http
        .post(hub.url("/v1/auth/signup"))
        .header("content-type", "application/json")
        .body("{nope")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "invalid-body");

    let (status, body) = hub.send(hub.http.get(hub.url("/v1/nope"))).await;
    assert_eq!((status, body["code"].as_str()), (404, Some("not-found")));
    let (status, body) = hub.send(hub.http.delete(hub.url("/v1/tiers"))).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (405, Some("method-not-allowed"))
    );
}

#[tokio::test]
async fn password_change_ends_other_sessions() {
    let hub = start_hub(HubOptions::default()).await;
    let (s1, _) = hub.signup("pw@example.com").await;
    let login = |password: &str| {
        hub.send(
            hub.http
                .post(hub.url("/v1/auth/login"))
                .json(&json!({ "email": "pw@example.com", "password": password })),
        )
    };
    let (_, body) = login(PASSWORD).await;
    let s2 = body["session"].as_str().unwrap().to_owned();
    let change = |current: &str, new: &str| {
        hub.send(
            hub.http
                .post(hub.url("/v1/auth/password"))
                .bearer_auth(&s1)
                .json(&json!({ "current": current, "new": new })),
        )
    };
    let (status, body) = change("wrong wrong wrong", "another long password").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("invalid-credentials"))
    );
    let (status, body) = change(PASSWORD, "short").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("weak-password"))
    );
    let (status, _) = change(PASSWORD, "another long password").await;
    assert_eq!(status, 204);
    assert_eq!(hub.me(&s1).await.0, 200, "the changing session stays");
    assert_eq!(hub.me(&s2).await.0, 401, "the other session ended");
    assert_eq!(login(PASSWORD).await.0, 401);
    assert_eq!(login("another long password").await.0, 200);
}

#[tokio::test]
async fn health_reports_the_public_url() {
    let hub = start_hub(HubOptions {
        public_url: Some("http://hub.local".into()),
        ..Default::default()
    })
    .await;
    let (status, body) = hub.send(hub.http.get(hub.url("/v1/health"))).await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], dshx_hub::VERSION);
    assert_eq!(body["publicUrl"], "http://hub.local");
}

// Tiers and servers

#[tokio::test]
async fn tiers_limit_and_admin_change() {
    let hub = start_hub(HubOptions {
        admin_token: Some("admin-secret".into()),
        ..Default::default()
    })
    .await;
    let (status, tiers) = hub.send(hub.http.get(hub.url("/v1/tiers"))).await;
    assert_eq!(status, 200);
    assert_eq!(
        tiers,
        json!([
            { "id": "free", "name": "Free", "limits": { "servers": 2, "relay": true }, "priceEurMonthly": null },
            { "id": "pro", "name": "Pro", "limits": { "servers": 10, "relay": true }, "priceEurMonthly": 9.99 },
            { "id": "team", "name": "Team", "limits": { "servers": null, "relay": true }, "priceEurMonthly": 29.0 },
        ])
    );

    let (session, body) = hub.signup("tier@example.com").await;
    let user_id = body["user"]["id"].as_str().unwrap().to_owned();
    // Two servers fit into free; the third enrol code is refused.
    for n in 1..=2 {
        let (status, code) = hub.enroll_code(&session).await;
        assert_eq!(status, 200, "{code}");
        assert_eq!(code["code"].as_str().unwrap().len(), 9);
        assert!(code["expiresAt"].is_number());
        let (status, body) = hub
            .enroll(code["code"].as_str().unwrap(), &format!("Server {n}"), None)
            .await;
        assert_eq!(status, 201, "{body}");
    }
    let (status, body) = hub.enroll_code(&session).await;
    assert_eq!((status, body["code"].as_str()), (403, Some("tier-limit")));
    assert_eq!(hub.me(&session).await.1["usage"]["servers"], 2);

    let admin_url = hub.url(&format!("/v1/admin/users/{user_id}/tier"));
    let (status, _) = hub
        .send(
            hub.http
                .post(&admin_url)
                .bearer_auth("nope")
                .json(&json!({ "tierId": "pro" })),
        )
        .await;
    assert_eq!(status, 401);
    let (status, body) = hub
        .send(
            hub.http
                .post(&admin_url)
                .bearer_auth("admin-secret")
                .json(&json!({ "tierId": "gold" })),
        )
        .await;
    assert_eq!((status, body["code"].as_str()), (400, Some("unknown-tier")));
    let (status, _) = hub
        .send(
            hub.http
                .post(hub.url("/v1/admin/users/usr_nope/tier"))
                .bearer_auth("admin-secret")
                .json(&json!({ "tierId": "pro" })),
        )
        .await;
    assert_eq!(status, 404);
    let (status, body) = hub
        .send(
            hub.http
                .post(&admin_url)
                .bearer_auth("admin-secret")
                .json(&json!({ "tierId": "pro" })),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["tier"]["id"], "pro");
    assert_eq!(body["user"]["id"], user_id);
    assert_eq!(hub.enroll_code(&session).await.0, 200, "pro allows more");
    assert_eq!(hub.me(&session).await.1["tier"]["id"], "pro");

    // Without an admin token the routes are off.
    let plain = start_hub(HubOptions::default()).await;
    let (status, body) = plain
        .send(
            plain
                .http
                .post(plain.url("/v1/admin/users/x/tier"))
                .bearer_auth("admin-secret")
                .json(&json!({ "tierId": "pro" })),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("admin-disabled"))
    );
}

#[tokio::test]
async fn enroll_code_lifecycle() {
    let hub = start_hub(HubOptions::default()).await;
    let (session, _) = hub.signup("codes@example.com").await;

    let (status, body) = hub.enroll("ZZZZ-9999", "x", None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (404, Some("enroll-code-unknown"))
    );
    let (status, body) = hub.enroll("nope", "x", None).await;
    assert_eq!((status, body["code"].as_str()), (400, Some("invalid-code")));
    let info = ServerInfo::default();
    match dshx_hub::enroll(&hub.base, "ZZZZ-9999", &info).await {
        Err(dshx_hub::relay_client::Error::Hub { status, code, .. }) => {
            assert_eq!((status, code.as_str()), (404, "enroll-code-unknown"));
        }
        other => panic!("unexpected: {other:?}"),
    }

    // Used once, in any spelling.
    let (_, body) = hub.enroll_code(&session).await;
    let code = body["code"].as_str().unwrap().to_owned();
    let (status, body) = hub
        .enroll(&code.to_lowercase().replace('-', ""), "First", None)
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["server"]["name"], "First");
    assert!(body["server"]["id"].as_str().unwrap().starts_with("srv_"));
    assert!(body["serverKey"].as_str().unwrap().starts_with("ask_"));
    assert_eq!(body["server"]["online"], false);
    let (status, body) = hub.enroll(&code, "Again", None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("enroll-code-used"))
    );

    // A newer code replaces the user's unused one.
    let (_, c1) = hub.enroll_code(&session).await;
    let (_, c2) = hub.enroll_code(&session).await;
    let (status, body) = hub.enroll(c1["code"].as_str().unwrap(), "x", None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (404, Some("enroll-code-unknown"))
    );

    // Expired: age the code in the database.
    {
        let db = rusqlite::Connection::open(hub.db_path()).unwrap();
        db.execute("UPDATE enroll_codes SET expires_at = 0", [])
            .unwrap();
    }
    let (status, body) = hub.enroll(c2["code"].as_str().unwrap(), "x", None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("enroll-code-expired"))
    );

    // A known id enrolled again by its owner gets a new key; by someone
    // else it is refused.
    let (s2, _) = hub.signup("codes2@example.com").await;
    let (_, c) = hub.enroll_code(&s2).await;
    let (status, body) = hub
        .enroll(c["code"].as_str().unwrap(), "Box", Some("srv_box"))
        .await;
    assert_eq!(status, 201, "{body}");
    let key1 = body["serverKey"].as_str().unwrap().to_owned();
    let (_, c) = hub.enroll_code(&s2).await;
    let (status, body) = hub
        .enroll(c["code"].as_str().unwrap(), "Box again", Some("srv_box"))
        .await;
    assert_eq!(status, 201, "{body}");
    assert_ne!(body["serverKey"].as_str().unwrap(), key1);
    let list = hub.servers(&s2).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "Box again");
    let (_, c) = hub.enroll_code(&session).await;
    let (status, body) = hub
        .enroll(c["code"].as_str().unwrap(), "Theirs", Some("srv_box"))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("server-exists"))
    );
    let (status, body) = hub
        .enroll(c["code"].as_str().unwrap(), "Bad id", Some("cli_x"))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("invalid-server-id"))
    );
}

#[tokio::test]
async fn servers_list_rename_delete() {
    let hub = start_hub(HubOptions::default()).await;
    let (s1, _) = hub.signup("list1@example.com").await;
    let (s2, _) = hub.signup("list2@example.com").await;
    let (_, code) = hub.enroll_code(&s1).await;
    let (status, _) = hub
        .enroll(code["code"].as_str().unwrap(), "Box", Some("srv_box"))
        .await;
    assert_eq!(status, 201);
    let list = hub.servers(&s1).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], "srv_box");
    assert_eq!(list[0]["hostname"], "host");
    assert_eq!(list[0]["online"], false);
    assert_eq!(list[0]["lastSeenAt"], Value::Null);
    assert_eq!(hub.servers(&s2).await, json!([]));

    let patch = |session: &str, name: &str| {
        hub.send(
            hub.http
                .patch(hub.url("/v1/servers/srv_box"))
                .bearer_auth(session)
                .json(&json!({ "name": name })),
        )
    };
    let (status, body) = patch(&s2, "Stolen").await;
    assert_eq!((status, body["code"].as_str()), (403, Some("forbidden")));
    let (status, _) = hub
        .send(
            hub.http
                .delete(hub.url("/v1/servers/srv_box"))
                .bearer_auth(&s2),
        )
        .await;
    assert_eq!(status, 403);
    let (status, body) = patch(&s1, "Big box").await;
    assert_eq!((status, body["name"].as_str()), (200, Some("Big box")));
    let (status, body) = patch(&s1, "  ").await;
    assert_eq!((status, body["code"].as_str()), (400, Some("invalid-name")));

    let (status, _) = hub
        .send(
            hub.http
                .delete(hub.url("/v1/servers/srv_box"))
                .bearer_auth(&s1),
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(hub.servers(&s1).await, json!([]));
    let (status, body) = hub
        .send(
            hub.http
                .delete(hub.url("/v1/servers/srv_box"))
                .bearer_auth(&s1),
        )
        .await;
    assert_eq!((status, body["code"].as_str()), (404, Some("not-found")));
    assert_eq!(hub.me(&s1).await.1["usage"]["servers"], 0);
}

// Relay

#[tokio::test]
async fn relay_round_trip() {
    let hub = start_hub(HubOptions::default()).await;
    let (owner, _) = hub.signup("owner@example.com").await;
    let (other, _) = hub.signup("other@example.com").await;
    let (_, code) = hub.enroll_code(&owner).await;
    let (fake, seen) = start_fake().await;
    let info = ServerInfo {
        id: Some("srv_test".into()),
        name: "Fake".into(),
        hostname: "fake-host".into(),
        platform: "test".into(),
        version: "0.0.1".into(),
    };
    let (server, key) = dshx_hub::enroll(&hub.base, code["code"].as_str().unwrap(), &info)
        .await
        .unwrap();
    assert_eq!(server.id, "srv_test");
    assert!(key.starts_with("ask_"));
    assert!(!server.online);

    // Offline until the relay client connects.
    let info_url = hub.url("/v1/relay/srv_test/api/info");
    let (status, body) = hub.send(hub.http.get(&info_url).bearer_auth(&owner)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (503, Some("server-offline"))
    );

    let status_handle = Arc::new(RelayStatus::default());
    let relay = tokio::spawn(dshx_hub::relay_client::run_with_status(
        relay_config(&hub, &key, fake, info.clone()),
        status_handle.clone(),
    ));
    hub.wait_online(&owner, "srv_test", true).await;
    assert!(status_handle.connected());
    let list = hub.servers(&owner).await;
    assert!(list[0]["lastSeenAt"].is_number());

    // GET through the hub: the stand-in saw the loopback Host, the local
    // token and the app's own header, never the hub session.
    let response = hub
        .http
        .get(&info_url)
        .bearer_auth(&owner)
        .header("x-extra", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["version"], "fake");
    let expected = format!(
        "info host=Some(\"127.0.0.1:{}\") auth=Some(\"Bearer {LOCAL_TOKEN}\") extra=Some(\"1\")",
        fake.port()
    );
    assert!(
        seen.lock().unwrap().contains(&expected),
        "{:?}",
        seen.lock().unwrap()
    );

    // A POST with a binary body comes back byte for byte, status and
    // headers included.
    let payload = vec![0u8, 1, 2, 255, 7];
    let response = hub
        .http
        .post(hub.url("/v1/relay/srv_test/api/echo"))
        .bearer_auth(&owner)
        .header("content-type", "application/octet-stream")
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers().get("x-echo-length").unwrap(), "5");
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
    assert_eq!(response.bytes().await.unwrap().to_vec(), payload);

    // Someone else's server, no session, unknown server.
    let (status, body) = hub.send(hub.http.get(&info_url).bearer_auth(&other)).await;
    assert_eq!((status, body["code"].as_str()), (403, Some("forbidden")));
    assert_eq!(hub.send(hub.http.get(&info_url)).await.0, 401);
    let (status, _) = hub
        .send(
            hub.http
                .get(hub.url("/v1/relay/srv_nope/api/info"))
                .bearer_auth(&owner),
        )
        .await;
    assert_eq!(status, 404);

    // The stream: hub session in the query, dropped before forwarding; echo.
    let ws_base = hub.base.replace("http://", "ws://");
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "{ws_base}/v1/relay/srv_test/api/stream?session=s1&token={owner}"
    ))
    .await
    .unwrap();
    let first = ws.next().await.unwrap().unwrap();
    assert_eq!(first.to_text().unwrap(), "ready");
    ws.send(tungstenite::Message::text("hello")).await.unwrap();
    let echoed = ws.next().await.unwrap().unwrap();
    assert_eq!(echoed.to_text().unwrap(), "echo:hello");
    ws.close(None).await.unwrap();
    assert!(
        seen.lock()
            .unwrap()
            .contains(&"stream session=Some(\"s1\") token_in_query=false".to_owned()),
        "{:?}",
        seen.lock().unwrap()
    );
    match tokio_tungstenite::connect_async(format!(
        "{ws_base}/v1/relay/srv_test/api/stream?session=s1"
    ))
    .await
    {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected a 401 handshake, got {:?}", other.map(|_| ())),
    }

    // The relay client goes away: offline again.
    relay.abort();
    hub.wait_online(&owner, "srv_test", false).await;
    let (status, body) = hub.send(hub.http.get(&info_url).bearer_auth(&owner)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (503, Some("server-offline"))
    );
}

#[tokio::test]
async fn deleting_a_server_closes_its_relay() {
    let hub = start_hub(HubOptions::default()).await;
    let (session, _) = hub.signup("del@example.com").await;
    let (_, code) = hub.enroll_code(&session).await;
    let (fake, _) = start_fake().await;
    let info = ServerInfo {
        name: "Gone soon".into(),
        ..Default::default()
    };
    let (server, key) = dshx_hub::enroll(&hub.base, code["code"].as_str().unwrap(), &info)
        .await
        .unwrap();
    let status = Arc::new(RelayStatus::default());
    let relay = tokio::spawn(dshx_hub::relay_client::run_with_status(
        relay_config(&hub, &key, fake, info),
        status.clone(),
    ));
    hub.wait_online(&session, &server.id, true).await;

    let (code, _) = hub
        .send(
            hub.http
                .delete(hub.url(&format!("/v1/servers/{}", server.id)))
                .bearer_auth(&session),
        )
        .await;
    assert_eq!(code, 204);
    for _ in 0..100 {
        if !status.connected() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!status.connected(), "the connection ends with the server");
    let (code, body) = hub
        .send(
            hub.http
                .get(hub.url(&format!("/v1/relay/{}/api/info", server.id)))
                .bearer_auth(&session),
        )
        .await;
    assert_eq!((code, body["code"].as_str()), (404, Some("not-found")));
    relay.abort();
}
