//! Provider-owned model-request retry policy (dsh `llm/src/retry-policy.ts`)
//! and the delay rule of `llm-retry/src/index.ts`. The adapter never retries;
//! the agent executes this policy and logs `llm/retry` events.
use std::hash::{BuildHasher, Hasher};

use dshx_core::LlmFailure;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_RETRIES: u32 = 5;
pub const DEFAULT_INITIAL_DELAY_MS: f64 = 500.0;
pub const DEFAULT_MAX_DELAY_MS: f64 = 10_000.0;
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;
/// Transient failure codes retried by default.
pub const DEFAULT_RETRYABLE_CODES: [&str; 5] = [
    "EMPTY_RESPONSE",
    "RATE_LIMIT",
    "SERVER",
    "TIMEOUT",
    "TRANSPORT",
];

/// `normal`: bounded retries of transient codes; `always`: retry every failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum RetryMode {
    #[serde(rename_all = "camelCase")]
    Normal {
        max_retries: u32,
        retryable_codes: Vec<String>,
    },
    Always,
}

/// Fully resolved policy (dsh `ResolvedRetryPolicy`), serialized like dsh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    #[serde(flatten)]
    pub mode: RetryMode,
    pub initial_delay_ms: f64,
    pub max_delay_ms: f64,
    pub jitter_ratio: f64,
}

impl Default for RetryPolicy {
    /// Normal mode: 5 retries, 500 ms doubling to 10 s, ±10 % jitter.
    fn default() -> Self {
        RetryPolicy {
            mode: RetryMode::Normal {
                max_retries: DEFAULT_MAX_RETRIES,
                retryable_codes: DEFAULT_RETRYABLE_CODES
                    .iter()
                    .map(|c| c.to_string())
                    .collect(),
            },
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            jitter_ratio: DEFAULT_JITTER_RATIO,
        }
    }
}

/// What the agent should do after one failed request attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum RetryDecision {
    /// Log `llm/retry` with this `retry` number and wait `delay_ms` (cancellably).
    Retry { retry: u32, delay_ms: f64 },
    /// Not retryable or retries exhausted: surface the failure.
    GiveUp,
}

impl RetryPolicy {
    pub fn always() -> Self {
        RetryPolicy {
            mode: RetryMode::Always,
            ..RetryPolicy::default()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let positive = |v: f64| v.is_finite() && v > 0.0;
        if !positive(self.initial_delay_ms)
            || !positive(self.max_delay_ms)
            || self.initial_delay_ms > self.max_delay_ms
        {
            return Err(
                "retryPolicy delays must be positive with initialDelayMs <= maxDelayMs".into(),
            );
        }
        if !(0.0..=1.0).contains(&self.jitter_ratio) {
            return Err("retryPolicy.jitterRatio must be between 0 and 1".into());
        }
        if let RetryMode::Normal {
            retryable_codes, ..
        } = &self.mode
        {
            let unique: std::collections::HashSet<_> = retryable_codes.iter().collect();
            if retryable_codes.is_empty()
                || retryable_codes.iter().any(|c| c.is_empty())
                || unique.len() != retryable_codes.len()
            {
                return Err("retryPolicy.retryableCodes must be non-empty, unique strings".into());
            }
        }
        Ok(())
    }

    pub fn is_retryable(&self, code: &str) -> bool {
        match &self.mode {
            RetryMode::Normal {
                retryable_codes, ..
            } => retryable_codes.iter().any(|c| c == code),
            RetryMode::Always => true,
        }
    }

    /// Local exponential backoff for retry number `retry` (1-based) with a
    /// jitter sample `random` in `[0, 1]`.
    pub fn local_delay_ms(&self, retry: u32, random: f64) -> f64 {
        let exponent = retry.saturating_sub(1).min(1024) as i32;
        let exponential = (self.initial_delay_ms * 2f64.powi(exponent)).min(self.max_delay_ms);
        let jitter = 1.0 - self.jitter_ratio + 2.0 * self.jitter_ratio * random;
        (exponential * jitter).min(self.max_delay_ms)
    }

    /// dsh's decision rule. `previous_retries` counts retries already logged
    /// for this step (reset on `step/start`); `random` is the jitter sample.
    /// A provider `Retry-After` within `max_delay_ms` is honoured verbatim; a
    /// longer one gives up in normal mode.
    pub fn decide(
        &self,
        failure: &LlmFailure,
        previous_retries: u32,
        random: f64,
    ) -> RetryDecision {
        if let RetryMode::Normal { max_retries, .. } = &self.mode
            && (!self.is_retryable(&failure.code) || previous_retries >= *max_retries)
        {
            return RetryDecision::GiveUp;
        }
        let retry = previous_retries + 1;
        let delay_ms = match failure.provider_retry_after_ms.filter(|ms| *ms > 0) {
            Some(after) if after as f64 > self.max_delay_ms => match self.mode {
                RetryMode::Normal { .. } => return RetryDecision::GiveUp,
                RetryMode::Always => self.local_delay_ms(retry, random),
            },
            Some(after) => after as f64,
            None => self.local_delay_ms(retry, random),
        };
        RetryDecision::Retry { retry, delay_ms }
    }

    /// [`Self::decide`] with a fresh random jitter sample.
    pub fn decide_now(&self, failure: &LlmFailure, previous_retries: u32) -> RetryDecision {
        self.decide(failure, previous_retries, random_unit())
    }

    /// `policyKey` of `llm/retry` events: the JSON array dsh derives from the policy.
    pub fn policy_key(&self) -> String {
        let value = match &self.mode {
            RetryMode::Always => {
                serde_json::json!([
                    "always",
                    self.initial_delay_ms,
                    self.max_delay_ms,
                    self.jitter_ratio
                ])
            }
            RetryMode::Normal {
                max_retries,
                retryable_codes,
            } => {
                let mut codes = retryable_codes.clone();
                codes.sort();
                serde_json::json!([
                    "normal",
                    max_retries,
                    codes,
                    self.initial_delay_ms,
                    self.max_delay_ms,
                    self.jitter_ratio
                ])
            }
        };
        crate::json::to_string(&value)
    }

    pub fn max_retries(&self) -> Option<u32> {
        match &self.mode {
            RetryMode::Normal { max_retries, .. } => Some(*max_retries),
            RetryMode::Always => None,
        }
    }
}

/// A uniform sample in `[0, 1)` without an RNG dependency.
pub fn random_unit() -> f64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: &str) -> LlmFailure {
        LlmFailure::new(code, "x")
    }

    #[test]
    fn defaults_match_dsh() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries(), Some(5));
        assert_eq!(
            serde_json::to_value(&policy).unwrap(),
            serde_json::json!({"mode":"normal","maxRetries":5,"retryableCodes":["EMPTY_RESPONSE","RATE_LIMIT","SERVER","TIMEOUT","TRANSPORT"],"initialDelayMs":500.0,"maxDelayMs":10000.0,"jitterRatio":0.1})
        );
        assert_eq!(
            policy.policy_key(),
            r#"["normal",5,["EMPTY_RESPONSE","RATE_LIMIT","SERVER","TIMEOUT","TRANSPORT"],500,10000,0.1]"#
        );
        assert_eq!(
            RetryPolicy::always().policy_key(),
            r#"["always",500,10000,0.1]"#
        );
    }

    #[test]
    fn backoff_doubles_with_jitter_and_caps() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.local_delay_ms(1, 0.5), 500.0);
        assert_eq!(policy.local_delay_ms(2, 0.5), 1000.0);
        assert_eq!(policy.local_delay_ms(1, 0.0), 450.0);
        assert_eq!(policy.local_delay_ms(1, 1.0), 550.0);
        assert_eq!(policy.local_delay_ms(10, 1.0), 10_000.0);
        let sample = random_unit();
        assert!((0.0..1.0).contains(&sample));
    }

    #[test]
    fn decisions_follow_codes_limits_and_retry_after() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.decide(&failure("AUTH"), 0, 0.5),
            RetryDecision::GiveUp
        );
        assert_eq!(
            policy.decide(&failure("SERVER"), 0, 0.5),
            RetryDecision::Retry {
                retry: 1,
                delay_ms: 500.0
            }
        );
        assert_eq!(
            policy.decide(&failure("SERVER"), 5, 0.5),
            RetryDecision::GiveUp
        );
        let mut limited = failure("RATE_LIMIT");
        limited.provider_retry_after_ms = Some(2000);
        assert_eq!(
            policy.decide(&limited, 1, 0.5),
            RetryDecision::Retry {
                retry: 2,
                delay_ms: 2000.0
            }
        );
        limited.provider_retry_after_ms = Some(60_000);
        assert_eq!(policy.decide(&limited, 1, 0.5), RetryDecision::GiveUp);
        assert_eq!(
            RetryPolicy::always().decide(&limited, 7, 0.5),
            RetryDecision::Retry {
                retry: 8,
                delay_ms: 10_000.0
            }
        );
        assert_eq!(
            RetryPolicy::always().decide(&failure("AUTH"), 0, 0.5),
            RetryDecision::Retry {
                retry: 1,
                delay_ms: 500.0
            }
        );
    }
}
