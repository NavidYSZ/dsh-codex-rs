//! OAuth against a local mock: a protected MCP server and an authorization
//! server with a path issuer, metadata only at the second well-known
//! location, dynamic registration, PKCE, `iss`, refresh-token rotation,
//! revocation and a 403 `insufficient_scope` step-up.
mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use common::{call, ctx, only_text};
use dshx_core::config::{McpOAuthConfig, McpServerConfig};
use dshx_mcp::{CallbackParams, LoginRequest, McpHandle, OAuthManager};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Mock {
    base: String,
    /// Leave `resource_metadata` out of the challenge (well-known fallback).
    no_hint: bool,
    no_pkce: bool,
    wrong_issuer: bool,
    /// Lifetime of issued access tokens.
    ttl: u64,
    clients: HashMap<String, Vec<String>>,
    codes: HashMap<String, Value>,
    /// Access token → granted scope.
    access: HashMap<String, String>,
    /// Valid refresh token → scope.
    refresh: HashMap<String, String>,
    used_refresh: HashSet<String>,
    registrations: Vec<Value>,
    refreshes: u32,
    revoked: Vec<String>,
    metadata_hits: Vec<String>,
    seq: u32,
}

type Shared = Arc<Mutex<Mock>>;

impl Mock {
    fn issuer(&self) -> String {
        if self.wrong_issuer {
            format!("{}/other", self.base)
        } else {
            format!("{}/tenant", self.base)
        }
    }

    fn next(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    fn tokens(&mut self, scope: &str) -> Value {
        let access = self.next("at");
        let refresh = self.next("rt");
        self.access.insert(access.clone(), scope.to_owned());
        self.refresh.insert(refresh.clone(), scope.to_owned());
        json!({"access_token": access, "token_type": "Bearer", "expires_in": self.ttl,
               "refresh_token": refresh, "scope": scope})
    }
}

fn challenge(m: &Mock, status: StatusCode, extra: &str) -> Response {
    let hint = if m.no_hint {
        String::new()
    } else {
        format!(
            ", resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            m.base
        )
    };
    let value = format!("Bearer realm=\"mock\"{hint}{extra}");
    (status, [(header::WWW_AUTHENTICATE, value)], "").into_response()
}

async fn mcp(State(s): State<Shared>, headers: HeaderMap, Json(msg): Json<Value>) -> Response {
    let m = s.lock().unwrap();
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let scope = match token.as_ref().and_then(|t| m.access.get(t)) {
        Some(scope) => scope.clone(),
        None => {
            let error = if token.is_some() {
                ", error=\"invalid_token\""
            } else {
                ", scope=\"mcp:read\""
            };
            return challenge(&m, StatusCode::UNAUTHORIZED, error);
        }
    };
    let id = msg.get("id").cloned();
    let result = match msg["method"].as_str().unwrap_or_default() {
        "initialize" => json!({"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                               "serverInfo": {"name": "mock", "version": "1"}}),
        "tools/list" => json!({"tools": [
            {"name": "whoami", "inputSchema": {"type": "object"}},
            {"name": "admin", "inputSchema": {"type": "object"}}]}),
        "tools/call" if msg["params"]["name"] == "admin" => {
            if !scope.split(' ').any(|s| s == "mcp:admin") {
                return challenge(
                    &m,
                    StatusCode::FORBIDDEN,
                    ", error=\"insufficient_scope\", scope=\"mcp:read mcp:admin\"",
                );
            }
            json!({"content": [{"type": "text", "text": "admin ok"}]})
        }
        "tools/call" => {
            json!({"content": [{"type": "text", "text": format!("token {}", token.unwrap())}]})
        }
        _ => return StatusCode::ACCEPTED.into_response(),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

async fn resource_metadata(State(s): State<Shared>) -> Json<Value> {
    let m = s.lock().unwrap();
    Json(json!({"resource": format!("{}/mcp", m.base),
                "authorization_servers": [format!("{}/tenant", m.base)],
                "scopes_supported": ["mcp:read", "mcp:write"]}))
}

async fn missing(State(s): State<Shared>, uri: axum::http::Uri) -> StatusCode {
    s.lock().unwrap().metadata_hits.push(uri.path().to_owned());
    StatusCode::NOT_FOUND
}

async fn server_metadata(State(s): State<Shared>, uri: axum::http::Uri) -> Json<Value> {
    let mut m = s.lock().unwrap();
    m.metadata_hits.push(uri.path().to_owned());
    let t = format!("{}/tenant", m.base);
    Json(json!({
        "issuer": m.issuer(),
        "authorization_endpoint": format!("{t}/authorize"),
        "token_endpoint": format!("{t}/token"),
        "registration_endpoint": format!("{t}/register"),
        "revocation_endpoint": format!("{t}/revoke"),
        "code_challenge_methods_supported": if m.no_pkce { json!(["plain"]) } else { json!(["S256"]) },
        "authorization_response_iss_parameter_supported": true,
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

async fn register(State(s): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut m = s.lock().unwrap();
    m.registrations.push(body.clone());
    let id = m.next("client");
    let uris: Vec<String> = body["redirect_uris"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap().to_owned())
        .collect();
    m.clients.insert(id.clone(), uris);
    (
        StatusCode::CREATED,
        Json(json!({"client_id": id, "token_endpoint_auth_method": "none"})),
    )
        .into_response()
}

async fn authorize(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut m = s.lock().unwrap();
    let redirect = &q["redirect_uri"];
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["resource"], format!("{}/mcp", m.base));
    assert!(m.clients[&q["client_id"]].contains(redirect));
    let code = m.next("code");
    m.codes.insert(code.clone(), json!(q));
    let iss = m.issuer();
    let mut location = url::Url::parse(redirect).unwrap();
    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &q["state"])
        .append_pair("iss", &iss);
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

fn pkce(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn token(State(s): State<Shared>, Form(f): Form<HashMap<String, String>>) -> Response {
    let mut m = s.lock().unwrap();
    let bad = |e: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    assert_eq!(f["resource"], format!("{}/mcp", m.base));
    match f["grant_type"].as_str() {
        "authorization_code" => {
            let Some(req) = m.codes.remove(&f["code"]) else {
                return bad("invalid_grant");
            };
            if pkce(&f["code_verifier"]) != req["code_challenge"]
                || f["redirect_uri"] != req["redirect_uri"]
                || f["client_id"] != req["client_id"]
            {
                return bad("invalid_grant");
            }
            let scope = req["scope"].as_str().unwrap_or("mcp:read").to_owned();
            Json(m.tokens(&scope)).into_response()
        }
        "refresh_token" => {
            let rt = &f["refresh_token"];
            let Some(scope) = m.refresh.remove(rt) else {
                m.used_refresh.insert(rt.clone());
                return bad("invalid_grant");
            };
            m.used_refresh.insert(rt.clone());
            m.refreshes += 1;
            Json(m.tokens(&scope)).into_response()
        }
        _ => bad("unsupported_grant_type"),
    }
}

async fn revoke(State(s): State<Shared>, Form(f): Form<HashMap<String, String>>) -> StatusCode {
    let mut m = s.lock().unwrap();
    m.access.remove(&f["token"]);
    m.refresh.remove(&f["token"]);
    m.revoked.push(f["token"].clone());
    StatusCode::OK
}

async fn start_mock(setup: impl FnOnce(&mut Mock)) -> Shared {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut mock = Mock {
        base,
        ttl: 3600,
        ..Default::default()
    };
    setup(&mut mock);
    let state: Shared = Arc::new(Mutex::new(mock));
    let app = Router::new()
        .route(
            "/mcp",
            post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/tenant",
            get(missing),
        )
        .route(
            "/.well-known/openid-configuration/tenant",
            get(server_metadata),
        )
        .route("/tenant/register", post(register))
        .route("/tenant/authorize", get(authorize))
        .route("/tenant/token", post(token))
        .route("/tenant/revoke", post(revoke))
        .with_state(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    state
}

const REDIRECT: &str = "http://127.0.0.1:3082/api/oauth/callback";

struct Setup {
    _dir: tempfile::TempDir,
    dir: std::path::PathBuf,
    mock: Shared,
    manager: Arc<OAuthManager>,
    handle: McpHandle,
    url: url::Url,
    servers: BTreeMap<String, McpServerConfig>,
}

async fn setup(configure: impl FnOnce(&mut Mock)) -> Setup {
    let mock = start_mock(configure).await;
    let base = mock.lock().unwrap().base.clone();
    let dir = tempfile::tempdir().unwrap();
    let manager = OAuthManager::new(dir.path().join("oauth"));
    let handle = McpHandle::new(Some(manager.clone()));
    let url = url::Url::parse(&format!("{base}/mcp")).unwrap();
    let servers: BTreeMap<String, McpServerConfig> = [(
        "t".to_owned(),
        McpServerConfig {
            url: Some(url.to_string()),
            ..Default::default()
        },
    )]
    .into();
    Setup {
        dir: dir.path().to_path_buf(),
        _dir: dir,
        mock,
        manager,
        handle,
        url,
        servers,
    }
}

impl Setup {
    fn status(&self) -> dshx_mcp::ServerInfo {
        self.handle
            .status()
            .into_iter()
            .find(|s| s.name == "t")
            .unwrap()
    }

    async fn start_login(&self) -> anyhow::Result<url::Url> {
        let challenge = self.handle.challenge("t");
        let url = self
            .manager
            .start(LoginRequest {
                server: "t",
                url: &self.url,
                oauth: &McpOAuthConfig::default(),
                headers: &BTreeMap::new(),
                challenge: challenge.as_ref(),
                redirect_uri: REDIRECT,
            })
            .await?;
        Ok(url::Url::parse(&url).unwrap())
    }

    /// Plays the browser: follows the authorization URL to the redirect.
    async fn browse(&self, auth: &url::Url) -> CallbackParams {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let r = http.get(auth.clone()).send().await.unwrap();
        assert_eq!(r.status(), 302);
        let location = url::Url::parse(r.headers()["location"].to_str().unwrap()).unwrap();
        assert!(location.as_str().starts_with(REDIRECT));
        let q: HashMap<String, String> = location.query_pairs().into_owned().collect();
        CallbackParams {
            code: q.get("code").cloned(),
            state: q.get("state").cloned(),
            iss: q.get("iss").cloned(),
            ..Default::default()
        }
    }

    async fn login(&self) {
        let auth = self.start_login().await.unwrap();
        let params = self.browse(&auth).await;
        let done = self.manager.complete(params).await.unwrap();
        assert_eq!(done.server, "t");
        self.handle
            .restart("t", Some(Duration::from_secs(10)))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn discovery_registration_pkce_refresh_rotation_and_step_up() {
    let s = setup(|_| {}).await;
    s.handle
        .apply(&s.servers, Some(Duration::from_secs(10)))
        .await
        .unwrap();
    let st = s.status();
    assert_eq!(st.status, "needs-login");
    assert!(st.tools.is_empty());
    assert!(
        st.challenge
            .as_ref()
            .unwrap()
            .resource_metadata
            .as_ref()
            .unwrap()
            .ends_with("/.well-known/oauth-protected-resource/mcp")
    );
    assert!(s.manager.status(&s.url).is_none());

    let auth = s.start_login().await.unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert_eq!(q["scope"], "mcp:read", "the challenge's scope wins");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["code_challenge"].len(), 43);
    assert_eq!(q["redirect_uri"], REDIRECT);
    {
        let m = s.mock.lock().unwrap();
        // RFC 8414 path insertion first, then OIDC path insertion.
        assert_eq!(
            m.metadata_hits,
            [
                "/.well-known/oauth-authorization-server/tenant",
                "/.well-known/openid-configuration/tenant"
            ]
        );
        let reg = &m.registrations[0];
        assert_eq!(reg["application_type"], "native");
        assert_eq!(reg["token_endpoint_auth_method"], "none");
        assert_eq!(reg["redirect_uris"], json!([REDIRECT]));
        assert_eq!(
            reg["grant_types"],
            json!(["authorization_code", "refresh_token"])
        );
    }

    // A forged issuer is refused and the flow is gone afterwards.
    let mut forged = s.browse(&auth).await;
    let state = forged.state.clone();
    forged.iss = Some("https://evil.example".into());
    let err = s.manager.complete(forged).await.unwrap_err();
    assert!(err.to_string().contains("expected"), "{err}");
    let err = s
        .manager
        .complete(CallbackParams {
            state,
            code: Some("x".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown or expired"), "{err}");

    s.login().await;
    assert_eq!(
        s.mock.lock().unwrap().registrations.len(),
        1,
        "the registration is reused"
    );
    let st = s.status();
    assert_eq!(st.status, "connected", "{st:?}");
    let login = s.manager.status(&s.url).unwrap();
    assert_eq!(login.kind, "oauth");
    assert!(login.refreshable);
    assert!(login.issuer.unwrap().ends_with("/tenant"));
    let tools = s.handle.tools();
    let (is_error, parts) = call(&tools, "mcp__t__whoami", json!({}), ctx()).await;
    assert!(!is_error);
    let first_token = only_text(&parts);
    assert!(first_token.starts_with("token at-"), "{first_token}");

    // Tokens are private files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = s.dir.join("oauth");
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(&dir).unwrap() {
            let meta = entry.unwrap().metadata().unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    // The server forgets every access token: the 401 triggers a refresh
    // whose rotated refresh token replaces the old one.
    let old_refresh: Vec<String> = s.mock.lock().unwrap().refresh.keys().cloned().collect();
    s.mock.lock().unwrap().access.clear();
    let (is_error, parts) = call(&tools, "mcp__t__whoami", json!({}), ctx()).await;
    assert!(!is_error, "{parts:?}");
    assert_ne!(only_text(&parts), first_token);
    {
        let m = s.mock.lock().unwrap();
        assert_eq!(m.refreshes, 1);
        assert!(old_refresh.iter().all(|t| m.used_refresh.contains(t)));
        assert!(!m.refresh.keys().any(|t| old_refresh.contains(t)));
    }

    // Short-lived tokens are refreshed before they are sent.
    s.mock.lock().unwrap().ttl = 5;
    s.mock.lock().unwrap().access.clear();
    let _ = call(&tools, "mcp__t__whoami", json!({}), ctx()).await;
    let _ = call(&tools, "mcp__t__whoami", json!({}), ctx()).await;
    {
        let mut m = s.mock.lock().unwrap();
        assert!(m.refreshes >= 3, "{}", m.refreshes);
        assert_eq!(m.used_refresh.len() as u32, m.refreshes);
        m.ttl = 3600;
    }

    // 403 insufficient_scope asks for a new login with the wider scope.
    let (is_error, parts) = call(&tools, "mcp__t__admin", json!({}), ctx()).await;
    assert!(is_error);
    assert_eq!(
        only_text(&parts),
        "Error: Insufficient scope: log in to this MCP server again to grant \"mcp:read mcp:admin\""
    );
    let st = s.status();
    assert_eq!(st.status, "needs-login");
    assert_eq!(
        st.challenge.unwrap().scope.as_deref(),
        Some("mcp:read mcp:admin")
    );
    let auth = s.start_login().await.unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert_eq!(q["scope"], "mcp:read mcp:admin");
    let params = s.browse(&auth).await;
    s.manager.complete(params).await.unwrap();
    s.handle
        .restart("t", Some(Duration::from_secs(10)))
        .await
        .unwrap();
    let tools = s.handle.tools();
    let (is_error, parts) = call(&tools, "mcp__t__admin", json!({}), ctx()).await;
    assert!(!is_error, "{parts:?}");
    assert_eq!(only_text(&parts), "admin ok");
    assert_eq!(s.status().status, "connected");

    // Logout revokes and forgets; the server needs a login again.
    assert!(s.manager.logout(&s.url).await.unwrap());
    assert_eq!(s.mock.lock().unwrap().revoked.len(), 2);
    assert!(s.manager.status(&s.url).is_none());
    s.handle
        .restart("t", Some(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(s.status().status, "needs-login");
    s.handle.shutdown().await;
}

#[tokio::test]
async fn well_known_fallback_and_manual_tokens() {
    let s = setup(|m| m.no_hint = true).await;
    s.handle
        .apply(&s.servers, Some(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(s.status().status, "needs-login");
    assert!(s.status().challenge.unwrap().resource_metadata.is_none());
    // Without the hint the path-aware well-known URI is found.
    s.login().await;
    assert_eq!(s.status().status, "connected");

    // A hand-entered token wins over the login and survives restarts.
    let issued = s.mock.lock().unwrap().tokens("mcp:read");
    let manual = issued["access_token"].as_str().unwrap().to_owned();
    s.manager.set_token(&s.url, Some(&manual)).unwrap();
    assert_eq!(s.manager.status(&s.url).unwrap().kind, "token");
    let (_, parts) = call(&s.handle.tools(), "mcp__t__whoami", json!({}), ctx()).await;
    assert_eq!(only_text(&parts), format!("token {manual}"));
    s.handle.shutdown().await;
}

#[tokio::test]
async fn refuses_missing_pkce_and_foreign_issuers() {
    let s = setup(|m| m.no_pkce = true).await;
    let err = s.start_login().await.unwrap_err();
    assert!(err.to_string().contains("PKCE"), "{err}");

    let s = setup(|m| m.wrong_issuer = true).await;
    let err = s.start_login().await.unwrap_err();
    assert!(err.to_string().contains("names the issuer"), "{err}");
}

#[tokio::test]
async fn preregistered_clients_skip_registration() {
    let s = setup(|_| {}).await;
    s.mock
        .lock()
        .unwrap()
        .clients
        .insert("my-app".into(), vec![REDIRECT.into()]);
    let oauth = McpOAuthConfig {
        client_id: Some("my-app".into()),
        scopes: vec!["mcp:write".into()],
        ..Default::default()
    };
    let auth = s
        .manager
        .start(LoginRequest {
            server: "t",
            url: &s.url,
            oauth: &oauth,
            headers: &BTreeMap::new(),
            challenge: None,
            redirect_uri: REDIRECT,
        })
        .await
        .unwrap();
    let auth = url::Url::parse(&auth).unwrap();
    let q: HashMap<String, String> = auth.query_pairs().into_owned().collect();
    assert_eq!(q["client_id"], "my-app");
    assert_eq!(q["scope"], "mcp:write", "configured scopes win");
    assert!(s.mock.lock().unwrap().registrations.is_empty());
    let params = s.browse(&auth).await;
    s.manager.complete(params).await.unwrap();
    assert!(s.manager.status(&s.url).is_some());
}
