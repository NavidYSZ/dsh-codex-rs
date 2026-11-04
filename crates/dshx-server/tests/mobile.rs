//! The Aether app's server side against a fake backend: client tokens next
//! to the start token, the pairing routes, revocation that closes a
//! WebSocket, and the TLS listener with its pinned certificate.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::config::MobileConfig;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, MobileHost, NotFound, PromptRequest, StreamMessage, serve};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

/// A home folder and `[codex.mobile]` in memory.
struct Host {
    home: tempfile::TempDir,
    settings: Mutex<MobileConfig>,
}

impl MobileHost for Host {
    fn home(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }
    fn settings(&self) -> MobileConfig {
        self.settings.lock().unwrap().clone()
    }
    fn update_settings(&self, settings: MobileConfig) -> anyhow::Result<()> {
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }
    fn version(&self) -> String {
        "test".into()
    }
}

struct Fake {
    tx: broadcast::Sender<StreamMessage>,
    host: Arc<Host>,
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({ "version": "test" }))
    }
    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value> {
        Ok(patch)
    }
    async fn workspaces(&self) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn add_workspace(&self, path: String) -> anyhow::Result<Value> {
        Ok(json!({ "path": path }))
    }
    async fn list_sessions(&self, _cwd: Option<String>) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn create_session(&self, _cwd: String) -> anyhow::Result<String> {
        Ok("s1".into())
    }
    async fn session(&self, id: &str) -> anyhow::Result<Value> {
        Err(NotFound(id.into()).into())
    }
    async fn events_after(
        &self,
        _id: &str,
        _after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(vec![])
    }
    async fn prompt(&self, _id: &str, _request: PromptRequest) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_session(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn approve(&self, _call_id: &str, _decision: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn save_attachment(
        &self,
        data: Vec<u8>,
        media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        Ok(ImageAttachmentRef {
            attachment_id: "a1".into(),
            media_type,
            bytes: data.len() as u64,
            width: 1,
            height: 1,
            name: None,
            original_dimensions: None,
        })
    }
    async fn load_attachment(&self, _id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Ok((vec![1], "image/png".into()))
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
    fn mobile(&self) -> Option<Arc<dyn MobileHost>> {
        Some(self.host.clone())
    }
}

async fn start(settings: MobileConfig) -> (u16, Arc<Host>) {
    let host = Arc::new(Host {
        home: tempfile::tempdir().unwrap(),
        settings: Mutex::new(settings),
    });
    let fake = Arc::new(Fake {
        tx: broadcast::channel(16).0,
        host: host.clone(),
    });
    let (addr, _handle) = serve(fake, "secret".into(), 0).await.unwrap();
    (addr.port(), host)
}

fn off() -> MobileConfig {
    MobileConfig {
        enabled: false,
        port: 0,
        name: "Test Mac".into(),
    }
}

#[tokio::test]
async fn client_tokens_open_the_api_until_revoked() {
    let (port, host) = start(off()).await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");

    let r = http
        .post(url("/api/mobile/pair"))
        .bearer_auth("secret")
        .json(&json!({ "name": "  Navid's iPhone " }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let paired: Value = r.json().await.unwrap();
    let token = paired["token"].as_str().unwrap().to_owned();
    let id = paired["client"]["id"].as_str().unwrap().to_owned();
    assert!(token.starts_with("act_") && token.len() == 47);
    assert_eq!(paired["client"]["name"], "Navid's iPhone");
    assert_eq!(paired["client"]["pairedVia"], "ui");
    assert_eq!(paired["payload"]["v"], 1);
    assert_eq!(paired["payload"]["token"], token);
    assert_eq!(paired["payload"]["client"]["id"], id);
    assert_eq!(paired["payload"]["server"]["name"], "Test Mac");
    assert!(
        paired["payload"]["server"]["id"]
            .as_str()
            .unwrap()
            .starts_with("srv_")
    );
    assert_eq!(paired["payload"]["transports"], json!([]), "listener off");
    assert!(
        paired["link"]
            .as_str()
            .unwrap()
            .starts_with("aether://pair#")
    );
    assert!(paired["qrSvg"].as_str().unwrap().starts_with("<svg"));
    let text = std::fs::read_to_string(host.home.path().join("mobile/clients.json")).unwrap();
    assert!(!text.contains(&token), "only the hash is stored");

    // The client token works like the start token; /api/info names the server.
    let r = http
        .get(url("/api/info"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["server"]["id"], paired["payload"]["server"]["id"]);
    assert_eq!(
        info["server"]["hostname"],
        paired["payload"]["server"]["hostname"]
    );
    assert_eq!(
        info["mobile"],
        json!({ "enabled": false, "port": 0, "listening": false })
    );
    // …and the Host check still guards the loopback listener.
    let r = http
        .get(url("/api/info"))
        .bearer_auth(&token)
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // `current` marks the requesting client.
    let r: Value = http
        .get(url("/api/mobile"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["clients"][0]["id"], id);
    assert_eq!(r["clients"][0]["current"], true);
    assert!(r["clients"][0].get("tokenSha256").is_none());
    assert!(r.get("addresses").is_none() && r.get("certSha256").is_none());
    let r: Value = http
        .get(url("/api/mobile"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["clients"][0]["current"], false);
    assert!(
        r["clients"][0]["lastSeenAt"].is_u64(),
        "seen through its token"
    );

    let r = http
        .patch(url(&format!("/api/mobile/clients/{id}")))
        .bearer_auth("secret")
        .json(&json!({ "name": "Phone" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["name"], "Phone");
    let r = http
        .patch(url("/api/mobile/clients/cli_missing"))
        .bearer_auth("secret")
        .json(&json!({ "name": "Phone" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // A WebSocket opened with the client token closes on revocation.
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/stream?token={token}"))
            .await
            .unwrap();
    let r = http
        .delete(url(&format!("/api/mobile/clients/{id}")))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("the socket closes");
    match closed {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
            assert_eq!(frame.unwrap().reason, "revoked");
        }
        None | Some(Err(_)) => {}
        other => panic!("unexpected message {other:?}"),
    }
    let r = http
        .get(url("/api/info"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "revoked at once");
    let r = http
        .delete(url(&format!("/api/mobile/clients/{id}")))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let text = std::fs::read_to_string(host.home.path().join("mobile/clients.json")).unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        file["clients"],
        json!([]),
        "revoking drops the client from the file"
    );
}

/// Records the certificate the server presents and accepts it.
#[derive(Debug)]
struct Capture(Mutex<Option<Vec<u8>>>);

impl rustls::client::danger::ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(end_entity.to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A TLS connection to `port` whose leaf certificate is captured.
async fn tls_connect(
    port: u16,
) -> (
    tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    Vec<u8>,
) {
    let capture = Arc::new(Capture(Mutex::new(None)));
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(capture.clone())
    .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls_pki_types::ServerName::try_from("localhost").unwrap(),
            tcp,
        )
        .await
        .unwrap();
    let der = capture.0.lock().unwrap().clone().expect("a certificate");
    (stream, der)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn tls_listener_serves_the_api_with_a_pinned_certificate() {
    let (port, host) = start(off()).await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");

    let r = http
        .put(url("/api/mobile"))
        .bearer_auth("secret")
        .json(&json!({ "enabled": true, "port": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let mobile: Value = r.json().await.unwrap();
    assert_eq!(mobile["enabled"], true);
    let tls_port = u16::try_from(mobile["port"].as_u64().unwrap()).unwrap();
    assert_ne!(tls_port, 0);
    let fingerprint = mobile["certSha256"].as_str().unwrap().to_owned();
    assert_eq!(fingerprint.len(), 64);
    assert!(mobile["addresses"].as_array().is_some());
    assert!(host.settings().enabled, "written to the config");
    assert!(host.home.path().join("mobile/cert.pem").exists());
    let info: Value = http
        .get(url("/api/info"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        info["mobile"],
        json!({ "enabled": true, "port": tls_port, "listening": true })
    );

    // The payload now carries the https transport with the pin.
    let paired: Value = http
        .post(url("/api/mobile/pair"))
        .bearer_auth("secret")
        .json(&json!({ "transports": ["https", "http"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().unwrap().to_owned();
    let transports = paired["payload"]["transports"].as_array().unwrap();
    let https = transports.iter().find(|t| t["kind"] == "https");
    if let Some(https) = https {
        assert_eq!(https["certSha256"], fingerprint);
        assert!(https["urls"][0].as_str().unwrap().starts_with("https://"));
        assert!(
            https["urls"][0]
                .as_str()
                .unwrap()
                .ends_with(&format!(":{tls_port}"))
        );
    }
    let http_transport = transports.iter().find(|t| t["kind"] == "http").unwrap();
    assert_eq!(
        http_transport["urls"][0],
        format!("http://127.0.0.1:{port}")
    );

    // The leaf certificate is the one `certSha256` names.
    let (mut stream, der) = tls_connect(tls_port).await;
    assert_eq!(sha256_hex(&der), fingerprint);
    // A WebSocket upgrade works over TLS (same hyper service with upgrades).
    stream
        .write_all(
            format!(
                "GET /api/stream?token={token} HTTP/1.1\r\nHost: phone.local\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "response head");
        head.push(byte[0]);
    }
    assert!(
        head.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&head)
    );

    // HTTPS requests with the client token, with any Host header.
    let tls_http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let r = tls_http
        .get(format!("https://127.0.0.1:{tls_port}/api/info"))
        .bearer_auth(&token)
        .header("host", "phone.local")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap()["server"]["name"],
        "Test Mac"
    );
    let r = tls_http
        .get(format!("https://127.0.0.1:{tls_port}/api/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "still bearer-protected");
    let r = tls_http
        .get(format!("https://127.0.0.1:{tls_port}/preview/x/"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "previews are loopback-only");
    let r = tls_http
        .get(format!(
            "https://127.0.0.1:{tls_port}/api/oauth/callback?state=good"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "the OAuth callback is loopback-only");
    let r = tls_http
        .get(format!("https://127.0.0.1:{tls_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "the UI shell is served too");
    // The loopback listener keeps its Host check and its previews route.
    let r = http.get(url("/preview/x/")).send().await.unwrap();
    assert_eq!(r.status(), 404, "unknown grant, but the route exists");

    // A taken port is refused without changing the settings.
    let (other, other_host) = start(off()).await;
    let r = http
        .put(format!("http://127.0.0.1:{other}/api/mobile"))
        .bearer_auth("secret")
        .json(&json!({ "enabled": true, "port": tls_port }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let body: Value = r.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains(&tls_port.to_string())
    );
    assert!(!other_host.settings().enabled);

    // Switching off stops the listener.
    let r = http
        .put(url("/api/mobile"))
        .bearer_auth("secret")
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(!host.settings().enabled);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", tls_port))
            .await
            .is_err(),
        "nothing listens any more"
    );
}

#[tokio::test]
async fn listener_starts_with_the_server_when_enabled() {
    let (port, _host) = start(MobileConfig {
        enabled: true,
        port: 0,
        name: String::new(),
    })
    .await;
    let http = reqwest::Client::new();
    let info: Value = http
        .get(format!("http://127.0.0.1:{port}/api/info"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["mobile"]["listening"], true);
    assert!(!info["server"]["name"].as_str().unwrap().is_empty());
    let tls_port = u16::try_from(info["mobile"]["port"].as_u64().unwrap()).unwrap();
    let (_stream, der) = tls_connect(tls_port).await;
    assert!(der.len() > 100);
}
