//! The certificate of the Aether listener and the listener itself: a
//! self-signed ECDSA P-256 certificate in `$DSHX_HOME/mobile/`, rustls, and
//! an accept loop that hands every connection to the router.
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use rustls_pki_types::pem::PemObject as _;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use sha2::Digest as _;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// A client that connects and never finishes its handshake is dropped after
/// this; it holds one task, never the accept loop.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// The listener's certificate and key (PEM) and the fingerprint the app pins.
pub struct CertInfo {
    pub cert_pem: String,
    pub key_pem: String,
    /// Lowercase hex SHA-256 of the DER certificate.
    pub sha256: String,
}

/// Lowercase hex SHA-256 of a DER certificate.
pub fn cert_sha256(der: &[u8]) -> String {
    super::clients::hex(&sha2::Sha256::digest(der))
}

/// Reads `mobile/cert.pem` and `key.pem`, making them on first use (folder
/// 0700, files 0600): CN `Aether on <name>`, SANs the host name,
/// `localhost` and the current addresses, valid ten years. A certificate
/// that does not parse is replaced, which changes the fingerprint.
pub fn ensure_cert(home: &Path, name: &str) -> anyhow::Result<CertInfo> {
    let dir = home.join("mobile");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    if let (Ok(cert_pem), Ok(key_pem)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        match load(cert_pem, key_pem) {
            Ok(info) => return Ok(info),
            Err(e) => tracing::warn!("{}: {e:#}; making a new certificate", cert_path.display()),
        }
    }
    let (cert_pem, key_pem) = generate(name)?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    super::write_private(&cert_path, cert_pem.as_bytes())?;
    super::write_private(&key_path, key_pem.as_bytes())?;
    load(cert_pem, key_pem)
}

fn load(cert_pem: String, key_pem: String) -> anyhow::Result<CertInfo> {
    let der = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("cert.pem: {e}"))?;
    PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("key.pem: {e}"))?;
    Ok(CertInfo {
        sha256: cert_sha256(&der),
        cert_pem,
        key_pem,
    })
}

/// A fresh self-signed certificate and its key, both PEM.
fn generate(name: &str) -> anyhow::Result<(String, String)> {
    let mut sans = vec!["localhost".to_owned()];
    sans.extend(super::net::addresses().iter().map(|a| a.ip.to_string()));
    let hostname = super::identity::hostname();
    // A host name that is no valid DNS name is left out rather than failing.
    let mut params = match rcgen::CertificateParams::new([&[hostname][..], &sans].concat()) {
        Ok(params) => params,
        Err(_) => rcgen::CertificateParams::new(sans)?,
    };
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, format!("Aether on {name}"));
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(10 * 365 + 3);
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let cert = params.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// rustls with the ring provider, HTTP/1.1 only (WebSockets upgrade over it).
pub(crate) fn acceptor(cert: &CertInfo) -> anyhow::Result<TlsAcceptor> {
    let certs = vec![
        CertificateDer::from_pem_slice(cert.cert_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("cert.pem: {e}"))?,
    ];
    let key = PrivateKeyDer::from_pem_slice(cert.key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("key.pem: {e}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Binds `[::]:port` for IPv6 and IPv4 at once, else `0.0.0.0:port`; port 0
/// takes any free port. Fails when the port is taken.
pub(crate) fn bind(port: u16) -> std::io::Result<TcpListener> {
    // On macOS a dual-stack socket binds even while another program holds
    // 0.0.0.0:port, so IPv4 is probed first: a taken port is an error
    // everywhere.
    if port != 0 {
        drop(bind_socket(
            Domain::IPV4,
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        )?);
    }
    let socket = bind_socket(
        Domain::IPV6,
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
    )
    .or_else(|_| {
        bind_socket(
            Domain::IPV4,
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        )
    })?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

fn bind_socket(domain: Domain, addr: SocketAddr) -> std::io::Result<Socket> {
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    if domain == Domain::IPV6 {
        socket.set_only_v6(false)?;
    }
    // Rebinding right after a stop must not wait for TIME_WAIT connections.
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    Ok(socket)
}

/// Accepts connections until the task is aborted; every connection gets its
/// own task for the TLS handshake and the HTTP exchange (with upgrades, so
/// WebSockets work).
pub(crate) fn spawn_accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::warn!("aether: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                let tls =
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => tls,
                        Ok(Err(e)) => {
                            tracing::debug!("aether: TLS handshake with {peer} failed: {e}");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!("aether: TLS handshake with {peer} timed out");
                            return;
                        }
                    };
                let service = TowerToHyperService::new(app);
                let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                if let Err(e) = builder
                    .serve_connection_with_upgrades(TokioIo::new(tls), service)
                    .await
                {
                    tracing::debug!("aether: connection from {peer} ended: {e}");
                }
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_is_made_once_with_private_files() {
        let home = tempfile::tempdir().unwrap();
        let first = ensure_cert(home.path(), "Test Mac").unwrap();
        assert!(first.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(first.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(first.sha256.len(), 64);
        let again = ensure_cert(home.path(), "Other").unwrap();
        assert_eq!(again.sha256, first.sha256, "kept, not regenerated");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for file in ["cert.pem", "key.pem"] {
                let mode = std::fs::metadata(home.path().join("mobile").join(file))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{file}");
            }
        }
        acceptor(&first).unwrap();
        std::fs::write(home.path().join("mobile").join("cert.pem"), "garbage").unwrap();
        let replaced = ensure_cert(home.path(), "Test Mac").unwrap();
        assert_ne!(replaced.sha256, first.sha256, "a broken file is replaced");
    }

    #[tokio::test]
    async fn bind_takes_any_port_and_refuses_a_taken_one() {
        let first = bind(0).unwrap();
        let port = first.local_addr().unwrap().port();
        assert_ne!(port, 0);
        let taken = bind(port);
        assert!(taken.is_err(), "the port is in use");
        drop(first);
    }
}
