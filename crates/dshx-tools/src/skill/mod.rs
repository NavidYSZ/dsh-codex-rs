//! Agent skills (dsh `skill`, `skill-filesystem`, `tool-skill`): discovery of
//! `SKILL.md` bundles and flat Markdown skills, the session skill catalog the
//! model sees, user `/name` invocations and the model-facing `skill` tool.
//!
//! # Wiring
//!
//! ```no_run
//! use std::sync::Arc;
//! use dshx_tools::skill::{SkillConfig, SkillRegistry, SkillTool};
//! let skills = Arc::new(SkillRegistry::new(SkillConfig::default()));
//! let mut registry = dshx_core::ToolRegistry::new();
//! registry.register(Arc::new(SkillTool::new(skills.clone())));
//! // Each step, after workspace instructions and the runtime context
//! // (see `SkillRegistry::pre_step`):
//! // entering = skills.pre_step(cwd, &claimed, entering, &history, registry.get("skill").is_some());
//! ```
//!
//! dsh registers the `skill` tool even when no skill exists; the catalog
//! message only appears once there is at least one model-invocable skill.
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dshx_core::{Message, Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

mod catalog;
mod discovery;
mod frontmatter;

pub use catalog::{
    CatalogEntry, CatalogHistory, apply_catalog, catalog_entries, catalog_message, escape_text,
    invocation_messages, invoked_skill_names, render_skill_content,
};
pub use discovery::{
    PROVIDER, ResourceBase, SkillConfig, SkillDefinition, SkillRegistry, SkillSnapshot,
    SkillSummary, discover_dir, is_skill_name,
};

/// Description of the `skill` tool, verbatim from dsh 0.2.0-rc.2.
pub const SKILL_TOOL_DESCRIPTION: &str = "Load the full instructions for a skill. Call it before acting on a task that names or clearly matches a skill in the session skill catalog.";

/// The `skill` tool schema exactly as dsh sends it.
pub fn skill_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "skill".into(),
        description: SKILL_TOOL_DESCRIPTION.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The exact skill name from the available skills list."
                }
            },
            "required": ["name"]
        }),
    }
}

impl SkillRegistry {
    /// Both `agent/pre-step` listeners of dsh `tool-skill`, in waterfall
    /// order: the catalog listener transforms `entering` (the batch after
    /// claimed input, workspace instructions and the runtime context), then
    /// one `skill-invocation` message is appended per `/name` gesture in
    /// `claimed` that names a user-invocable skill.
    ///
    /// `history` comes from [`CatalogHistory::from_events`] over the session
    /// log; `skill_tool_offered` is whether this registry's `skill` tool is in
    /// the agent's tool set (otherwise the catalog is withdrawn).
    pub fn pre_step(
        &self,
        cwd: &Path,
        claimed: &[Message],
        entering: Vec<Message>,
        history: &CatalogHistory,
        skill_tool_offered: bool,
    ) -> Vec<Message> {
        let snapshot = skill_tool_offered.then(|| self.snapshot(cwd));
        let mut messages = apply_catalog(
            entering,
            snapshot.as_ref(),
            self.config().catalog_description_max_length,
            history,
        );
        messages.extend(invocation_messages(self, cwd, claimed));
        messages
    }
}

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

/// The model-facing `skill` loader.
pub struct SkillTool {
    registry: Arc<SkillRegistry>,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        SkillTool { registry }
    }
}

/// Registration helper: the `skill` tool over a registry with `config`.
pub fn skill_tool(config: SkillConfig) -> (Arc<dyn Tool>, Arc<SkillRegistry>) {
    let registry = Arc::new(SkillRegistry::new(config));
    (Arc::new(SkillTool::new(registry.clone())), registry)
}

#[async_trait]
impl Tool for SkillTool {
    fn schema(&self) -> ToolSchema {
        skill_tool_schema()
    }

    fn describe(&self, args: &Value) -> String {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        format!("Load skill {name}")
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let violations = crate::args::validate(&skill_tool_schema().parameters, &args, "");
        if !violations.is_empty() {
            return Err(ToolError::InvalidArguments(format!(
                "invalid arguments: {}",
                violations.join("; ")
            )));
        }
        let args: SkillArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("invalid arguments: {e}")))?;
        let name = args.name;
        if !is_skill_name(&name) {
            return Err(ToolError::Failed(format!("invalid skill name \"{name}\"")));
        }
        let registry = self.registry.clone();
        let cwd = ctx.cwd.clone();
        let lookup = name.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let summary = registry.list(&cwd).into_iter().find(|s| s.name == lookup);
            let skill = summary
                .as_ref()
                .filter(|s| s.model_invocable)
                .and_then(|_| registry.get(&lookup, &cwd));
            (summary, skill)
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?;
        let unknown = || {
            ToolError::Failed(format!(
                "skill \"{name}\" is unknown or no longer available"
            ))
        };
        let refused = || {
            ToolError::Failed(format!(
                "skill \"{name}\" is not available for model invocation"
            ))
        };
        let skill = match loaded {
            (None, _) => return Err(unknown()),
            (Some(s), _) if !s.model_invocable => return Err(refused()),
            (Some(_), None) => return Err(unknown()),
            (Some(_), Some(skill)) if !skill.summary.model_invocable => return Err(refused()),
            (Some(_), Some(skill)) => skill,
        };
        let mut value = json!({"name": skill.summary.name, "provider": skill.summary.provider});
        value["resourceBase"] = skill.summary.resource_base.json();
        value["content"] = Value::String(skill.content.clone());
        Ok(ToolOutput {
            value: Some(value),
            ..ToolOutput::text(render_skill_content(&skill))
        })
    }
}
