//! Agent presets: Standard leaves the configuration alone; Minimal sends
//! dsh's fixed persona as the whole system prompt and composes no runtime
//! context, workspace instructions or compaction.
mod common;

use common::*;
use dshx_agent::testing::text;
use dshx_agent::{AgentConfig, Preset, PromptConfig, render_system_prompt};
use dshx_core::Role;
use serde_json::json;

fn golden_minimal_prompt() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust/upstream/system-prompt-minimal-0.2.0-rc.2.txt"
    ))
    .unwrap()
}

#[test]
fn preset_ids() {
    assert_eq!(Preset::parse("minimal"), Some(Preset::Minimal));
    assert_eq!(Preset::parse("standard"), Some(Preset::Standard));
    assert_eq!(Preset::parse("ptc"), None);
    assert_eq!(Preset::of_session(None), Preset::Standard);
    assert_eq!(Preset::of_session(Some("cordis")), Preset::Standard);
    assert_eq!(Preset::of_session(Some("minimal")), Preset::Minimal);
    assert_eq!(
        serde_json::to_value(Preset::Minimal).unwrap(),
        json!("minimal")
    );
}

#[test]
fn standard_changes_nothing() {
    let plain = AgentConfig::new("deepseek-flash", "/w");
    let mut standard = plain.clone();
    standard.apply_preset(Preset::Standard);
    assert_eq!(format!("{standard:?}"), format!("{plain:?}"));
    assert_eq!(standard.prompt, PromptConfig::default());
}

#[test]
fn minimal_prompt_is_the_fixed_persona_for_any_tools() {
    let mut config = AgentConfig::new("deepseek-flash", "/w");
    config.apply_preset(Preset::Minimal);
    for tools in [
        vec![],
        vec!["bash".to_owned()],
        vec!["bash".into(), "read".into(), "subagent".into()],
    ] {
        let prompt = render_system_prompt(
            &config.prompt,
            &tools,
            "deepseek-official",
            "deepseek-flash",
            "/w",
        )
        .unwrap();
        assert_eq!(prompt, golden_minimal_prompt());
    }
    assert!(!config.instructions.enabled);
    assert!(!config.compaction.auto);
    assert!(!config.pruner.enabled);
    assert!(config.title.enabled, "titles are a host service in dsh");
    assert!(
        config.repeat_reminder.enabled,
        "so is the repeat-tool reminder"
    );
}

#[tokio::test]
async fn minimal_requests_carry_no_runtime_context_or_instructions() {
    let h = harness(vec![text("deepseek-flash", "done")], vec![], |c, _| {
        c.apply_preset(Preset::Minimal)
    });
    // An AGENTS.md that Standard loads (see below).
    let cwd = h.agent.config().cwd;
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Always say hi.").unwrap();
    h.agent
        .run_turn(vec![dshx_core::ContentBlock::text("Say FERTIG.")])
        .await
        .unwrap();
    let requests = h.llm.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let messages = &requests[0].messages;
    assert_eq!(messages.len(), 2, "{messages:#?}");
    assert_eq!(messages[0].role, Role::System);
    assert_eq!(messages[0].plain_text(), golden_minimal_prompt());
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(messages[1].plain_text(), "Say FERTIG.");
    assert!(
        of_kind(&h.log, "user/message")
            .iter()
            .all(|e| e.data["source"]["kind"] == "user")
    );
    validate(&h.log);

    // The same turn under Standard: instructions and runtime context enter.
    let standard = harness(vec![text("deepseek-flash", "done")], vec![], |_, _| {});
    let cwd = standard.agent.config().cwd;
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Always say hi.").unwrap();
    standard
        .agent
        .run_turn(vec![dshx_core::ContentBlock::text("Say FERTIG.")])
        .await
        .unwrap();
    let requests = standard.llm.requests.lock().unwrap().clone();
    let texts: Vec<String> = requests[0]
        .messages
        .iter()
        .map(|m| m.plain_text())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("Always say hi.")),
        "{texts:#?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Current runtime context.")),
        "{texts:#?}"
    );
}
