//! OAuth for Streamable HTTP servers, following the MCP authorization
//! specification:
//!
//! 1. A 401 carries `WWW-Authenticate: Bearer resource_metadata=...`
//!    (otherwise the RFC 9728 well-known URIs are tried).
//! 2. The Protected Resource Metadata names the authorization server; its
//!    metadata comes from RFC 8414 or OIDC Discovery in the specification's
//!    order, and its `issuer` must match.
//! 3. PKCE with S256 is required; the client is the one configured in
//!    `[mcp_servers.<name>.oauth]` or registered dynamically (RFC 7591).
//! 4. The browser is sent to the authorization endpoint with `resource`
//!    (RFC 8707) and `state`; the callback checks `state` and `iss`
//!    (RFC 9207) before the code is exchanged.
//! 5. Tokens are stored per issuer (0600) and refreshed shortly before they
//!    expire or after a 401; a rotated refresh token replaces the old one.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use dshx_core::config::McpOAuthConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

mod discovery;
mod store;

pub use discovery::{AuthChallenge, parse_www_authenticate};

use discovery::{
    ServerMetadata, canonical_resource, is_secure, resource_allowed, resource_metadata_urls,
    same_issuer, server_metadata_urls,
};
use store::{ClientRecord, Store, TokenRecord};

/// How long a started login waits for its callback.
const PENDING_TTL: Duration = Duration::from_secs(600);
/// Tokens this close to expiry are refreshed before use.
const REFRESH_MARGIN_MS: u64 = 60_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_NAME: &str = "dsh-codex";

/// Supplies the bearer token of one server's requests.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// The token to send, refreshed first when it is about to expire;
    /// `None` without a login.
    async fn token(&self) -> Option<String>;
    /// The server rejected `rejected` (401): a fresh token to retry with.
    async fn refresh(&self, rejected: &str) -> Option<String>;
}

/// A bearer token from an environment variable (`bearer_token_env_var`).
pub(crate) struct EnvBearer(pub String);

#[async_trait]
impl TokenProvider for EnvBearer {
    async fn token(&self) -> Option<String> {
        std::env::var(&self.0).ok().filter(|v| !v.is_empty())
    }

    async fn refresh(&self, _rejected: &str) -> Option<String> {
        None
    }
}

/// Login state of one server for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    /// `oauth` (tokens from a login) or `token` (entered by hand).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Milliseconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub refreshable: bool,
}

/// Query parameters of the redirect back from the authorization server.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub iss: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// What a login needs to know about the server.
pub struct LoginRequest<'a> {
    /// Server name, handed back by [`OAuthManager::complete`].
    pub server: &'a str,
    pub url: &'a url::Url,
    pub oauth: &'a McpOAuthConfig,
    /// The server's configured headers, sent with the probe request.
    pub headers: &'a BTreeMap<String, String>,
    /// The challenge of the server's last 401/403, if any.
    pub challenge: Option<&'a AuthChallenge>,
    /// Loopback (or HTTPS) callback URL of this process.
    pub redirect_uri: &'a str,
}

/// A finished login.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginDone {
    pub server: String,
    pub server_url: String,
}

struct PendingFlow {
    server: String,
    server_url: String,
    resource: String,
    issuer: String,
    iss_required: bool,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    client: ClientRecord,
    verifier: String,
    redirect_uri: String,
    scope: Option<String>,
    started: Instant,
}

struct TokenFailure {
    message: String,
    /// The grant or client is no longer valid; the stored tokens are useless.
    permanent: bool,
}

/// Logins, token storage and refreshes for every HTTP server of a process.
pub struct OAuthManager {
    store: Store,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, PendingFlow>>,
    refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Key of a server in the token store.
pub fn server_key(url: &url::Url) -> String {
    canonical_resource(url)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("system random source");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn now_ms() -> u64 {
    dshx_core::now_ms()
}

fn is_loopback(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    )
}

impl OAuthManager {
    /// Tokens live in `dir` (normally `$DSHX_HOME/oauth`).
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client");
        Arc::new(OAuthManager {
            store: Store::new(dir.into()),
            http,
            pending: Mutex::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        self.store.dir()
    }

    /// The token source for requests to `url`.
    pub fn provider(self: &Arc<Self>, url: &url::Url) -> Arc<dyn TokenProvider> {
        Arc::new(OAuthTokens {
            manager: self.clone(),
            key: server_key(url),
        })
    }

    /// Whether a login or a hand-entered token exists for `url`.
    pub fn status(&self, url: &url::Url) -> Option<LoginStatus> {
        let key = server_key(url);
        if self.store.manual_token(&key).is_some() {
            return Some(LoginStatus {
                kind: "token",
                issuer: None,
                scope: None,
                expires_at: None,
                refreshable: false,
            });
        }
        self.store.tokens(&key).map(|t| LoginStatus {
            kind: "oauth",
            issuer: Some(t.issuer),
            scope: t.scope,
            expires_at: t.expires_at,
            refreshable: t.refresh_token.is_some(),
        })
    }

    /// Stores (or with `None` removes) a hand-entered bearer token, e.g. a
    /// personal access token for a server without dynamic registration.
    pub fn set_token(&self, url: &url::Url, token: Option<&str>) -> anyhow::Result<()> {
        let token = token.map(str::trim).filter(|t| !t.is_empty());
        self.store.set_manual_token(&server_key(url), token)?;
        Ok(())
    }

    /// Forgets the server's tokens (and revokes them where the server
    /// offers revocation). Its client registration stays.
    pub async fn logout(&self, url: &url::Url) -> anyhow::Result<bool> {
        let key = server_key(url);
        let manual = self.store.manual_token(&key).is_some();
        if manual {
            self.store.set_manual_token(&key, None)?;
        }
        let Some(record) = self.store.remove_tokens(&key)? else {
            return Ok(manual);
        };
        if let Some(endpoint) = &record.revocation_endpoint {
            let client = client_of(&record);
            for token in record.refresh_token.iter().chain([&record.access_token]) {
                let form = vec![("token", token.clone())];
                if let Err(e) = self.token_request(endpoint, &client, form).await {
                    tracing::debug!("token revocation at {endpoint} failed: {}", e.message);
                }
            }
        }
        Ok(true)
    }

    /// Starts a login and returns the authorization URL to open.
    pub async fn start(&self, req: LoginRequest<'_>) -> anyhow::Result<String> {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, f| f.started.elapsed() < PENDING_TTL);
        let redirect = url::Url::parse(req.redirect_uri)?;
        anyhow::ensure!(
            is_secure(&redirect),
            "the redirect URI must be https or loopback http"
        );
        let challenge = match req.challenge {
            Some(c) => c.clone(),
            None => self.probe(req.url, req.headers).await,
        };
        let (resource, issuer, prm_scopes, legacy) = self
            .protected_resource(req.url, challenge.resource_metadata.as_deref())
            .await?;
        let metadata = self.server_metadata(&issuer, legacy).await?;
        anyhow::ensure!(
            metadata.code_challenge_methods.iter().any(|m| m == "S256"),
            "the authorization server {} does not support PKCE with S256; refusing to log in",
            metadata.issuer
        );
        let scope = if !req.oauth.scopes.is_empty() {
            Some(req.oauth.scopes.join(" "))
        } else {
            challenge
                .scope
                .clone()
                .or_else(|| (!prm_scopes.is_empty()).then(|| prm_scopes.join(" ")))
        };
        let client = self
            .client(&metadata, req.oauth, req.redirect_uri, scope.as_deref())
            .await?;
        let verifier = random_token();
        let state = random_token();
        let mut auth = url::Url::parse(&metadata.authorization_endpoint)?;
        {
            let mut q = auth.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &client.client_id)
                .append_pair("redirect_uri", req.redirect_uri);
            if let Some(scope) = &scope {
                q.append_pair("scope", scope);
            }
            q.append_pair("state", &state)
                .append_pair("code_challenge", &pkce_challenge(&verifier))
                .append_pair("code_challenge_method", "S256")
                .append_pair("resource", &resource);
        }
        self.pending.lock().unwrap().insert(
            state,
            PendingFlow {
                server: req.server.to_owned(),
                server_url: server_key(req.url),
                resource,
                issuer: metadata.issuer.clone(),
                iss_required: metadata.iss_parameter_supported,
                token_endpoint: metadata.token_endpoint.clone(),
                revocation_endpoint: metadata.revocation_endpoint.clone(),
                client,
                verifier,
                redirect_uri: req.redirect_uri.to_owned(),
                scope,
                started: Instant::now(),
            },
        );
        Ok(auth.to_string())
    }

    /// Finishes a login from the callback parameters: checks `state` and
    /// `iss`, exchanges the code and stores the tokens.
    pub async fn complete(&self, params: CallbackParams) -> anyhow::Result<LoginDone> {
        let state = params
            .state
            .ok_or_else(|| anyhow::anyhow!("the callback lacks the state parameter"))?;
        let flow = self
            .pending
            .lock()
            .unwrap()
            .remove(&state)
            .filter(|f| f.started.elapsed() < PENDING_TTL)
            .ok_or_else(|| anyhow::anyhow!("unknown or expired login; start it again"))?;
        if let Some(error) = params.error {
            let detail = params
                .error_description
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            anyhow::bail!("the authorization server refused the login ({error}{detail})");
        }
        match &params.iss {
            Some(iss) => anyhow::ensure!(
                same_issuer(iss, &flow.issuer),
                "the authorization response came from {iss}, expected {}",
                flow.issuer
            ),
            None => anyhow::ensure!(
                !flow.iss_required,
                "the authorization response lacks the iss parameter its server announced"
            ),
        }
        let code = params
            .code
            .ok_or_else(|| anyhow::anyhow!("the callback lacks the authorization code"))?;
        let form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", flow.redirect_uri.clone()),
            ("code_verifier", flow.verifier.clone()),
            ("resource", flow.resource.clone()),
        ];
        let tokens = self
            .token_request(&flow.token_endpoint, &flow.client, form)
            .await
            .map_err(|e| anyhow::anyhow!("token exchange failed: {}", e.message))?;
        let record = token_record(
            &tokens,
            TokenRecord {
                access_token: String::new(),
                refresh_token: None,
                token_type: "Bearer".into(),
                expires_at: None,
                scope: flow.scope.clone(),
                resource: flow.resource.clone(),
                issuer: flow.issuer.clone(),
                token_endpoint: flow.token_endpoint.clone(),
                revocation_endpoint: flow.revocation_endpoint.clone(),
                client_id: flow.client.client_id.clone(),
                client_secret: flow.client.client_secret.clone(),
                auth_method: flow.client.auth_method.clone(),
                obtained_at: now_ms(),
            },
        )?;
        self.store.put_tokens(&flow.server_url, record)?;
        Ok(LoginDone {
            server: flow.server,
            server_url: flow.server_url,
        })
    }

    /// Unauthenticated `initialize` to learn the server's challenge.
    async fn probe(&self, url: &url::Url, headers: &BTreeMap<String, String>) -> AuthChallenge {
        let mut request = self
            .http
            .post(url.clone())
            .header("accept", "application/json, text/event-stream")
            .json(
                &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION")}}}),
            );
        for (k, v) in headers {
            request = request.header(k.as_str(), v.as_str());
        }
        match request.send().await {
            Ok(r) if matches!(r.status().as_u16(), 401 | 403) => parse_www_authenticate(
                r.status().as_u16(),
                r.headers()
                    .get_all("www-authenticate")
                    .iter()
                    .filter_map(|v| v.to_str().ok()),
            ),
            _ => AuthChallenge::default(),
        }
    }

    async fn get_json(&self, url: &str) -> Option<Map<String, Value>> {
        let parsed = url::Url::parse(url).ok().filter(is_secure)?;
        let response = self
            .http
            .get(parsed)
            .header("accept", "application/json")
            .header("mcp-protocol-version", crate::protocol::PROTOCOL_VERSION)
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(Value::Object(map)) => Some(map),
                _ => None,
            },
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("metadata request {url} failed: {e}");
                None
            }
        }
    }

    /// RFC 9728: `(resource, issuer, scopes_supported, legacy)`. Servers
    /// without metadata (MCP 2025-03-26) use their origin as the
    /// authorization server (`legacy`).
    async fn protected_resource(
        &self,
        server: &url::Url,
        hint: Option<&str>,
    ) -> anyhow::Result<(String, String, Vec<String>, bool)> {
        for location in resource_metadata_urls(server, hint) {
            let Some(data) = self.get_json(&location).await else {
                continue;
            };
            let resource = match data.get("resource").and_then(Value::as_str) {
                Some(r) => {
                    let parsed = url::Url::parse(r).map_err(|e| {
                        anyhow::anyhow!("invalid resource {r:?} in {location}: {e}")
                    })?;
                    anyhow::ensure!(
                        resource_allowed(server, &parsed),
                        "the protected resource metadata at {location} names {r}, which does not cover {server}"
                    );
                    r.to_owned()
                }
                None => canonical_resource(server),
            };
            let issuer = data
                .get("authorization_servers")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find_map(Value::as_str))
                .ok_or_else(|| {
                    anyhow::anyhow!("the protected resource metadata at {location} names no authorization server")
                })?
                .to_owned();
            let scopes = data
                .get("scopes_supported")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            return Ok((resource, issuer, scopes, false));
        }
        Ok((
            canonical_resource(server),
            server.origin().ascii_serialization(),
            Vec::new(),
            true,
        ))
    }

    async fn server_metadata(&self, issuer: &str, legacy: bool) -> anyhow::Result<ServerMetadata> {
        let issuer_url = url::Url::parse(issuer)
            .map_err(|e| anyhow::anyhow!("invalid authorization server {issuer:?}: {e}"))?;
        anyhow::ensure!(
            is_secure(&issuer_url),
            "refusing the non-HTTPS authorization server {issuer}"
        );
        for location in server_metadata_urls(&issuer_url) {
            let Some(data) = self.get_json(&location).await else {
                continue;
            };
            let metadata =
                ServerMetadata::from_json(&data).map_err(|e| anyhow::anyhow!("{location}: {e}"))?;
            anyhow::ensure!(
                same_issuer(&metadata.issuer, issuer),
                "the metadata at {location} names the issuer {}, expected {issuer}",
                metadata.issuer
            );
            return Ok(metadata);
        }
        anyhow::ensure!(
            legacy,
            "no authorization server metadata found for {issuer}"
        );
        let origin = issuer.trim_end_matches('/');
        Ok(ServerMetadata {
            issuer: origin.to_owned(),
            authorization_endpoint: format!("{origin}/authorize"),
            token_endpoint: format!("{origin}/token"),
            registration_endpoint: Some(format!("{origin}/register")),
            revocation_endpoint: None,
            code_challenge_methods: vec!["S256".into()],
            token_auth_methods: Vec::new(),
            iss_parameter_supported: false,
            scopes_supported: Vec::new(),
        })
    }

    /// The configured client, a stored registration for this redirect URI,
    /// or a new dynamic registration (RFC 7591).
    async fn client(
        &self,
        metadata: &ServerMetadata,
        oauth: &McpOAuthConfig,
        redirect_uri: &str,
        scope: Option<&str>,
    ) -> anyhow::Result<ClientRecord> {
        if let Some(client_id) = &oauth.client_id {
            let auth_method = match &oauth.client_secret {
                None => "none",
                Some(_)
                    if metadata.token_auth_methods.is_empty()
                        || metadata
                            .token_auth_methods
                            .iter()
                            .any(|m| m == "client_secret_basic") =>
                {
                    "client_secret_basic"
                }
                Some(_) => "client_secret_post",
            };
            return Ok(ClientRecord {
                redirect_uri: redirect_uri.to_owned(),
                client_id: client_id.clone(),
                client_secret: oauth.client_secret.clone(),
                auth_method: auth_method.to_owned(),
                registered_at: 0,
            });
        }
        if let Some(client) = self.store.client(&metadata.issuer, redirect_uri) {
            return Ok(client);
        }
        let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} offers no dynamic client registration: register an OAuth app there and set client_id in [mcp_servers.<name>.oauth], or enter an access token",
                metadata.issuer
            )
        })?;
        let redirect = url::Url::parse(redirect_uri)?;
        let mut body = json!({
            "client_name": CLIENT_NAME,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": if is_loopback(&redirect) { "native" } else { "web" },
        });
        if let Some(scope) = scope {
            body["scope"] = Value::String(scope.to_owned());
        }
        let response = self
            .http
            .post(endpoint)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("client registration at {endpoint} failed: {e}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::ensure!(
            status.is_success(),
            "client registration at {endpoint} failed: HTTP {status}: {}",
            text.trim()
        );
        let data: Map<String, Value> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid registration response: {e}"))?;
        let client_id = data
            .get("client_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("the registration response lacks client_id"))?;
        let client_secret = data
            .get("client_secret")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let auth_method = data
            .get("token_endpoint_auth_method")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if client_secret.is_some() {
                    "client_secret_basic"
                } else {
                    "none"
                }
                .to_owned()
            });
        let client = ClientRecord {
            redirect_uri: redirect_uri.to_owned(),
            client_id: client_id.to_owned(),
            client_secret,
            auth_method,
            registered_at: now_ms(),
        };
        self.store.put_client(&metadata.issuer, client.clone())?;
        Ok(client)
    }

    /// A form POST to the token (or revocation) endpoint with client
    /// authentication.
    async fn token_request(
        &self,
        endpoint: &str,
        client: &ClientRecord,
        mut form: Vec<(&str, String)>,
    ) -> Result<Map<String, Value>, TokenFailure> {
        let failure = |message: String| TokenFailure {
            message,
            permanent: false,
        };
        let mut request = self
            .http
            .post(endpoint)
            .header("accept", "application/json");
        match (client.auth_method.as_str(), &client.client_secret) {
            ("client_secret_basic", Some(secret)) => {
                let enc = |s: &str| {
                    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                };
                request = request.basic_auth(enc(&client.client_id), Some(enc(secret)));
            }
            ("client_secret_post", Some(secret)) => {
                form.push(("client_id", client.client_id.clone()));
                form.push(("client_secret", secret.clone()));
            }
            _ => form.push(("client_id", client.client_id.clone())),
        }
        let response = request
            .form(&form)
            .send()
            .await
            .map_err(|e| failure(e.to_string()))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let data: Map<String, Value> = match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => map,
            _ if status.is_success() && text.trim().is_empty() => Map::new(),
            _ => {
                return Err(failure(format!("HTTP {status}: {}", text.trim())));
            }
        };
        if !status.is_success() {
            let code = data.get("error").and_then(Value::as_str).unwrap_or("error");
            let detail = data
                .get("error_description")
                .and_then(Value::as_str)
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            return Err(TokenFailure {
                message: format!("{code}{detail}"),
                permanent: matches!(
                    code,
                    "invalid_grant" | "invalid_client" | "unauthorized_client"
                ),
            });
        }
        Ok(data)
    }

    fn refresh_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks
            .lock()
            .unwrap()
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    /// Current token of `key`, refreshed when expiring.
    async fn token(&self, key: &str) -> Option<String> {
        if let Some(manual) = self.store.manual_token(key) {
            return Some(manual);
        }
        let record = self.store.tokens(key)?;
        if !expiring(&record) || record.refresh_token.is_none() {
            return Some(record.access_token);
        }
        match self.refresh(key, &record.access_token).await {
            Some(token) => Some(token),
            // Still usable for a moment; the server decides.
            None if record.expires_at.is_some_and(|e| e > now_ms()) => Some(record.access_token),
            None => None,
        }
    }

    /// Refreshes `key`'s tokens unless someone already replaced `rejected`.
    async fn refresh(&self, key: &str, rejected: &str) -> Option<String> {
        let lock = self.refresh_lock(key);
        let _guard = lock.lock().await;
        if let Some(manual) = self.store.manual_token(key) {
            return (manual != rejected).then_some(manual);
        }
        let record = self.store.tokens(key)?;
        if record.access_token != rejected && !expiring(&record) {
            return Some(record.access_token);
        }
        let refresh_token = record.refresh_token.clone()?;
        let form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token),
            ("resource", record.resource.clone()),
        ];
        match self
            .token_request(&record.token_endpoint, &client_of(&record), form)
            .await
        {
            Ok(data) => {
                let base = TokenRecord {
                    obtained_at: now_ms(),
                    ..record
                };
                let fresh = token_record(&data, base).ok()?;
                let token = fresh.access_token.clone();
                if let Err(e) = self.store.put_tokens(key, fresh) {
                    tracing::warn!("storing refreshed tokens failed: {e}");
                }
                Some(token)
            }
            Err(e) if e.permanent => {
                tracing::warn!(
                    "token refresh for {key} rejected ({}); login required",
                    e.message
                );
                let _ = self.store.remove_tokens(key);
                None
            }
            Err(e) => {
                tracing::warn!("token refresh for {key} failed: {}", e.message);
                None
            }
        }
    }
}

fn expiring(record: &TokenRecord) -> bool {
    record
        .expires_at
        .is_some_and(|e| e <= now_ms() + REFRESH_MARGIN_MS)
}

fn client_of(record: &TokenRecord) -> ClientRecord {
    ClientRecord {
        redirect_uri: String::new(),
        client_id: record.client_id.clone(),
        client_secret: record.client_secret.clone(),
        auth_method: record.auth_method.clone(),
        registered_at: 0,
    }
}

/// Applies a token response to `base`: a new refresh token replaces the
/// old one (rotation), a missing one keeps it.
fn token_record(data: &Map<String, Value>, base: TokenRecord) -> anyhow::Result<TokenRecord> {
    let access_token = data
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the token response lacks access_token"))?;
    let token_type = data
        .get("token_type")
        .and_then(Value::as_str)
        .unwrap_or("Bearer");
    anyhow::ensure!(
        token_type.eq_ignore_ascii_case("bearer"),
        "unsupported token type {token_type}"
    );
    let expires_at = data
        .get("expires_in")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .map(|secs| now_ms() + secs * 1000);
    Ok(TokenRecord {
        access_token: access_token.to_owned(),
        refresh_token: data
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(base.refresh_token),
        token_type: "Bearer".into(),
        expires_at,
        scope: data
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(base.scope),
        ..base
    })
}

struct OAuthTokens {
    manager: Arc<OAuthManager>,
    key: String,
}

#[async_trait]
impl TokenProvider for OAuthTokens {
    async fn token(&self) -> Option<String> {
        self.manager.token(&self.key).await
    }

    async fn refresh(&self, rejected: &str) -> Option<String> {
        self.manager.refresh(&self.key, rejected).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random_token();
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_token());
    }

    #[test]
    fn rotation_keeps_or_replaces_the_refresh_token() {
        let base = TokenRecord {
            access_token: "old".into(),
            refresh_token: Some("r1".into()),
            token_type: "Bearer".into(),
            expires_at: None,
            scope: Some("read".into()),
            resource: "https://x/mcp".into(),
            issuer: "https://x".into(),
            token_endpoint: "https://x/token".into(),
            revocation_endpoint: None,
            client_id: "c".into(),
            client_secret: None,
            auth_method: "none".into(),
            obtained_at: 0,
        };
        let kept = token_record(
            json!({"access_token": "a2", "token_type": "bearer", "expires_in": 60})
                .as_object()
                .unwrap(),
            base.clone(),
        )
        .unwrap();
        assert_eq!(kept.refresh_token.as_deref(), Some("r1"));
        assert!(kept.expires_at.is_some());
        let rotated = token_record(
            json!({"access_token": "a3", "refresh_token": "r2"})
                .as_object()
                .unwrap(),
            base.clone(),
        )
        .unwrap();
        assert_eq!(rotated.refresh_token.as_deref(), Some("r2"));
        assert!(
            token_record(
                json!({"access_token": "x", "token_type": "DPoP"})
                    .as_object()
                    .unwrap(),
                base
            )
            .is_err()
        );
    }
}
