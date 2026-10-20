//! Tool names, descriptions and argument schemas, verbatim from the request
//! header of dsh 0.2.0-rc.2 (`docs/rust/upstream/request-header-0.2.0-rc.2.json`).
//! Keeping them byte-identical keeps DeepSeek's prefix cache and the model's
//! behaviour the same as under dsh.
use std::sync::OnceLock;

use dshx_core::ToolSchema;
use serde_json::Value;

const SCHEMAS_JSON: &str = include_str!("schemas.json");

fn all() -> &'static [ToolSchema] {
    static SCHEMAS: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    SCHEMAS.get_or_init(|| serde_json::from_str(SCHEMAS_JSON).expect("embedded tool schemas parse"))
}

/// The upstream schema of one of this crate's tools.
///
/// # Panics
/// When `name` is not one of the thirteen tools this crate implements.
pub fn upstream_schema(name: &str) -> ToolSchema {
    all()
        .iter()
        .find(|schema| schema.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no upstream schema for tool {name}"))
}

/// The argument schema (`parameters`) of one tool.
pub(crate) fn parameters(name: &str) -> &'static Value {
    &all()
        .iter()
        .find(|schema| schema.name == name)
        .unwrap_or_else(|| panic!("no upstream schema for tool {name}"))
        .parameters
}

/// Names of every tool this crate implements, in the request order dsh uses
/// (tools are sent sorted by name).
pub const TOOL_NAMES: [&str; 13] = [
    "bash",
    "edit",
    "glob",
    "grep",
    "job_kill",
    "job_list",
    "job_output",
    "read",
    "read_image",
    "todo_write",
    "web_fetch",
    "web_search",
    "write",
];
