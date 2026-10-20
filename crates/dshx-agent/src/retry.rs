//! Model-request retry policy (dsh `llm/src/retry-policy.ts` + `llm-retry`).
//!
//! Defined locally until `dshx-llm` exports the shared struct; field names and
//! defaults are identical (5 retries, 500 ms → 10 s, jitter 0.1).
use std::sync::Arc;

use dshx_core::LlmFailure;

/// Retry every failure (`Always`) or only `retryable_codes` up to `max_retries` (`Normal`).
#[derive(Debug, Clone, PartialEq)]
pub enum RetryMode {
    Normal { max_retries: u32 },
    Always,
}

#[derive(Clone)]
pub struct RetryPolicy {
    pub mode: RetryMode,
    pub retryable_codes: Vec<String>,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ratio: f64,
    /// Uniform sample in `[0, 1]` for jitter; replaced by tests.
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("mode", &self.mode)
            .field("retryable_codes", &self.retryable_codes)
            .field("initial_delay_ms", &self.initial_delay_ms)
            .field("max_delay_ms", &self.max_delay_ms)
            .field("jitter_ratio", &self.jitter_ratio)
            .finish()
    }
}

pub const DEFAULT_RETRYABLE_CODES: [&str; 5] = [
    "EMPTY_RESPONSE",
    "RATE_LIMIT",
    "SERVER",
    "TIMEOUT",
    "TRANSPORT",
];

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            mode: RetryMode::Normal { max_retries: 5 },
            retryable_codes: DEFAULT_RETRYABLE_CODES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            jitter_ratio: 0.1,
            random: Arc::new(random_unit),
        }
    }
}

/// A process-local uniform sample without an extra RNG dependency.
pub(crate) fn random_unit() -> f64 {
    let bits = uuid::Uuid::new_v4().as_u128() as u64 >> 11;
    bits as f64 / (1u64 << 53) as f64
}

/// JS `String(number)` for the values a policy holds.
pub(crate) fn js_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// JSON number as JS would serialize it (integral values without `.0`).
pub(crate) fn js_json_number(n: f64) -> serde_json::Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    }
}

impl RetryPolicy {
    /// dsh `retryPolicyKey`: `JSON.stringify` of the policy facts.
    pub fn policy_key(&self) -> String {
        match &self.mode {
            RetryMode::Always => format!(
                "[\"always\",{},{},{}]",
                self.initial_delay_ms,
                self.max_delay_ms,
                js_number(self.jitter_ratio)
            ),
            RetryMode::Normal { max_retries } => {
                let mut codes = self.retryable_codes.clone();
                codes.sort();
                format!(
                    "[\"normal\",{},{},{},{},{}]",
                    max_retries,
                    serde_json::to_string(&codes).unwrap_or_default(),
                    self.initial_delay_ms,
                    self.max_delay_ms,
                    js_number(self.jitter_ratio)
                )
            }
        }
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            RetryMode::Normal { .. } => "normal",
            RetryMode::Always => "always",
        }
    }

    /// Exponential backoff with symmetric jitter (dsh `localDelay`).
    pub fn local_delay(&self, retry: u32) -> f64 {
        let exponent = retry.saturating_sub(1).min(1024) as i32;
        let exponential =
            (self.initial_delay_ms as f64 * 2f64.powi(exponent)).min(self.max_delay_ms as f64);
        let jitter = 1.0 - self.jitter_ratio + 2.0 * self.jitter_ratio * (self.random)();
        (exponential * jitter).min(self.max_delay_ms as f64)
    }

    /// Decides whether attempt `previous_retry + 1` happens and after which delay.
    /// `None` keeps the original failure.
    pub fn decide(&self, failure: &LlmFailure, previous_retry: u32) -> Option<f64> {
        match &self.mode {
            RetryMode::Normal { max_retries } => {
                if !self.retryable_codes.contains(&failure.code) || previous_retry >= *max_retries {
                    return None;
                }
            }
            RetryMode::Always => {}
        }
        let retry = previous_retry + 1;
        match failure.provider_retry_after_ms.filter(|ms| *ms > 0) {
            Some(after) if after > self.max_delay_ms => match self.mode {
                RetryMode::Normal { .. } => None,
                RetryMode::Always => Some(self.local_delay(retry)),
            },
            Some(after) => Some(after as f64),
            None => Some(self.local_delay(retry)),
        }
    }

    /// `llm/retry` payload.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn event_data(
        &self,
        retry_id: &str,
        turn: u64,
        step: u64,
        provider: &str,
        retry: u32,
        delay_ms: f64,
        failure: &LlmFailure,
    ) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("retryId".into(), retry_id.into());
        m.insert("turn".into(), turn.into());
        m.insert("step".into(), step.into());
        m.insert("provider".into(), provider.into());
        m.insert("mode".into(), self.mode_name().into());
        m.insert("policyKey".into(), self.policy_key().into());
        m.insert("retry".into(), retry.into());
        if let RetryMode::Normal { max_retries } = self.mode {
            m.insert("maxRetries".into(), max_retries.into());
        }
        m.insert("delayMs".into(), js_json_number(delay_ms));
        m.insert(
            "failure".into(),
            serde_json::to_value(failure).unwrap_or_default(),
        );
        serde_json::Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_key_matches_dsh() {
        assert_eq!(
            RetryPolicy::default().policy_key(),
            "[\"normal\",5,[\"EMPTY_RESPONSE\",\"RATE_LIMIT\",\"SERVER\",\"TIMEOUT\",\"TRANSPORT\"],500,10000,0.1]"
        );
    }

    #[test]
    fn delays_and_limits() {
        let p = RetryPolicy {
            random: Arc::new(|| 0.5),
            ..Default::default()
        };
        let f = LlmFailure::new("SERVER", "boom");
        assert_eq!(p.decide(&f, 0), Some(500.0));
        assert_eq!(p.decide(&f, 1), Some(1000.0));
        assert_eq!(p.decide(&f, 5), None);
        assert_eq!(p.local_delay(10), 10_000.0);
        assert_eq!(p.decide(&LlmFailure::new("AUTH", "x"), 0), None);
        let mut ra = LlmFailure::new("RATE_LIMIT", "slow");
        ra.provider_retry_after_ms = Some(2000);
        assert_eq!(p.decide(&ra, 0), Some(2000.0));
        ra.provider_retry_after_ms = Some(20_000);
        assert_eq!(p.decide(&ra, 0), None);
        assert_eq!(js_json_number(500.0).to_string(), "500");
    }
}
