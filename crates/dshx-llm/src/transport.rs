//! HTTP and in-band error classification (dsh `llm-deepseek/src/transport.ts`,
//! `llm/src/error.ts`).
use std::sync::LazyLock;
use std::time::SystemTime;

use dshx_core::LlmFailure;
use regex::Regex;
use serde_json::Value;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

// JS `\b` and `\s` without the `u` flag are ASCII-only.
static CONTEXT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(
            r"(?i)(?:^|[^a-z0-9])context[\s_-](?:length|window)[\s_-](?:exceed(?:ed|s)?|overflow(?:ed)?|limit[\s_-]exceeded)(?:$|[^a-z0-9])",
        ),
        re(
            r"(?i)(?-u:\b)(?:maximum|max)(?:\s+(?:allowed|supported))?\s+context\s+(?:length|window)(?-u:\b)",
        ),
        re(
            r"(?i)(?-u:\b)(?:request|prompt|input|messages?)\s+(?:is\s+|are\s+)?too\s+(?:large|long)\s+for\s+(?:(?:this|the)\s+)?(?:model(?:'s)?\s+)?context(?:\s+window)?(?-u:\b)",
        ),
        re(
            r"(?i)(?-u:\b)(?:input|prompt|request)\s+(?:is\s+)?too\s+(?:long|large)\s+for\s+(?:this|the)\s+model(?-u:\b)",
        ),
        re(
            r"(?i)(?-u:\b)(?:input|prompt|request|messages?)(?-u:\b).{0,40}(?-u:\b)(?:exceed(?:s|ed)?|overflows?|is\s+larger\s+than)(?-u:\b).{0,40}(?-u:\b)(?:the\s+)?(?:model(?:'s)?\s+)?context(?:\s+(?:length|window))?(?-u:\b)",
        ),
    ]
});

static QUOTA_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)(?-u:\b)insufficient[\s_-]+(?:quota|balance|credits?)(?-u:\b)"),
        re(r"(?i)(?-u:\b)(?:quota|usage[\s_-]+limit)[\s_-]+(?:exceeded|exhausted|reached)(?-u:\b)"),
        re(
            r"(?i)(?-u:\b)exceed(?:ed|s)?[\s_-]+(?:(?:your|the)[\s_-]+)?(?:current[\s_-]+)?quota(?-u:\b)",
        ),
        re(r"(?i)(?-u:\b)(?:balance|credits?)[\s_-]+(?:exhausted|depleted)(?-u:\b)"),
        re(r"(?i)(?-u:\b)out[\s_-]+of[\s_-]+(?:credits?|budget)(?-u:\b)"),
    ]
});

/// Provider wording for a request that exceeds the model context window.
pub fn is_context_window_exceeded(detail: &str) -> bool {
    CONTEXT_PATTERNS.iter().any(|r| r.is_match(detail))
}

/// Provider wording for an exhausted account quota (not a rate limit).
pub fn is_quota_exceeded(detail: &str) -> bool {
    QUOTA_PATTERNS.iter().any(|r| r.is_match(detail))
}

/// The response headers the classifier reads.
#[derive(Debug, Clone, Default)]
pub struct ErrorHeaders {
    pub retry_after: Option<String>,
    pub request_id: Option<String>,
}

impl ErrorHeaders {
    pub fn from_map(headers: &reqwest::header::HeaderMap) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        ErrorHeaders {
            retry_after: get("retry-after"),
            request_id: get("request-id")
                .or_else(|| get("x-request-id"))
                .or_else(|| get("x-deepseek-request-id"))
                .filter(|id| !id.is_empty()),
        }
    }
}

/// Classifies a provider error into a dsh failure code. `raw` is the decoded
/// response body or in-band `error` event; `status` the HTTP status when the
/// error preceded streaming.
pub fn provider_error(
    raw: Option<&Value>,
    status: Option<u16>,
    headers: Option<&ErrorHeaders>,
) -> LlmFailure {
    labeled_error("DeepSeek Messages", raw, status, headers)
}

/// [`provider_error`] for another protocol: `label` names it in messages
/// (`Anthropic Messages`, `OpenAI Chat Completions`). A bare string `error`
/// or a top-level `message` (common on gateways) becomes the message.
pub fn labeled_error(
    label: &str,
    raw: Option<&Value>,
    status: Option<u16>,
    headers: Option<&ErrorHeaders>,
) -> LlmFailure {
    let error = raw.and_then(|r| r.get("error")).and_then(Value::as_object);
    let field = |name: &str| error.and_then(|e| e.get(name)).and_then(Value::as_str);
    let loose = || {
        let raw = raw?;
        raw.get("error")
            .and_then(Value::as_str)
            .or_else(|| raw.get("message").and_then(Value::as_str))
            .filter(|m| !m.trim().is_empty())
    };
    let message = field("message")
        .or_else(|| if error.is_none() { loose() } else { None })
        .map(str::to_owned)
        .unwrap_or_else(|| match status {
            Some(status) => format!("{label} request failed ({status})"),
            None => format!("{label} request failed (stream error)"),
        });
    let kind = field("type").unwrap_or("");
    // OpenAI-style bodies carry a numeric or string `code`.
    let code_text = error
        .and_then(|e| e.get("code"))
        .map(|c| {
            c.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| c.to_string())
        })
        .unwrap_or_default();
    let detail = format!("{kind} {code_text} {message}");
    let code = if matches!(status, Some(401 | 403))
        || matches!(kind, "authentication_error" | "permission_error")
    {
        "AUTH".to_string()
    } else if is_quota_exceeded(&detail) || status == Some(402) {
        "QUOTA".into()
    } else if status == Some(429) || kind == "rate_limit_error" {
        "RATE_LIMIT".into()
    } else if is_context_window_exceeded(&detail) {
        "CONTEXT_WINDOW_EXCEEDED".into()
    } else if matches!(status, Some(400 | 413)) || kind == "invalid_request_error" {
        "INVALID_REQUEST".into()
    } else if status.is_some_and(|s| s >= 500) || matches!(kind, "api_error" | "overloaded_error") {
        "SERVER".into()
    } else {
        status
            .map(|s| format!("HTTP_{s}"))
            .unwrap_or_else(|| "SERVER".into())
    };
    let mut failure = LlmFailure::new(code, message);
    failure.status = status;
    if let Some(headers) = headers {
        failure.request_id = headers.request_id.clone();
        failure.provider_retry_after_ms = headers.retry_after.as_deref().and_then(retry_after_ms);
    }
    failure
}

/// `Retry-After` as delay-seconds (fractions allowed) or an HTTP date.
pub fn retry_after_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    let is_seconds = !value.is_empty()
        && value.split_once('.').map_or(
            value.bytes().all(|b| b.is_ascii_digit()),
            |(int, frac)| {
                !int.is_empty()
                    && !frac.is_empty()
                    && (int.bytes().chain(frac.bytes())).all(|b| b.is_ascii_digit())
            },
        );
    let ms = if is_seconds {
        value.parse::<f64>().ok()? * 1000.0
    } else {
        let at = httpdate::parse_http_date(value).ok()?;
        match at.duration_since(SystemTime::now()) {
            Ok(delay) => delay.as_secs_f64() * 1000.0,
            Err(_) => return None,
        }
    };
    (ms.is_finite() && ms > 0.0).then(|| ms.ceil() as u64)
}

/// The failure for an HTTP error response with body `text`.
pub fn http_error(status: u16, text: &str, headers: &ErrorHeaders) -> LlmFailure {
    // The status is authoritative when a gateway does not return JSON.
    let raw: Option<Value> = serde_json::from_str(text).ok();
    provider_error(raw.as_ref(), Some(status), Some(headers))
}

/// [`http_error`] for another protocol (see [`labeled_error`]).
pub fn labeled_http_error(
    label: &str,
    status: u16,
    text: &str,
    headers: &ErrorHeaders,
) -> LlmFailure {
    let raw: Option<Value> = serde_json::from_str(text).ok();
    labeled_error(label, raw.as_ref(), Some(status), Some(headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn code(status: Option<u16>, error: Value) -> String {
        provider_error(Some(&json!({"error": error})), status, None).code
    }

    #[test]
    fn classifies_status_and_error_type_like_dsh() {
        let cases: Vec<(Option<u16>, Value, &str)> = vec![
            (Some(401), json!({}), "AUTH"),
            (Some(403), json!({}), "AUTH"),
            (Some(402), json!({}), "QUOTA"),
            (Some(429), json!({}), "RATE_LIMIT"),
            (Some(400), json!({}), "INVALID_REQUEST"),
            (Some(413), json!({}), "INVALID_REQUEST"),
            (Some(503), json!({}), "SERVER"),
            (Some(404), json!({}), "HTTP_404"),
            (None, json!({"type": "authentication_error"}), "AUTH"),
            (None, json!({"type": "rate_limit_error"}), "RATE_LIMIT"),
            (
                None,
                json!({"type": "invalid_request_error"}),
                "INVALID_REQUEST",
            ),
            (None, json!({}), "SERVER"),
            (
                Some(400),
                json!({"message": "maximum context length exceeded"}),
                "CONTEXT_WINDOW_EXCEEDED",
            ),
            (
                Some(400),
                json!({"message": "insufficient balance"}),
                "QUOTA",
            ),
            (Some(400), json!({"code": "insufficient_balance"}), "QUOTA"),
            (
                Some(400),
                json!({"type": "invalid_request_error", "message": "The prompt is too long for this model's context window"}),
                "CONTEXT_WINDOW_EXCEEDED",
            ),
            (
                Some(400),
                json!({"message": "Input exceeds the context window of this model"}),
                "CONTEXT_WINDOW_EXCEEDED",
            ),
            (
                Some(400),
                json!({"code": "context_length_exceeded"}),
                "CONTEXT_WINDOW_EXCEEDED",
            ),
            (
                Some(400),
                json!({"message": "This model's maximum context length is 1000000 tokens"}),
                "CONTEXT_WINDOW_EXCEEDED",
            ),
            (
                Some(400),
                json!({"message": "messages: tool_use ids must be unique"}),
                "INVALID_REQUEST",
            ),
        ];
        for (status, error, expected) in cases {
            assert_eq!(code(status, error.clone()), expected, "{status:?} {error}");
        }
        assert_eq!(
            provider_error(None, Some(502), None).message,
            "DeepSeek Messages request failed (502)"
        );
    }

    #[test]
    fn keeps_request_id_and_valid_retry_after() {
        let headers = ErrorHeaders {
            retry_after: Some("2".into()),
            request_id: Some("r1".into()),
        };
        let failure = provider_error(None, Some(429), Some(&headers));
        assert_eq!(
            (
                failure.provider_retry_after_ms,
                failure.request_id.as_deref()
            ),
            (Some(2000), Some("r1"))
        );
        assert_eq!(retry_after_ms("1.5"), Some(1500));
        assert_eq!(retry_after_ms("invalid"), None);
        assert_eq!(retry_after_ms("0"), None);
        let later = httpdate::fmt_http_date(SystemTime::now() + std::time::Duration::from_secs(60));
        assert!(retry_after_ms(&later).is_some_and(|ms| ms > 50_000));
        let earlier =
            httpdate::fmt_http_date(SystemTime::now() - std::time::Duration::from_secs(60));
        assert_eq!(retry_after_ms(&earlier), None);
    }
}
