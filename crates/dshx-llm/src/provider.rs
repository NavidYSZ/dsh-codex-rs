//! Provider routes besides the built-in DeepSeek one (dsh `llm-pi-ai`
//! providers): the protocols, the built-in catalog, `[providers.<id>]`
//! resolved against it, and `provider/model` references.
//!
//! A model reference is a bare DeepSeek id (`deepseek-flash`, what every
//! config and session before providers holds) or `provider/model`
//! (`anthropic/claude-sonnet-5-5`); the model part may itself contain `/`
//! (`relay/moonshotai/kimi-k3`).
use std::collections::HashSet;

use dshx_core::ModelInfo;
use dshx_core::config::{ModelCost, ProviderCompat, ProviderConfig, ProviderModelConfig};

use crate::config::ConfigError;
use crate::models::PROVIDER;

/// Context window of a custom model that declares none (dsh `llm-pi-ai`'s default).
pub const DEFAULT_CONTEXT_WINDOW: u64 = 262_144;
/// Output cap of a custom model that declares none.
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Output cap a request gets by default on these routes; streaming keeps
/// larger values safe, but they reserve rate-limit headroom for nothing.
pub const DEFAULT_REQUEST_MAX_TOKENS: u32 = 64_000;
/// Effort levels the picker knows, from least to greatest.
pub const EFFORT_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The wire protocol of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// `POST {base}/v1/messages`, SSE (Anthropic and compatible endpoints).
    AnthropicMessages,
    /// `POST {base}/chat/completions`, SSE (OpenAI-compatible endpoints).
    OpenAiCompletions,
}

impl Api {
    pub fn parse(text: &str) -> Option<Api> {
        match text {
            "anthropic-messages" => Some(Api::AnthropicMessages),
            "openai-completions" => Some(Api::OpenAiCompletions),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Api::AnthropicMessages => "anthropic-messages",
            Api::OpenAiCompletions => "openai-completions",
        }
    }

    /// Protocol name in failure messages.
    pub fn label(self) -> &'static str {
        match self {
            Api::AnthropicMessages => "Anthropic Messages",
            Api::OpenAiCompletions => "OpenAI Chat Completions",
        }
    }
}

/// How an Anthropic Messages request asks for thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    /// `{type: "adaptive"}` plus `output_config.effort` (Claude 4.6 and later).
    Adaptive,
    /// `{type: "enabled", budget_tokens}` (older Claude models, many gateways).
    Budget,
    /// `{type: "enabled"}` plus `output_config.effort` (DeepSeek style).
    Effort,
    /// No thinking parameter at all.
    None,
}

impl Thinking {
    pub fn parse(text: &str) -> Option<Thinking> {
        match text {
            "adaptive" => Some(Thinking::Adaptive),
            "budget" => Some(Thinking::Budget),
            "effort" => Some(Thinking::Effort),
            "none" => Some(Thinking::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Thinking::Adaptive => "adaptive",
            Thinking::Budget => "budget",
            Thinking::Effort => "effort",
            Thinking::None => "none",
        }
    }
}

/// One model of a provider, with every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u32,
    /// `["text"]` or `["text", "image"]`.
    pub input: Vec<String>,
    /// Picker levels; empty = no reasoning control.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub thinking: Thinking,
    /// USD per million tokens, when known.
    pub cost: Option<ModelCost>,
}

impl ProviderModel {
    pub fn accepts_images(&self) -> bool {
        self.input.iter().any(|m| m == "image")
    }

    /// Selector metadata (dsh `LlmModelInfo`).
    pub fn info(&self) -> ModelInfo {
        ModelInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            context_window: self.context_window,
            input_modalities: self.input.clone(),
            reasoning_efforts: self.efforts.clone(),
            default_reasoning_effort: self.default_effort.clone(),
        }
    }

    /// The level this model receives for a requested effort: a listed level
    /// passes, others fall to the nearest listed level (`off` to the lowest
    /// one), no request to the default. `None` when the model has no levels.
    pub fn effort(&self, requested: Option<&str>) -> Option<String> {
        if self.efforts.is_empty() {
            return None;
        }
        let fallback = || {
            self.default_effort
                .clone()
                .filter(|d| self.efforts.contains(d))
                .or_else(|| self.efforts.iter().find(|e| *e == "high").cloned())
                .unwrap_or_else(|| self.efforts[self.efforts.len() - 1].clone())
        };
        let Some(requested) = requested else {
            return Some(fallback());
        };
        if self.efforts.iter().any(|e| e == requested) {
            return Some(requested.to_owned());
        }
        let Some(want) = rank(requested) else {
            return Some(fallback());
        };
        // Nearest ranked level; ties go up. `off` is never a substitute.
        self.efforts
            .iter()
            .filter(|e| *e != "off")
            .filter_map(|e| rank(e).map(|r| (r, e)))
            .min_by_key(|(r, _)| ((*r as i32 - want as i32).abs(), usize::MAX - r))
            .map(|(_, e)| e.clone())
            .or_else(|| Some(fallback()))
    }
}

fn rank(effort: &str) -> Option<usize> {
    EFFORT_LEVELS.iter().position(|e| *e == effort)
}

/// Endpoint quirks with their defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compat {
    /// Anthropic Messages: `Authorization: Bearer` instead of `x-api-key`.
    pub bearer: bool,
    /// Anthropic Messages: send `cache_control` breakpoints.
    pub cache_control: bool,
    /// OpenAI Chat Completions: name of the output cap field.
    pub max_tokens_field: String,
    /// OpenAI Chat Completions: replay reasoning as `reasoning_content`.
    pub reasoning_content: bool,
}

impl Compat {
    fn resolve(api: Api, config: &ProviderCompat) -> Result<Compat, ConfigError> {
        let bearer = match config.auth.as_deref() {
            None | Some("x-api-key") => api == Api::OpenAiCompletions,
            Some("bearer") => true,
            Some(other) => {
                return Err(ConfigError(format!(
                    "unknown auth {other:?} (x-api-key or bearer)"
                )));
            }
        };
        let max_tokens_field = match config.max_tokens_field.as_deref() {
            None => "max_tokens".to_owned(),
            Some(f @ ("max_tokens" | "max_completion_tokens")) => f.to_owned(),
            Some(other) => {
                return Err(ConfigError(format!(
                    "unknown max_tokens_field {other:?} (max_tokens or max_completion_tokens)"
                )));
            }
        };
        Ok(Compat {
            bearer,
            cache_control: config.cache_control.unwrap_or(true),
            max_tokens_field,
            reasoning_content: config.reasoning_content.unwrap_or(false),
        })
    }
}

/// One provider route, resolved: the `[providers.<id>]` table over its
/// catalog entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub base_url: String,
    pub api_key_env: Option<String>,
    /// Catalog providers need a key; a custom endpoint may work without one.
    pub key_required: bool,
    pub headers: Vec<(String, String)>,
    /// Model for session titles; `None` = the chat's model.
    pub title_model: Option<String>,
    pub models: Vec<ProviderModel>,
    /// The id names a catalog entry (`anthropic`).
    pub catalog: bool,
    /// The table lists its own models instead of the catalog's.
    pub models_customized: bool,
    pub compat: Compat,
}

impl ProviderSpec {
    /// Resolves and validates one `[providers.<id>]` table.
    pub fn resolve(id: &str, config: &ProviderConfig) -> Result<ProviderSpec, ConfigError> {
        let fail = |e: ConfigError| ConfigError(format!("provider \"{id}\": {}", e.0));
        check_provider_id(id).map_err(fail)?;
        if id == PROVIDER {
            return Err(fail(ConfigError(
                "the DeepSeek route is built in; set base_url at the top level instead".into(),
            )));
        }
        let entry = catalog_entry(id);
        let api = match (&config.api, &entry) {
            (Some(text), _) => Api::parse(text).ok_or_else(|| {
                fail(ConfigError(format!(
                    "unknown api {text:?} (anthropic-messages or openai-completions)"
                )))
            })?,
            (None, Some(entry)) => entry.api,
            (None, None) => {
                return Err(fail(ConfigError(
                    "a custom provider needs api = \"anthropic-messages\" or \"openai-completions\"".into(),
                )));
            }
        };
        let base_url = match (&config.base_url, &entry) {
            (Some(url), _) => url.trim().to_owned(),
            (None, Some(entry)) => entry.base_url.clone(),
            (None, None) => {
                return Err(fail(ConfigError(
                    "a custom provider needs a base_url".into(),
                )));
            }
        };
        check_base_url(&base_url).map_err(fail)?;
        let mut headers = Vec::new();
        for (name, value) in &config.headers {
            check_header(name, value).map_err(fail)?;
            headers.push((name.clone(), value.clone()));
        }
        if let Some(var) = &config.api_key_env
            && !valid_env_name(var)
        {
            return Err(fail(ConfigError(format!(
                "api_key_env {var:?} is not an environment variable name"
            ))));
        }
        let models_customized = !config.models.is_empty() || entry.is_none();
        let models = if config.models.is_empty() {
            entry.as_ref().map(|e| e.models.clone()).unwrap_or_default()
        } else {
            let mut models = Vec::with_capacity(config.models.len());
            for model in &config.models {
                // Hand-declared models of a catalog route keep what the catalog knows about them.
                let known = entry.as_ref().and_then(|_| known_model(api, &model.id));
                models.push(resolve_model(model, api, known.as_ref()).map_err(fail)?);
            }
            models
        };
        if models.is_empty() {
            return Err(fail(ConfigError(
                "a custom provider needs at least one model".into(),
            )));
        }
        let mut seen = HashSet::new();
        if let Some(dup) = models.iter().find(|m| !seen.insert(m.id.as_str())) {
            return Err(fail(ConfigError(format!(
                "model id {:?} appears twice",
                dup.id
            ))));
        }
        let title_model = config
            .title_model
            .clone()
            .or_else(|| entry.as_ref().and_then(|e| e.title_model.clone()))
            .filter(|m| !m.trim().is_empty());
        Ok(ProviderSpec {
            id: id.to_owned(),
            name: config
                .display_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| entry.as_ref().map(|e| e.name.clone()))
                .unwrap_or_else(|| id.to_owned()),
            api,
            base_url,
            api_key_env: config
                .api_key_env
                .clone()
                .or_else(|| entry.as_ref().and_then(|e| e.api_key_env.clone())),
            key_required: entry.is_some(),
            headers,
            title_model,
            models,
            catalog: entry.is_some(),
            models_customized,
            compat: Compat::resolve(api, &config.compat).map_err(fail)?,
        })
    }

    pub fn model(&self, id: &str) -> Option<&ProviderModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Whether the official Anthropic API serves this route: it drops thinking
    /// blocks a model cannot read, so they replay across model switches.
    pub fn official_anthropic(&self) -> bool {
        self.api == Api::AnthropicMessages
            && reqwest::Url::parse(&self.base_url)
                .is_ok_and(|u| u.host_str() == Some("api.anthropic.com"))
    }
}

/// The model of `config`, with the protocol's and the catalog's defaults.
fn resolve_model(
    config: &ProviderModelConfig,
    api: Api,
    known: Option<&ProviderModel>,
) -> Result<ProviderModel, ConfigError> {
    let id = config.id.trim();
    if id.is_empty() {
        return Err(ConfigError("every model needs an id".into()));
    }
    if id.chars().any(char::is_control) || id.len() > 200 {
        return Err(ConfigError(format!("model id {id:?} is not valid")));
    }
    let input = if config.input.is_empty() {
        known
            .map(|k| k.input.clone())
            .unwrap_or_else(|| vec!["text".into()])
    } else {
        config.input.clone()
    };
    if !input.iter().any(|m| m == "text") || input.iter().any(|m| m != "text" && m != "image") {
        return Err(ConfigError(format!(
            "model {id:?}: input must be [\"text\"] or [\"text\", \"image\"]"
        )));
    }
    let efforts = if config.reasoning_efforts.is_empty() {
        known.map(|k| k.efforts.clone()).unwrap_or_default()
    } else {
        config.reasoning_efforts.clone()
    };
    let mut seen = HashSet::new();
    for effort in &efforts {
        let ok = !effort.is_empty()
            && effort.len() <= 32
            && effort
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !ok || !seen.insert(effort.as_str()) {
            return Err(ConfigError(format!(
                "model {id:?}: invalid or repeated reasoning effort {effort:?}"
            )));
        }
    }
    let default_effort = config
        .default_effort
        .clone()
        .or_else(|| known.and_then(|k| k.default_effort.clone()));
    if let Some(effort) = &default_effort
        && !efforts.contains(effort)
    {
        return Err(ConfigError(format!(
            "model {id:?}: default_effort {effort:?} is not one of its reasoning_efforts"
        )));
    }
    let thinking = match &config.thinking {
        Some(text) => Thinking::parse(text).ok_or_else(|| {
            ConfigError(format!(
                "model {id:?}: unknown thinking {text:?} (adaptive, budget, effort or none)"
            ))
        })?,
        None => match (known, api) {
            (Some(k), _) => k.thinking,
            // Gateways commonly take the budget form; without levels nothing is asked.
            (None, Api::AnthropicMessages) if !efforts.is_empty() => Thinking::Budget,
            _ => Thinking::None,
        },
    };
    let context_window = config
        .context_window
        .or(known.map(|k| k.context_window))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let max_tokens = config
        .max_tokens
        .or(known.map(|k| k.max_tokens))
        .unwrap_or(DEFAULT_MAX_TOKENS);
    if context_window == 0 || max_tokens == 0 {
        return Err(ConfigError(format!(
            "model {id:?}: context_window and max_tokens must be positive"
        )));
    }
    let cost = config.cost.or(known.and_then(|k| k.cost));
    if let Some(c) = &cost
        && [Some(c.input), Some(c.output), c.cache_read, c.cache_write]
            .into_iter()
            .flatten()
            .any(|p| !p.is_finite() || p < 0.0)
    {
        return Err(ConfigError(format!(
            "model {id:?}: prices must be non-negative numbers"
        )));
    }
    Ok(ProviderModel {
        id: id.to_owned(),
        name: config
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| known.map(|k| k.name.clone()))
            .unwrap_or_else(|| id.to_owned()),
        context_window,
        max_tokens,
        input,
        efforts,
        default_effort,
        thinking,
        cost,
    })
}

/// Lowercase letter first, then lowercase letters, digits and dashes (dsh rule).
pub fn check_provider_id(id: &str) -> Result<(), ConfigError> {
    let mut bytes = id.bytes();
    let ok = id.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ConfigError(format!(
            "provider id {id:?} must start with a lowercase letter, then lowercase letters, digits and dashes"
        )))
    }
}

/// An HTTP(S) root without credentials, query or fragment.
pub fn check_base_url(url: &str) -> Result<(), ConfigError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| ConfigError(format!("invalid base URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ConfigError(
            "the base URL must be an HTTP(S) root without credentials, query, or fragment".into(),
        ));
    }
    Ok(())
}

fn check_header(name: &str, value: &str) -> Result<(), ConfigError> {
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    let value_ok = value
        .bytes()
        .all(|b| b == b'\t' || (0x20..0x7f).contains(&b));
    let reserved = matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "content-type" | "transfer-encoding" | "connection"
    );
    if name_ok && value_ok && !reserved {
        Ok(())
    } else {
        Err(ConfigError(format!("header {name:?} is not allowed")))
    }
}

fn valid_env_name(var: &str) -> bool {
    !var.is_empty()
        && var.len() <= 128
        && !var.as_bytes()[0].is_ascii_digit()
        && var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// ---- model references ----------------------------------------------------

/// The reference of a model: bare for the DeepSeek route, else `provider/model`.
pub fn model_ref(provider: &str, model: &str) -> String {
    if provider == PROVIDER {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    }
}

/// Splits a reference into `(provider, model)`. A prefix that names no
/// configured provider keeps the whole text as a DeepSeek model id;
/// `deepseek-official/<id>` is accepted too.
pub fn split_model_ref(reference: &str, is_provider: impl Fn(&str) -> bool) -> (&str, &str) {
    if let Some((provider, model)) = reference.split_once('/')
        && !model.is_empty()
        && (provider == PROVIDER || is_provider(provider))
    {
        return (provider, model);
    }
    (PROVIDER, reference)
}

// ---- catalog ---------------------------------------------------------------

/// A provider the "add provider" dialog offers ready-made.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub title_model: Option<String>,
    pub models: Vec<ProviderModel>,
}

/// Official Anthropic API root.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// The built-in catalog besides DeepSeek: Anthropic.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![CatalogEntry {
        id: "anthropic".into(),
        name: "Anthropic".into(),
        api: Api::AnthropicMessages,
        base_url: ANTHROPIC_BASE_URL.into(),
        api_key_env: Some("ANTHROPIC_API_KEY".into()),
        // Titles need no thinking; Haiku 5.5 may turn it off and costs least.
        title_model: Some("claude-haiku-5-5".into()),
        models: [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
        ]
        .iter()
        .filter_map(|id| known_model(Api::AnthropicMessages, id))
        .collect(),
    }]
}

pub fn catalog_entry(id: &str) -> Option<CatalogEntry> {
    catalog().into_iter().find(|e| e.id == id)
}

const ADAPTIVE_FULL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const ADAPTIVE_OFF: &[&str] = &["off", "low", "medium", "high", "xhigh", "max"];

/// What we know about a model of a protocol: catalog metadata and list
/// prices. Sources: claude-api skill model tables (cached 2026-10-06) and
/// platform.claude.com/docs/en/about-claude/pricing (fetched 2026-10-22);
/// prices are USD per million tokens, cache writes the 5-minute rate.
pub fn known_model(api: Api, id: &str) -> Option<ProviderModel> {
    if api != Api::AnthropicMessages {
        return None;
    }
    // (id, name, context, max output, efforts, default effort, thinking, input, output, cache read, cache write)
    type Row = (
        &'static str,
        &'static str,
        u64,
        u32,
        &'static [&'static str],
        &'static str,
        Thinking,
        [f64; 4],
    );
    const ROWS: &[Row] = &[
        // Thinking cannot be turned off; Anthropic's default effort is medium.
        (
            "claude-opus-5-5",
            "Claude Opus 5.5",
            1_000_000,
            128_000,
            ADAPTIVE_FULL,
            "high",
            Thinking::Adaptive,
            [4.0, 20.0, 0.20, 5.0],
        ),
        // `disabled` is a 400 (only `between_tools` turns it off).
        (
            "claude-sonnet-5-5",
            "Claude Sonnet 5.5",
            1_000_000,
            128_000,
            ADAPTIVE_FULL,
            "high",
            Thinking::Adaptive,
            [2.0, 10.0, 0.10, 2.50],
        ),
        // Up to 100K-token prompts; longer ones cost 0.50 / 2.50 / 0.05 / 0.625.
        (
            "claude-haiku-5-5",
            "Claude Haiku 5.5",
            1_000_000,
            128_000,
            ADAPTIVE_OFF,
            "medium",
            Thinking::Adaptive,
            [0.10, 0.50, 0.01, 0.125],
        ),
        // Thinking is always on.
        (
            "claude-fable-5-1",
            "Claude Fable 5.1",
            1_000_000,
            128_000,
            ADAPTIVE_FULL,
            "high",
            Thinking::Adaptive,
            [10.0, 50.0, 0.25, 12.50],
        ),
        (
            "claude-opus-5",
            "Claude Opus 5",
            1_000_000,
            128_000,
            ADAPTIVE_OFF,
            "high",
            Thinking::Adaptive,
            [5.0, 25.0, 0.50, 6.25],
        ),
        (
            "claude-sonnet-5",
            "Claude Sonnet 5",
            1_000_000,
            128_000,
            ADAPTIVE_OFF,
            "high",
            Thinking::Adaptive,
            [2.0, 10.0, 0.20, 2.50],
        ),
        (
            "claude-opus-4-8",
            "Claude Opus 4.8",
            1_000_000,
            128_000,
            ADAPTIVE_OFF,
            "high",
            Thinking::Adaptive,
            [5.0, 25.0, 0.50, 6.25],
        ),
        (
            "claude-opus-4-7",
            "Claude Opus 4.7",
            1_000_000,
            128_000,
            ADAPTIVE_OFF,
            "high",
            Thinking::Adaptive,
            [5.0, 25.0, 0.50, 6.25],
        ),
        (
            "claude-opus-4-6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            &["off", "low", "medium", "high", "max"],
            "high",
            Thinking::Adaptive,
            [5.0, 25.0, 0.50, 6.25],
        ),
        (
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            1_000_000,
            128_000,
            &["off", "low", "medium", "high", "max"],
            "high",
            Thinking::Adaptive,
            [3.0, 15.0, 0.30, 3.75],
        ),
        // No effort parameter: levels become thinking budgets.
        (
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            200_000,
            64_000,
            &["off", "low", "medium", "high"],
            "medium",
            Thinking::Budget,
            [1.0, 5.0, 0.10, 1.25],
        ),
    ];
    let row = ROWS.iter().find(|r| r.0 == id)?;
    let [input, output, cache_read, cache_write] = row.7;
    Some(ProviderModel {
        id: row.0.into(),
        name: row.1.into(),
        context_window: row.2,
        max_tokens: row.3,
        input: vec!["text".into(), "image".into()],
        efforts: row.4.iter().map(|e| e.to_string()).collect(),
        default_effort: Some(row.5.into()),
        thinking: row.6,
        cost: Some(ModelCost {
            input,
            output,
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(api: &str) -> ProviderConfig {
        ProviderConfig {
            api: Some(api.into()),
            base_url: Some("https://relay.example/v1".into()),
            models: vec![ProviderModelConfig {
                id: "glm-5.3".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn catalog_provider_needs_no_settings() {
        let spec = ProviderSpec::resolve("anthropic", &ProviderConfig::default()).unwrap();
        assert_eq!(spec.api, Api::AnthropicMessages);
        assert_eq!(spec.base_url, ANTHROPIC_BASE_URL);
        assert_eq!(spec.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert!(spec.key_required && spec.catalog && !spec.models_customized);
        assert!(spec.official_anthropic());
        assert_eq!(spec.title_model.as_deref(), Some("claude-haiku-5-5"));
        let ids: Vec<&str> = spec.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-fable-5-1"
            ]
        );
        let opus = spec.model("claude-opus-5-5").unwrap();
        assert_eq!((opus.context_window, opus.max_tokens), (1_000_000, 128_000));
        assert_eq!(opus.cost.unwrap().input, 4.0);
        assert!(!opus.efforts.contains(&"off".to_string()));
        assert!(spec.compat.cache_control && !spec.compat.bearer);
    }

    #[test]
    fn hand_declared_catalog_models_keep_known_metadata() {
        let config = ProviderConfig {
            models: vec![ProviderModelConfig {
                id: "claude-haiku-4-5".into(),
                name: Some("Haiku".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let spec = ProviderSpec::resolve("anthropic", &config).unwrap();
        assert!(spec.models_customized);
        let haiku = &spec.models[0];
        assert_eq!(haiku.name, "Haiku");
        assert_eq!(haiku.thinking, Thinking::Budget);
        assert_eq!(haiku.context_window, 200_000);
        assert_eq!(haiku.cost.unwrap().output, 5.0);
    }

    #[test]
    fn custom_providers_are_validated() {
        let spec = ProviderSpec::resolve("relay", &custom("openai-completions")).unwrap();
        assert!(!spec.key_required && !spec.catalog);
        assert!(spec.compat.bearer);
        let model = &spec.models[0];
        assert_eq!(model.context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(model.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(model.input, ["text"]);
        assert_eq!(model.thinking, Thinking::None);
        assert_eq!(model.cost, None);

        let bad = |id: &str, f: &dyn Fn(&mut ProviderConfig)| {
            let mut config = custom("anthropic-messages");
            f(&mut config);
            ProviderSpec::resolve(id, &config).unwrap_err().0
        };
        assert!(bad("Relay", &|_| {}).contains("lowercase"));
        assert!(bad("deepseek-official", &|_| {}).contains("built in"));
        assert!(bad("relay", &|c| c.api = Some("openai-responses".into())).contains("unknown api"));
        assert!(bad("relay", &|c| c.api = None).contains("needs api"));
        assert!(bad("relay", &|c| c.base_url = None).contains("base_url"));
        assert!(bad("relay", &|c| c.base_url = Some("ftp://x".into())).contains("base URL"));
        assert!(bad("relay", &|c| c.base_url = Some("https://u:p@x".into())).contains("base URL"));
        assert!(bad("relay", &|c| c.models.clear()).contains("at least one model"));
        assert!(bad("relay", &|c| c.models.push(c.models[0].clone())).contains("twice"));
        assert!(bad("relay", &|c| c.models[0].input = vec!["image".into()]).contains("input"));
        assert!(bad("relay", &|c| c.models[0].context_window = Some(0)).contains("positive"));
        assert!(
            bad("relay", &|c| {
                c.models[0].reasoning_efforts = vec!["low".into()];
                c.models[0].default_effort = Some("high".into());
            })
            .contains("default_effort")
        );
        assert!(
            bad("relay", &|c| {
                c.models[0].cost = Some(ModelCost {
                    input: -1.0,
                    ..Default::default()
                })
            })
            .contains("prices")
        );
        assert!(
            bad("relay", &|c| {
                c.headers.insert("Host".into(), "x".into());
            })
            .contains("header")
        );
        assert!(bad("relay", &|c| c.api_key_env = Some("1KEY".into())).contains("api_key_env"));
        assert!(bad("relay", &|c| c.compat.auth = Some("basic".into())).contains("auth"));
        // A gateway model with levels asks for budget thinking.
        let mut config = custom("anthropic-messages");
        config.models[0].reasoning_efforts = vec!["off".into(), "high".into()];
        let spec = ProviderSpec::resolve("relay", &config).unwrap();
        assert_eq!(spec.models[0].thinking, Thinking::Budget);
        assert!(!spec.compat.bearer);
    }

    #[test]
    fn efforts_map_to_the_nearest_listed_level() {
        let opus = known_model(Api::AnthropicMessages, "claude-opus-5-5").unwrap();
        assert_eq!(opus.effort(Some("xhigh")).as_deref(), Some("xhigh"));
        assert_eq!(opus.effort(Some("off")).as_deref(), Some("low"));
        assert_eq!(opus.effort(None).as_deref(), Some("high"));
        assert_eq!(opus.effort(Some("turbo")).as_deref(), Some("high"));
        let haiku4 = known_model(Api::AnthropicMessages, "claude-haiku-4-5").unwrap();
        assert_eq!(haiku4.effort(Some("max")).as_deref(), Some("high"));
        assert_eq!(haiku4.effort(Some("off")).as_deref(), Some("off"));
        let plain = ProviderModel {
            efforts: vec![],
            ..opus.clone()
        };
        assert_eq!(plain.effort(Some("high")), None);
    }

    #[test]
    fn model_references() {
        let known = |p: &str| p == "anthropic" || p == "relay";
        assert_eq!(
            split_model_ref("deepseek-flash", known),
            (PROVIDER, "deepseek-flash")
        );
        assert_eq!(
            split_model_ref("deepseek-official/deepseek-v4-pro", known),
            (PROVIDER, "deepseek-v4-pro")
        );
        assert_eq!(
            split_model_ref("anthropic/claude-opus-5-5", known),
            ("anthropic", "claude-opus-5-5")
        );
        assert_eq!(
            split_model_ref("relay/moonshotai/kimi-k3", known),
            ("relay", "moonshotai/kimi-k3")
        );
        // An unknown prefix stays a (DeepSeek) model id.
        assert_eq!(split_model_ref("gone/x", known), (PROVIDER, "gone/x"));
        assert_eq!(model_ref(PROVIDER, "deepseek-flash"), "deepseek-flash");
        assert_eq!(
            model_ref("anthropic", "claude-opus-5-5"),
            "anthropic/claude-opus-5-5"
        );
    }
}
