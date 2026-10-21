//! Authorization discovery (MCP authorization, RFC 9728, RFC 8414, OIDC
//! Discovery): the `WWW-Authenticate` challenge, Protected Resource
//! Metadata, and the authorization server's metadata.
use serde::Serialize;
use serde_json::{Map, Value};

/// A Bearer challenge from a 401 or 403 answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthChallenge {
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

impl AuthChallenge {
    /// 403 `insufficient_scope`: the token lacks a scope (step-up authorization).
    pub fn insufficient_scope(&self) -> bool {
        self.status == 403 && self.error.as_deref() == Some("insufficient_scope")
    }
}

/// Parses `WWW-Authenticate` values and keeps the parameters of the Bearer
/// challenge (RFC 9110 auth-params, quoted or token values).
pub fn parse_www_authenticate<'a>(
    status: u16,
    values: impl IntoIterator<Item = &'a str>,
) -> AuthChallenge {
    let mut challenge = AuthChallenge {
        status,
        ..Default::default()
    };
    for value in values {
        for (scheme, params) in challenges(value) {
            if !scheme.eq_ignore_ascii_case("bearer") {
                continue;
            }
            for (key, val) in params {
                let slot = match key.to_ascii_lowercase().as_str() {
                    "resource_metadata" => &mut challenge.resource_metadata,
                    "scope" => &mut challenge.scope,
                    "error" => &mut challenge.error,
                    "error_description" => &mut challenge.error_description,
                    _ => continue,
                };
                *slot = Some(val);
            }
        }
    }
    challenge
}

/// Splits one header value into `(scheme, params)` challenges.
fn challenges(value: &str) -> Vec<(String, Vec<(String, String)>)> {
    let b = value.as_bytes();
    let mut i = 0;
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let skip = |i: &mut usize, extra: u8| {
        while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == extra) {
            *i += 1;
        }
    };
    while i < b.len() {
        skip(&mut i, b',');
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b',' && b[i] != b'=' {
            i += 1;
        }
        let token = value[start..i].to_owned();
        if token.is_empty() {
            i += 1;
            continue;
        }
        skip(&mut i, b' ');
        let is_param = i < b.len() && b[i] == b'=' && !out.is_empty();
        if !is_param {
            out.push((token, Vec::new()));
            continue;
        }
        i += 1;
        skip(&mut i, b' ');
        let mut val = String::new();
        if i < b.len() && b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let ch = value[i..].chars().next().unwrap_or(' ');
                val.push(ch);
                i += ch.len_utf8();
            }
            i += 1;
        } else {
            let s = i;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            val = value[s..i].trim().to_owned();
        }
        if let Some((_, params)) = out.last_mut() {
            params.push((token, val));
        }
    }
    out
}

/// Plain `http://` is only allowed for loopback hosts.
pub(crate) fn is_secure(url: &url::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        ),
        _ => false,
    }
}

/// Issuers compare exactly, except for a trailing slash.
pub(crate) fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Protected Resource Metadata locations: the challenge's
/// `resource_metadata`, then the path-aware and the root well-known URI.
pub(crate) fn resource_metadata_urls(server: &url::Url, hint: Option<&str>) -> Vec<String> {
    let origin = server.origin().ascii_serialization();
    let mut urls = Vec::new();
    if let Some(h) = hint {
        urls.push(h.to_owned());
    }
    let path = server.path();
    if !path.is_empty() && path != "/" {
        urls.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    urls.push(format!("{origin}/.well-known/oauth-protected-resource"));
    urls.dedup();
    urls
}

/// Authorization server metadata locations in the order of the MCP
/// specification: RFC 8414 and OIDC with path insertion, then OIDC with
/// the path appended (issuers without a path try the two root URIs).
pub(crate) fn server_metadata_urls(issuer: &url::Url) -> Vec<String> {
    let origin = issuer.origin().ascii_serialization();
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// Whether the resource a server's metadata names covers the server URL
/// (same origin, the server path under the resource path).
pub(crate) fn resource_allowed(server: &url::Url, resource: &url::Url) -> bool {
    if server.origin() != resource.origin() {
        return false;
    }
    let slash = |p: &str| {
        if p.ends_with('/') {
            p.to_owned()
        } else {
            format!("{p}/")
        }
    };
    slash(server.path()).starts_with(&slash(resource.path()))
}

/// The server URL as a resource indicator (RFC 8707): no fragment.
pub(crate) fn canonical_resource(server: &url::Url) -> String {
    let mut u = server.clone();
    u.set_fragment(None);
    u.to_string()
}

/// Authorization server metadata fields this client uses.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    pub code_challenge_methods: Vec<String>,
    pub token_auth_methods: Vec<String>,
    pub iss_parameter_supported: bool,
    pub scopes_supported: Vec<String>,
}

impl ServerMetadata {
    pub fn from_json(data: &Map<String, Value>) -> Result<ServerMetadata, String> {
        let text = |k: &str| data.get(k).and_then(Value::as_str).map(str::to_owned);
        let list = |k: &str| -> Vec<String> {
            data.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let need =
            |k: &str| text(k).ok_or_else(|| format!("authorization server metadata lacks {k}"));
        let metadata = ServerMetadata {
            issuer: need("issuer")?,
            authorization_endpoint: need("authorization_endpoint")?,
            token_endpoint: need("token_endpoint")?,
            registration_endpoint: text("registration_endpoint"),
            revocation_endpoint: text("revocation_endpoint"),
            code_challenge_methods: list("code_challenge_methods_supported"),
            token_auth_methods: list("token_endpoint_auth_methods_supported"),
            iss_parameter_supported: data
                .get("authorization_response_iss_parameter_supported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            scopes_supported: list("scopes_supported"),
        };
        for endpoint in [
            Some(&metadata.authorization_endpoint),
            Some(&metadata.token_endpoint),
            metadata.registration_endpoint.as_ref(),
            metadata.revocation_endpoint.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let url = url::Url::parse(endpoint)
                .map_err(|e| format!("invalid endpoint {endpoint:?}: {e}"))?;
            if !is_secure(&url) {
                return Err(format!("refusing the non-HTTPS endpoint {endpoint}"));
            }
        }
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_challenges() {
        let c = parse_www_authenticate(
            401,
            [
                r#"Bearer realm="OAuth", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp", scope="read write""#,
            ],
        );
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(c.scope.as_deref(), Some("read write"));
        let c = parse_www_authenticate(
            403,
            [
                "Basic realm=\"x\"",
                r#"Bearer error="insufficient_scope", scope="files:write", error_description="needs \"write\"""#,
            ],
        );
        assert!(c.insufficient_scope());
        assert_eq!(c.scope.as_deref(), Some("files:write"));
        assert_eq!(c.error_description.as_deref(), Some("needs \"write\""));
        let c = parse_www_authenticate(401, ["Basic realm=x, Bearer error=invalid_token"]);
        assert_eq!(c.error.as_deref(), Some("invalid_token"));
        assert_eq!(parse_www_authenticate(401, [""]).error, None);
    }

    #[test]
    fn well_known_urls() {
        let server = url::Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        assert_eq!(
            resource_metadata_urls(&server, None),
            [
                "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource"
            ]
        );
        let issuer = url::Url::parse("https://auth.example.com/tenant1").unwrap();
        assert_eq!(
            server_metadata_urls(&issuer),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration"
            ]
        );
        let root = url::Url::parse("https://auth.example.com/").unwrap();
        assert_eq!(server_metadata_urls(&root).len(), 2);
    }

    #[test]
    fn resources_and_issuers() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(resource_allowed(
            &u("https://a.com/mcp"),
            &u("https://a.com/mcp/")
        ));
        assert!(resource_allowed(
            &u("https://a.com/mcp"),
            &u("https://a.com")
        ));
        assert!(!resource_allowed(
            &u("https://a.com/mcp"),
            &u("https://a.com/other")
        ));
        assert!(!resource_allowed(
            &u("https://a.com/mcp"),
            &u("https://b.com/mcp")
        ));
        assert!(same_issuer("https://x.com/", "https://x.com"));
        assert!(!same_issuer("https://x.com/a", "https://x.com/b"));
        assert!(is_secure(&u("http://127.0.0.1:9/x")));
        assert!(!is_secure(&u("http://example.com/x")));
    }
}
