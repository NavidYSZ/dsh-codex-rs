//! "Fetch available models": the model list a provider publishes (Anthropic
//! `GET /v1/models`, OpenAI-compatible `GET /models`), mapped to candidate
//! rows for the settings page. Known Anthropic models get their catalog
//! metadata and prices.
use std::time::Duration;

use dshx_core::LlmFailure;
use dshx_core::config::ModelCost;
use serde::Serialize;
use serde_json::Value;

use crate::config::user_agent;
use crate::provider::{Api, known_model};
use crate::transport::{ErrorHeaders, labeled_http_error};
use crate::{anthropic, openai};

/// Pages of an Anthropic listing read at most (1000 models each).
const MAX_PAGES: usize = 10;
const TIMEOUT: Duration = Duration::from_secs(20);

/// One model a provider lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
}

/// Where and how to ask.
pub struct Probe<'a> {
    pub api: Api,
    pub base_url: &'a str,
    pub key: Option<&'a str>,
    pub bearer: bool,
    pub headers: &'a [(String, String)],
}

/// Lists the provider's models.
pub async fn fetch_models(probe: &Probe<'_>) -> Result<Vec<RemoteModel>, LlmFailure> {
    let label = probe.api.label();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| LlmFailure::new("TRANSPORT", format!("HTTP client: {e}")))?;
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = match probe.api {
            Api::AnthropicMessages => anthropic::models_url(probe.base_url),
            Api::OpenAiCompletions => openai::models_url(probe.base_url),
        };
        if probe.api == Api::AnthropicMessages {
            url.push_str("?limit=1000");
            if let Some(id) = &after {
                url.push_str("&after_id=");
                url.push_str(&urlencode(id));
            }
        }
        let mut request = http
            .get(&url)
            .header("user-agent", user_agent())
            .header("accept", "application/json");
        if probe.api == Api::AnthropicMessages {
            request = request.header("anthropic-version", anthropic::ANTHROPIC_VERSION);
        }
        if let Some(key) = probe.key {
            request = if probe.bearer {
                request.bearer_auth(key)
            } else {
                request.header("x-api-key", key)
            };
        }
        for (name, value) in probe.headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| LlmFailure::new("TRANSPORT", format!("{label} model list failed: {e}")))?;
        let status = response.status();
        let headers = ErrorHeaders::from_map(response.headers());
        let text = response
            .text()
            .await
            .map_err(|e| LlmFailure::new("TRANSPORT", format!("{label} model list failed: {e}")))?;
        if !status.is_success() {
            return Err(labeled_http_error(label, status.as_u16(), &text, &headers));
        }
        let body: Value = serde_json::from_str(&text).map_err(|_| {
            LlmFailure::new(
                "MALFORMED_RESPONSE",
                format!("{label} model list is not JSON"),
            )
        })?;
        let rows = body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                LlmFailure::new(
                    "MALFORMED_RESPONSE",
                    format!("{label} model list has no data"),
                )
            })?;
        out.extend(rows.iter().filter_map(|row| remote_model(probe.api, row)));
        let more = body.get("has_more").and_then(Value::as_bool) == Some(true);
        after = body
            .get("last_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if probe.api != Api::AnthropicMessages || !more || after.is_none() {
            break;
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|m| seen.insert(m.id.clone()));
    Ok(out)
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One listed row; capability fields are read where providers publish them.
pub fn remote_model(api: Api, row: &Value) -> Option<RemoteModel> {
    let id = row.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let number = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| row.get(*k).and_then(Value::as_u64))
    };
    let mut model = RemoteModel {
        id: id.to_owned(),
        name: ["display_name", "name"]
            .iter()
            .find_map(|k| row.get(*k).and_then(Value::as_str))
            .filter(|n| !n.is_empty() && *n != id)
            .map(str::to_owned),
        context_window: number(&[
            "max_input_tokens",
            "context_window",
            "context_length",
            "max_model_len",
        ]),
        max_tokens: number(&["max_tokens", "max_output_tokens"])
            .or_else(|| {
                row.get("top_provider")
                    .and_then(|p| p.get("max_completion_tokens"))
                    .and_then(Value::as_u64)
            })
            .and_then(|n| u32::try_from(n).ok()),
        ..RemoteModel::default()
    };
    let caps = row.get("capabilities");
    let supported = |path: &[&str]| {
        let mut node = caps;
        for key in path {
            node = node.and_then(|n| n.get(*key));
        }
        node.and_then(|n| n.get("supported"))
            .and_then(Value::as_bool)
    };
    let image = supported(&["image_input"]).or_else(|| {
        row.get("architecture")
            .and_then(|a| a.get("input_modalities"))
            .and_then(Value::as_array)
            .map(|m| m.iter().any(|v| v == "image"))
    });
    if let Some(image) = image {
        model.input = Some(if image {
            vec!["text".into(), "image".into()]
        } else {
            vec!["text".into()]
        });
    }
    if api == Api::AnthropicMessages {
        model.reasoning_efforts = ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .filter(|e| supported(&["effort", e]) == Some(true))
            .map(|e| e.to_string())
            .collect();
        if supported(&["thinking", "types", "adaptive"]) == Some(true) {
            model.thinking = Some("adaptive".into());
        } else if supported(&["thinking", "types", "enabled"]) == Some(true) {
            model.thinking = Some("budget".into());
        }
    }
    if let Some(known) = known_model(api, id) {
        model.name = model.name.or(Some(known.name));
        model.context_window = model.context_window.or(Some(known.context_window));
        model.max_tokens = model.max_tokens.or(Some(known.max_tokens));
        model.input = model.input.or(Some(known.input));
        model.reasoning_efforts = known.efforts;
        model.default_effort = known.default_effort;
        model.thinking = Some(known.thinking.as_str().into());
        model.cost = known.cost;
    }
    Some(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_carry_what_providers_publish() {
        let anthropic = remote_model(
            Api::AnthropicMessages,
            &json!({"id": "claude-new-1", "display_name": "Claude New", "max_input_tokens": 1000000,
                    "max_tokens": 128000, "capabilities": {"image_input": {"supported": true},
                    "thinking": {"types": {"adaptive": {"supported": true}}},
                    "effort": {"low": {"supported": true}, "high": {"supported": true}, "max": {"supported": false}}}}),
        )
        .unwrap();
        assert_eq!(anthropic.name.as_deref(), Some("Claude New"));
        assert_eq!(anthropic.context_window, Some(1_000_000));
        assert_eq!(
            anthropic.input.as_deref(),
            Some(&["text".to_string(), "image".to_string()][..])
        );
        assert_eq!(anthropic.reasoning_efforts, ["low", "high"]);
        assert_eq!(anthropic.thinking.as_deref(), Some("adaptive"));
        assert_eq!(anthropic.cost, None);
        let known =
            remote_model(Api::AnthropicMessages, &json!({"id": "claude-sonnet-5-5"})).unwrap();
        assert_eq!(known.cost.unwrap().input, 2.0);
        let router = remote_model(
            Api::OpenAiCompletions,
            &json!({"id": "moonshotai/kimi-k3", "name": "Kimi K3", "context_length": 1048576,
                    "architecture": {"input_modalities": ["text", "image"]},
                    "top_provider": {"max_completion_tokens": 131072}}),
        )
        .unwrap();
        assert_eq!(router.context_window, Some(1_048_576));
        assert_eq!(router.max_tokens, Some(131_072));
        assert!(router.input.unwrap().contains(&"image".to_string()));
        assert!(remote_model(Api::OpenAiCompletions, &json!({"object": "model"})).is_none());
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
    }
}
