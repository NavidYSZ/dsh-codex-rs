//! The Team tools equal the request header of dsh 0.2.0-rc.2 with the
//! agent-team profile byte for byte, and the system prompt of that tool set
//! equals dsh's (`team:policy` at order 600, no `tool:subagent`).
use std::sync::Arc;

use dshx_agent::testing::{FixedApprover, MemoryAttachments, ScriptedLlm};
use dshx_subagent::{MemorySessions, RuntimeDeps, SubagentRuntime};
use dshx_team::{TOOL_NAMES, TeamService, TeamSettings, team_tools};
use serde_json::Value;

const UPSTREAM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/rust/upstream");

#[tokio::test]
async fn schemas_and_prompt_match_the_team_profile() {
    let sessions = Arc::new(MemorySessions::new());
    let runtime = SubagentRuntime::new(RuntimeDeps {
        llm: ScriptedLlm::new(vec![]),
        sessions: sessions.clone(),
        attachments: Arc::new(MemoryAttachments::default()),
        approver: Arc::new(FixedApprover(dshx_core::ApprovalDecision::Rejected)),
    });
    let teams = TeamService::new(TeamSettings::default(), runtime, sessions);
    let tools = team_tools(&teams);
    let names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
    assert_eq!(names, TOOL_NAMES);
    let header: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{UPSTREAM}/request-header-team-0.2.0-rc.2.json"))
            .unwrap(),
    )
    .unwrap();
    let wire = header["tools"].as_array().unwrap();
    for tool in &tools {
        let name = tool.schema().name;
        let expected = wire.iter().find(|t| t["name"] == name.as_str()).unwrap();
        assert_eq!(
            serde_json::to_string(&tool.schema()).unwrap(),
            expected.to_string(),
            "{name}"
        );
    }
    let all: Vec<String> = wire
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let rendered = dshx_agent::render_system_prompt(
        &dshx_agent::PromptConfig::default(),
        &all,
        "deepseek-official",
        "deepseek-flash",
        "/tmp/dsh-smoke",
    )
    .unwrap();
    let expected =
        std::fs::read_to_string(format!("{UPSTREAM}/system-prompt-team-0.2.0-rc.2.txt")).unwrap();
    assert_eq!(rendered, expected);
}
