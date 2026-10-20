//! The five tools' names, descriptions and schemas equal the request header
//! of dsh 0.2.0-rc.2 byte for byte (key order included).
use dshx_subagent::{TOOL_NAMES, subagent_tools};
use serde_json::Value;

fn upstream() -> Vec<Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/request-header-0.2.0-rc.2.json"
    );
    let header: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    header["tools"].as_array().unwrap().clone()
}

#[tokio::test]
async fn schemas_match_the_upstream_header() {
    let deps = dshx_subagent::RuntimeDeps {
        llm: dshx_agent::testing::ScriptedLlm::new(vec![]),
        sessions: std::sync::Arc::new(dshx_subagent::MemorySessions::new()),
        attachments: std::sync::Arc::new(dshx_agent::testing::MemoryAttachments::default()),
        approver: std::sync::Arc::new(dshx_agent::testing::FixedApprover(
            dshx_core::ApprovalDecision::Rejected,
        )),
    };
    let runtime = dshx_subagent::SubagentRuntime::new(deps);
    let tools = subagent_tools(runtime);
    let names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
    assert_eq!(names, TOOL_NAMES);
    let upstream = upstream();
    for tool in &tools {
        let schema = tool.schema();
        let expected = upstream
            .iter()
            .find(|t| t["name"] == schema.name.as_str())
            .unwrap_or_else(|| panic!("{} missing upstream", schema.name));
        let ours = serde_json::to_value(&schema).unwrap();
        assert_eq!(
            serde_json::to_string(&ours).unwrap(),
            serde_json::to_string(expected).unwrap(),
            "schema of {}",
            schema.name
        );
    }
    // Only the delegation tools may run in parallel (dsh `isConcurrencySafe`).
    let parallel: Vec<String> = tools
        .iter()
        .filter(|t| t.concurrency_safe())
        .map(|t| t.schema().name)
        .collect();
    assert_eq!(parallel, ["subagent", "subagent_fork"]);
}
