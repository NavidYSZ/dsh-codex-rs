//! The pairing payload (docs/aether/SPEC.md §2.1): what a QR code or a link
//! carries to the phone. Secrets live only here and in the client list.
use serde::{Deserialize, Serialize};

use super::identity::ServerInfo;

/// A link is this prefix plus the payload's JSON, base64url without padding.
pub const LINK_PREFIX: &str = "aether://pair#";

/// Current payload version.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRef {
    pub id: String,
    pub name: String,
}

/// One way for the phone to reach the server, tried in payload order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Transport {
    /// The TLS listener; `cert_sha256` pins the self-signed certificate
    /// (absent: a real certificate, validated normally).
    Https {
        urls: Vec<String>,
        #[serde(rename = "certSha256", skip_serializing_if = "Option::is_none")]
        cert_sha256: Option<String>,
    },
    /// Plain HTTP to a loopback URL (the simulator, an SSH tunnel's local end).
    Http { urls: Vec<String> },
    /// An SSH tunnel the app opens itself to `remote_host:remote_port`.
    Ssh {
        host: String,
        port: u16,
        user: String,
        #[serde(rename = "hostKeySha256")]
        host_key_sha256: String,
        #[serde(rename = "privateKey")]
        private_key: String,
        #[serde(rename = "remoteHost")]
        remote_host: String,
        #[serde(rename = "remotePort")]
        remote_port: u16,
        #[serde(rename = "tokenCommand", skip_serializing_if = "Option::is_none")]
        token_command: Option<String>,
    },
    /// Through the accounts hub's relay.
    Hub {
        url: String,
        #[serde(rename = "serverId")]
        server_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u32,
    pub server: ServerInfo,
    pub client: ClientRef,
    /// Absent when only a `tokenCommand` is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub transports: Vec<Transport>,
}

impl Payload {
    /// `aether://pair#<base64url(JSON)>`, without padding.
    pub fn link(&self) -> String {
        use base64::Engine as _;
        let json = serde_json::to_vec(self).expect("a payload serializes");
        format!(
            "{LINK_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// Reads a link, the bare base64url text or the bare JSON.
    pub fn from_text(text: &str) -> anyhow::Result<Payload> {
        use base64::Engine as _;
        let text = text.trim();
        let body = text.strip_prefix(LINK_PREFIX).unwrap_or(text);
        if body.starts_with('{') {
            return Ok(serde_json::from_str(body)?);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.trim_end_matches('='))
            .map_err(|e| anyhow::anyhow!("not a pairing link: {e}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Payload {
        Payload {
            v: VERSION,
            server: ServerInfo {
                id: "srv_8f1c".into(),
                name: "MacBook Pro".into(),
                hostname: "mbp-480".into(),
                platform: "darwin".into(),
                version: "0.3.0-alpha.0".into(),
            },
            client: ClientRef {
                id: "cli_2b7e".into(),
                name: "iPhone".into(),
            },
            token: Some("act_abc".into()),
            transports: vec![
                Transport::Https {
                    urls: vec!["https://192.168.178.20:7443".into()],
                    cert_sha256: Some("9f".repeat(32)),
                },
                Transport::Http {
                    urls: vec!["http://127.0.0.1:3190".into()],
                },
                Transport::Ssh {
                    host: "130.162.241.44".into(),
                    port: 22,
                    user: "ubuntu".into(),
                    host_key_sha256: "SHA256:abc".into(),
                    private_key: "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".into(),
                    remote_host: "127.0.0.1".into(),
                    remote_port: 3080,
                    token_command: None,
                },
            ],
        }
    }

    #[test]
    fn link_round_trip_without_padding() {
        let payload = sample();
        let link = payload.link();
        assert!(link.starts_with("aether://pair#"));
        let body = &link[LINK_PREFIX.len()..];
        assert!(!body.contains('='), "no padding");
        assert!(
            body.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(Payload::from_text(&link).unwrap(), payload);
        assert_eq!(Payload::from_text(body).unwrap(), payload, "bare base64url");
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(Payload::from_text(&json).unwrap(), payload, "bare JSON");
        assert!(Payload::from_text("aether://pair#***").is_err());
    }

    #[test]
    fn wire_names_follow_the_spec() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["transports"][0]["kind"], "https");
        assert!(v["transports"][0]["certSha256"].is_string());
        assert_eq!(v["transports"][2]["kind"], "ssh");
        assert_eq!(v["transports"][2]["hostKeySha256"], "SHA256:abc");
        assert_eq!(v["transports"][2]["remotePort"], 3080);
        assert!(v["transports"][2].get("tokenCommand").is_none());
        let mut without_token = sample();
        without_token.token = None;
        let v = serde_json::to_value(without_token).unwrap();
        assert!(v.get("token").is_none());
    }
}
