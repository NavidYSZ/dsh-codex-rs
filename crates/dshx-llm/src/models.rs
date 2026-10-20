//! DeepSeek model catalog (dsh `llm-deepseek/src/{models,model-info}.ts`).
use dshx_core::ModelInfo;
use serde::{Deserialize, Serialize};

/// Provider route name recorded in assistant message sources.
pub const PROVIDER: &str = "deepseek-official";
/// Combined request/response capacity of the official models.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
/// Default per-request output cap.
pub const DEFAULT_MAX_TOKENS: u32 = 256_000;
/// Reasoning effort ids, from least to greatest.
pub const REASONING_EFFORTS: [&str; 4] = ["off", "low", "high", "max"];

/// The endpoint reads the latest `system` message anywhere in `messages` as
/// the effective system prompt, so a changed prompt can follow cached history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemPromptUpdate {
    InHistory,
}

/// How the endpoint accepts tool declarations that change mid-conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolUpdate {
    /// `tool_addition` and `tool_removal` blocks are read in order.
    InHistory,
    /// Only `tool_addition` activates a `defer_loading` tool.
    AdditionOnly,
}

/// One advertised model (dsh `DeepSeekCatalogModel`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// `["text"]` or `["text", "image"]`.
    pub input_modalities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_update: Option<SystemPromptUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_update: Option<ToolUpdate>,
}

impl CatalogModel {
    pub fn accepts_images(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "image")
    }
}

/// The built-in catalog: DeepSeek-V4.1-Flash (text + image) and DeepSeek-V4-Pro (text).
pub fn default_models() -> Vec<CatalogModel> {
    vec![
        CatalogModel {
            id: "deepseek-flash".into(),
            name: Some("DeepSeek-V4.1-Flash".into()),
            description: None,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: None,
            input_modalities: vec!["text".into(), "image".into()],
            system_prompt_update: Some(SystemPromptUpdate::InHistory),
            tool_update: Some(ToolUpdate::AdditionOnly),
        },
        CatalogModel {
            id: "deepseek-v4-pro".into(),
            name: Some("DeepSeek-V4-Pro".into()),
            description: Some(
                "Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."
                    .into(),
            ),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: None,
            input_modalities: vec!["text".into()],
            system_prompt_update: None,
            tool_update: None,
        },
    ]
}

/// Everything the agent needs to know about one exact model route
/// (dsh `LlmResolvedModelInfo`). Uncatalogued ids resolve as text-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModel {
    #[serde(flatten)]
    pub info: ModelInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Output cap materialized when a request names none.
    pub default_max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_update: Option<SystemPromptUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_update: Option<ToolUpdate>,
}

/// Selector metadata for one catalog entry (dsh `catalogModelInfo` + `modelInfo`).
pub(crate) fn resolve(
    catalog: Option<&CatalogModel>,
    id: &str,
    default_context_window: u64,
    max_tokens: u32,
    thinking_disabled: bool,
    default_effort: Option<&str>,
) -> ResolvedModel {
    let (efforts, default) = if thinking_disabled {
        (vec!["off".to_string()], "off".to_string())
    } else {
        let default = match default_effort {
            Some(e @ ("off" | "low" | "max")) => e.to_string(),
            _ => "high".to_string(),
        };
        (
            REASONING_EFFORTS.iter().map(|e| e.to_string()).collect(),
            default,
        )
    };
    ResolvedModel {
        info: ModelInfo {
            id: id.to_string(),
            name: catalog
                .and_then(|m| m.name.clone())
                .unwrap_or_else(|| id.to_string()),
            context_window: catalog
                .and_then(|m| m.context_window)
                .unwrap_or(default_context_window),
            input_modalities: catalog
                .map(|m| m.input_modalities.clone())
                .unwrap_or_else(|| vec!["text".into()]),
            reasoning_efforts: efforts,
            default_reasoning_effort: Some(default),
        },
        description: catalog.and_then(|m| m.description.clone()),
        default_max_tokens: catalog.and_then(|m| m.max_tokens).unwrap_or(max_tokens),
        system_prompt_update: catalog.and_then(|m| m.system_prompt_update),
        tool_update: catalog.and_then(|m| m.tool_update),
    }
}
