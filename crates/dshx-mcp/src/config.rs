//! Validation of `[mcp_servers.<name>]` tables into a resolved server spec.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use dshx_core::config::{McpOAuthConfig, McpServerConfig};

use crate::names::is_server_name;

/// Upper bound of a reconnect delay (Node's maximum timer delay).
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TransportSpec {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: url::Url,
        headers: BTreeMap<String, String>,
        auth: AuthSpec,
    },
}

/// Where the bearer token of an HTTP server comes from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AuthSpec {
    /// Only the configured headers (an `Authorization` header, or OAuth off).
    None,
    /// `bearer_token_env_var`.
    BearerEnv(String),
    /// Logins through [`crate::OAuthManager`].
    OAuth(McpOAuthConfig),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReconnectPolicy {
    pub enabled: bool,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
}

impl ReconnectPolicy {
    /// Delay before the `attempt`-th consecutive retry (1-based).
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.initial_delay
            .checked_mul(factor)
            .map_or(self.max_delay, |d| d.min(self.max_delay))
    }
}

/// A validated server.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerSpec {
    pub name: String,
    pub transport: TransportSpec,
    pub call_timeout: Duration,
    pub max_instruction_bytes: usize,
    pub fail_on_startup_error: bool,
    pub reconnect: ReconnectPolicy,
}

impl ServerSpec {
    /// `mcp-client(<name>)`, the prefix of dsh's log and error texts.
    pub fn label(&self) -> String {
        format!("mcp-client({})", self.name)
    }

    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            TransportSpec::Stdio { .. } => "stdio",
            TransportSpec::Http { .. } => "streamable-http",
        }
    }
}

/// Hosts that may be reached over plain `http://` (the MCP SDK's loopback
/// exemption from its TLS requirement).
fn is_loopback(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

pub(crate) fn resolve(name: &str, config: &McpServerConfig) -> Result<ServerSpec, String> {
    let fail = |message: String| format!("mcp server \"{name}\": {message}");
    if !is_server_name(name) {
        return Err(fail(
            "the server name must match [A-Za-z0-9_-]{1,32} (it becomes the mcp__<name>__ tool namespace)".into(),
        ));
    }
    let transport = match config.transport.as_deref() {
        Some("stdio") => "stdio",
        Some("streamable-http" | "http") => "streamable-http",
        Some(other) => {
            return Err(fail(format!(
                "unsupported transport \"{other}\" (use stdio or streamable-http)"
            )));
        }
        None => match (&config.command, &config.url) {
            (Some(_), None) => "stdio",
            (None, Some(_)) => "streamable-http",
            (Some(_), Some(_)) => return Err(fail("set either command or url, not both".into())),
            (None, None) => {
                return Err(fail("set command (stdio) or url (streamable-http)".into()));
            }
        },
    };
    let transport = if transport == "stdio" {
        let command = config
            .command
            .clone()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| fail("stdio servers need a command".into()))?;
        // Passed-through variables join the configured ones (which win).
        let mut env = config.env.clone();
        for name in &config.env_vars {
            if let (false, Ok(value)) = (env.contains_key(name), std::env::var(name)) {
                env.insert(name.clone(), value);
            }
        }
        TransportSpec::Stdio {
            command,
            args: config.args.clone(),
            env,
            cwd: config.cwd.clone().filter(|c| !c.as_os_str().is_empty()),
        }
    } else {
        let raw = config
            .url
            .as_deref()
            .ok_or_else(|| fail("streamable-http servers need a url".into()))?;
        let url = url::Url::parse(raw).map_err(|e| fail(format!("invalid url {raw:?}: {e}")))?;
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(&url) => {}
            "http" => {
                return Err(fail(format!(
                    "refusing plain http:// to the remote host {}; use https:// (plain http is only allowed for localhost, 127.0.0.1 and ::1)",
                    url.host_str().unwrap_or_default()
                )));
            }
            other => return Err(fail(format!("unsupported url scheme \"{other}\""))),
        }
        let has_auth_header = config
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"));
        let auth = match (&config.bearer_token_env_var, &config.oauth) {
            _ if has_auth_header => AuthSpec::None,
            (Some(var), _) if !var.is_empty() => AuthSpec::BearerEnv(var.clone()),
            (_, Some(oauth)) if !oauth.enabled => AuthSpec::None,
            (_, oauth) => AuthSpec::OAuth(oauth.clone().unwrap_or_default()),
        };
        TransportSpec::Http {
            url,
            headers: config.headers.clone(),
            auth,
        }
    };
    let r = &config.reconnect;
    let path = format!("mcp-client({name}): reconnect");
    if r.initial_delay_ms == 0 || r.initial_delay_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "{path}.initialDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }
    if r.max_delay_ms == 0 || r.max_delay_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "{path}.maxDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }
    if r.initial_delay_ms > r.max_delay_ms {
        return Err(format!(
            "{path}.initialDelayMs must be less than or equal to maxDelayMs"
        ));
    }
    if r.max_attempts == 0 {
        return Err(format!("{path}.maxAttempts must be a positive integer"));
    }
    if config.max_instruction_bytes == 0 {
        return Err(fail("max_instruction_bytes must be at least 1".into()));
    }
    Ok(ServerSpec {
        name: name.to_owned(),
        transport,
        call_timeout: Duration::from_millis(config.tool_call_timeout_ms.max(1)),
        max_instruction_bytes: config.max_instruction_bytes,
        fail_on_startup_error: config.fail_on_startup_error,
        reconnect: ReconnectPolicy {
            enabled: r.enabled,
            initial_delay: Duration::from_millis(r.initial_delay_ms),
            max_delay: Duration::from_millis(r.max_delay_ms),
            max_attempts: r.max_attempts,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(url: &str) -> McpServerConfig {
        McpServerConfig {
            url: Some(url.into()),
            ..Default::default()
        }
    }

    #[test]
    fn plain_http_only_for_loopback() {
        for ok in [
            "http://localhost:3000/mcp",
            "http://127.0.0.1/mcp",
            "http://[::1]:8/mcp",
            "https://example.com/mcp",
        ] {
            assert!(resolve("s", &http(ok)).is_ok(), "{ok}");
        }
        let err = resolve("s", &http("http://example.com/mcp")).unwrap_err();
        assert!(
            err.contains("refusing plain http:// to the remote host example.com"),
            "{err}"
        );
        assert!(resolve("s", &http("ftp://example.com/x")).is_err());
    }

    #[test]
    fn transport_is_inferred_and_validated() {
        let stdio = McpServerConfig {
            command: Some("srv".into()),
            ..Default::default()
        };
        assert!(matches!(
            resolve("a", &stdio).unwrap().transport,
            TransportSpec::Stdio { .. }
        ));
        assert!(resolve("a b", &stdio).is_err());
        assert!(resolve("a", &McpServerConfig::default()).is_err());
        let both = McpServerConfig {
            command: Some("srv".into()),
            url: Some("https://x".into()),
            ..Default::default()
        };
        assert!(resolve("a", &both).is_err());
    }

    #[test]
    fn reconnect_backoff() {
        let spec = resolve("a", &http("https://x/mcp")).unwrap();
        let p = &spec.reconnect;
        assert_eq!(p.delay(1), Duration::from_millis(500));
        assert_eq!(p.delay(2), Duration::from_millis(1000));
        assert_eq!(p.delay(10), Duration::from_secs(30));
        assert_eq!(p.delay(100), Duration::from_secs(30));
    }
}
