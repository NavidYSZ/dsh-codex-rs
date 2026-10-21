//! The schedule tools equal the compiled `dsh-schedule` 0.2.0-rc.2
//! definitions byte for byte (key order included).
use std::sync::Arc;

use dshx_schedule::{Retention, ScheduleService, TOOL_NAMES, schedule_tools};
use serde_json::{Value, json};

#[test]
fn schemas_match_the_compiled_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let service: Arc<ScheduleService> =
        ScheduleService::open(dir.path(), Retention::default(), None).unwrap();
    let tools = schedule_tools(&service);
    let names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
    assert_eq!(names, TOOL_NAMES);
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/tools-0.2.0-rc.2/schedule.json"
    );
    let compiled: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for tool in &tools {
        let name = tool.schema().name;
        let def = compiled["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name.as_str())
            .unwrap();
        let def = json!({"name": def["name"], "description": def["description"], "parameters": def["parameters"]});
        assert_eq!(
            serde_json::to_string(&tool.schema()).unwrap(),
            def.to_string(),
            "{name}"
        );
    }
    // dsh registers no prompt section for schedules.
    assert_eq!(compiled["sections"], json!([]));
}
