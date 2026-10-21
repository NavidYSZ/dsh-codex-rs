//! The goal tools' names, descriptions and schemas equal dsh 0.2.0-rc.2 byte
//! for byte (the captured request header and the compiled `dsh-tool-goal`
//! definitions), and so does the `tool:goal` prompt section.
use dshx_goal::{GoalService, GoalSettings, TOOL_NAMES, goal_tools};
use serde_json::Value;

const UPSTREAM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/rust/upstream");

fn read(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{UPSTREAM}/{path}")).unwrap()).unwrap()
}

#[test]
fn schemas_match_the_upstream_header_and_compiled_definitions() {
    let goals = GoalService::new(GoalSettings::default());
    let tools = goal_tools(&goals);
    let mut names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
    names.sort();
    assert_eq!(names, TOOL_NAMES);
    let header = read("request-header-0.2.0-rc.2.json");
    let compiled = read("tools-0.2.0-rc.2/goal.json");
    for tool in &tools {
        let ours = serde_json::to_string(&tool.schema()).unwrap();
        let name = tool.schema().name;
        let wire = header["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name.as_str())
            .unwrap();
        assert_eq!(ours, wire.to_string(), "{name} vs request header");
        let def = compiled["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name.as_str())
            .unwrap();
        let def = serde_json::json!({"name": def["name"], "description": def["description"], "parameters": def["parameters"]});
        assert_eq!(ours, def.to_string(), "{name} vs compiled definition");
        assert!(!tool.concurrency_safe());
    }
    let section = &compiled["sections"][0];
    assert_eq!(section["name"], "tool:goal");
    assert_eq!(section["order"], 2400);
    let prompt = dshx_agent::render_system_prompt(
        &dshx_agent::PromptConfig::default(),
        &["create_goal".to_owned()],
        "deepseek-official",
        "deepseek-flash",
        "/w",
    )
    .unwrap();
    assert!(prompt.contains(section["text"].as_str().unwrap()));
    let five = dshx_agent::PromptConfig {
        goal_blocked_after: 5,
        ..Default::default()
    };
    let prompt =
        dshx_agent::render_system_prompt(&five, &["create_goal".to_owned()], "p", "m", "/w")
            .unwrap();
    assert!(prompt.contains("persists for at least 5 consecutive goal rounds"));
}
