//! Every tool's name, description and schema equals the upstream request header
//! of dsh 0.2.0-rc.2, byte for byte (key order included).
use dshx_core::config::Config;
use serde_json::Value;

fn upstream() -> Vec<Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/request-header-0.2.0-rc.2.json"
    );
    let header: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    header["tools"].as_array().unwrap().clone()
}

#[test]
fn schemas_match_the_upstream_header() {
    let upstream = upstream();
    let tools = dshx_tools::default_tools(&Config::default());
    assert_eq!(tools.len(), 13);
    for tool in &tools {
        let schema = tool.schema();
        let expected = upstream
            .iter()
            .find(|t| t["name"] == schema.name.as_str())
            .unwrap_or_else(|| panic!("{} missing upstream", schema.name));
        let ours = serde_json::to_value(&schema).unwrap();
        assert_eq!(&ours, expected, "schema of {}", schema.name);
        // Byte-identical serialization, so key order matches too.
        assert_eq!(
            serde_json::to_string(&ours).unwrap(),
            serde_json::to_string(expected).unwrap(),
            "key order of {}",
            schema.name
        );
    }
}

#[test]
fn tools_come_in_request_order() {
    let names: Vec<String> = dshx_tools::default_tools(&Config::default())
        .iter()
        .map(|t| t.schema().name)
        .collect();
    let upstream_order: Vec<String> = upstream()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .filter(|name| dshx_tools::TOOL_NAMES.contains(&name.as_str()))
        .collect();
    assert_eq!(names, upstream_order);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn concurrency_and_timeouts_follow_dsh() {
    let tools = dshx_tools::default_tools(&Config::default());
    let safe: Vec<String> = tools
        .iter()
        .filter(|t| t.concurrency_safe())
        .map(|t| t.schema().name)
        .collect();
    assert_eq!(
        safe,
        [
            "glob",
            "grep",
            "read",
            "read_image",
            "web_fetch",
            "web_search"
        ]
    );
    let timed: Vec<(String, u128)> = tools
        .iter()
        .filter_map(|t| t.timeout().map(|d| (t.schema().name, d.as_millis())))
        .collect();
    assert_eq!(
        timed,
        [
            ("glob".into(), 30_000),
            ("grep".into(), 30_000),
            ("web_fetch".into(), 30_000)
        ]
    );
}
