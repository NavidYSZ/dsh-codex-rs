//! The core types must round-trip events written by the official dsh 0.2.x.
use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent};

const ASSISTANT: &str = r#"{"type":"assistant/message","seq":17,"time":1791919489579,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":""},{"type":"tool-call","id":"call_00_x","name":"bash","arguments":"{\"command\": \"hostname\"}"}],"source":{"kind":"model","provider":"deepseek-official","model":"deepseek-flash","replayState":{"response":{"kind":"deepseek-messages","version":1}}},"id":"m1"}}}"#;
const TOOL: &str = r#"{"type":"tool/result","seq":19,"time":1791919489754,"data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"call_00_x"},"toolCallId":"call_00_x","content":[{"type":"text","text":"host\n"}],"isError":false,"id":"fb20"}},"sourceEventSeqs":[18]}"#;

#[test]
fn reads_dsh_assistant_and_tool_events() {
    let e: SessionEvent = serde_json::from_str(ASSISTANT).unwrap();
    let m: Message = serde_json::from_value(e.data["message"].clone()).unwrap();
    assert_eq!(m.role, Role::Assistant);
    assert!(matches!(&m.content[1], ContentBlock::ToolCall { name, .. } if name == "bash"));
    assert!(matches!(
        &m.source,
        MessageSource::Model {
            replay_state: Some(_),
            ..
        }
    ));

    let e: SessionEvent = serde_json::from_str(TOOL).unwrap();
    assert_eq!(e.extra["sourceEventSeqs"], serde_json::json!([18]));
    let m: Message = serde_json::from_value(e.data["message"].clone()).unwrap();
    assert_eq!(m.tool_call_id.as_deref(), Some("call_00_x"));
    assert_eq!(m.is_error, Some(false));
    // Round trip keeps every field.
    let back = serde_json::to_value(&m).unwrap();
    assert_eq!(back, e.data["message"]);
}

#[test]
fn unknown_blocks_survive() {
    let v = serde_json::json!({"type": "future-block", "x": 1});
    let b: ContentBlock = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&b).unwrap(), v);
}
