//! Skills against references captured from the official dsh 0.2.0-rc.2
//! (`tests/golden/skills.json`): the fixture tree under `tests/fixtures/skills`
//! was discovered by `dsh --profile headless` (with `DSH_HOME` and
//! `DSH_AGENTS_HOME` pointing at `dshhome` and `agentshome`, cwd `proj/sub`)
//! against a scripted mock that called `skill` six times; a resumed session
//! after adding `epsilon` and removing `delta` produced the catalog update.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dshx_core::{ContentBlock, Message, MessageSource, SessionEvent, Tool, ToolError};
use dshx_tools::skill::{
    CatalogHistory, SkillConfig, SkillRegistry, SkillTool, apply_catalog, catalog_message,
    skill_tool_schema,
};
use serde_json::{Value, json};

fn golden() -> Value {
    serde_json::from_str(include_str!("golden/skills.json")).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    cwd: PathBuf,
    registry: Arc<SkillRegistry>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills"),
            &root,
        );
        std::fs::create_dir_all(root.join("proj/.git")).unwrap();
        let config = SkillConfig {
            include_default_roots: true,
            harness_home: Some(root.join("dshx")),
            dsh_home: Some(root.join("dshhome")),
            agents_home: Some(root.join("agentshome")),
            custom_dirs: vec![],
            bundled_dir: None,
            catalog_description_max_length: 500,
        };
        Fixture {
            cwd: root.join("proj/sub"),
            root,
            _dir: dir,
            registry: Arc::new(SkillRegistry::new(config)),
        }
    }

    fn expand(&self, text: &str) -> String {
        text.replace("{ROOT}", &self.root.to_string_lossy())
    }
}

fn text_of(message: &Message) -> &str {
    match &message.content[..] {
        [ContentBlock::Text { text }] => text,
        other => panic!("expected one text block, got {other:?}"),
    }
}

fn source_of(message: &Message) -> &Value {
    match &message.source {
        MessageSource::Other(v) => v,
        other => panic!("unexpected source {other:?}"),
    }
}

/// Model-visible text of a tool outcome, as the agent renders it.
fn rendered(result: Result<dshx_core::ToolOutput, ToolError>) -> (bool, String) {
    match result {
        Ok(out) => match &out.content[..] {
            [dshx_core::ToolResultPart::Text { text }] => (out.is_error, text.clone()),
            other => panic!("unexpected content {other:?}"),
        },
        Err(e) => (true, format!("Error: {e}")),
    }
}

fn event(seq: u64, message: &Message) -> SessionEvent {
    SessionEvent {
        kind: "user/message".into(),
        seq,
        time: 0,
        data: serde_json::to_value(message).unwrap(),
        extra: Default::default(),
    }
}

#[test]
fn discovery_follows_dsh_precedence_and_validation() {
    let f = Fixture::new();
    let snapshot = f.registry.snapshot(&f.cwd);
    assert!(snapshot.complete);
    let names: Vec<_> = snapshot.skills.iter().map(|s| s.name.as_str()).collect();
    // broken (no frontmatter), Bad_Name, legacy key, notes.txt and .system are skipped.
    assert_eq!(
        names,
        ["alpha", "beta", "delta", "gamma", "hidden", "quiet"]
    );
    let alpha = &snapshot.skills[0];
    assert_eq!(alpha.description, "Alpha skill for testing the loader.");
    assert_eq!(alpha.source, "project-dsh");
    assert_eq!(alpha.when_to_use.as_deref(), Some("when alpha"));
    assert_eq!(alpha.metadata.as_ref().unwrap()["owner"], "tests");
    let hidden = snapshot.skills.iter().find(|s| s.name == "hidden").unwrap();
    assert!(!hidden.model_invocable && hidden.user_invocable);
    let quiet = snapshot.skills.iter().find(|s| s.name == "quiet").unwrap();
    assert!(quiet.model_invocable && !quiet.user_invocable);
    assert_eq!(
        snapshot
            .skills
            .iter()
            .find(|s| s.name == "gamma")
            .unwrap()
            .source,
        "user-dsh"
    );
}

#[test]
fn first_catalog_matches_dsh() {
    let f = Fixture::new();
    let g = golden();
    let entering = vec![Message::user_text("x")];
    let out = f
        .registry
        .pre_step(&f.cwd, &[], entering, &CatalogHistory::default(), true);
    assert_eq!(out.len(), 2);
    assert_eq!(text_of(&out[1]), g["catalog"].as_str().unwrap());
    let source = source_of(&out[1]);
    let keys: Vec<_> = source.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["kind", "form", "entries"]);
    assert_eq!(source["kind"], "skill-catalog");
    assert_eq!(source["entries"].as_array().unwrap().len(), 5);
    assert_eq!(
        source["entries"][1],
        json!({"name": "beta", "description": "Beta handles <tags> & stuff across lines"})
    );
}

#[test]
fn user_gesture_injects_skill_content_after_the_catalog() {
    let f = Fixture::new();
    let g = golden();
    let prompt = Message::user_text(g["prompt"].as_str().unwrap());
    let out = f.registry.pre_step(
        &f.cwd,
        std::slice::from_ref(&prompt),
        vec![prompt.clone()],
        &CatalogHistory::default(),
        true,
    );
    // prompt, catalog, gamma (quiet is not user-invocable)
    assert_eq!(out.len(), 3);
    assert_eq!(
        source_of(&out[1])["kind"],
        "skill-catalog",
        "catalog precedes invocations"
    );
    assert_eq!(
        text_of(&out[2]),
        f.expand(g["invocation_gamma"].as_str().unwrap())
    );
    assert_eq!(
        source_of(&out[2]),
        &json!({"kind": "skill-invocation", "name": "gamma", "form": "instructions"})
    );
}

#[tokio::test]
async fn skill_tool_results_match_dsh() {
    let f = Fixture::new();
    let g = golden();
    let tool = SkillTool::new(f.registry.clone());
    for case in g["tool_results"].as_array().unwrap() {
        let ctx = dshx_tools::testing::context(&f.cwd);
        let (is_error, text) = rendered(tool.execute(case["input"].clone(), ctx).await);
        assert_eq!(is_error, case["is_error"].as_bool().unwrap(), "{case}");
        assert_eq!(text, f.expand(case["text"].as_str().unwrap()), "{case}");
    }
    let ctx = dshx_tools::testing::context(&f.cwd);
    let out = tool.execute(json!({"name": "alpha"}), ctx).await.unwrap();
    let value = out.value.unwrap();
    assert_eq!(value["provider"], "filesystem");
    assert_eq!(value["resourceBase"]["kind"], "directory");
    assert_eq!(
        value["content"],
        "# Alpha\n\nDo the alpha thing. See ./ref.md."
    );
    assert_eq!(tool.describe(&json!({"name": "alpha"})), "Load skill alpha");
}

#[test]
fn tool_schema_is_upstream() {
    let g = golden();
    let ours = serde_json::to_value(skill_tool_schema()).unwrap();
    assert_eq!(ours["name"], g["tool_schema"]["name"]);
    assert_eq!(ours["description"], g["tool_schema"]["description"]);
    assert_eq!(
        serde_json::to_string(&ours["parameters"]).unwrap(),
        serde_json::to_string(&g["tool_schema"]["input_schema"]).unwrap()
    );
    let header: Value = serde_json::from_str(include_str!(
        "../../../docs/rust/upstream/request-header-0.2.0-rc.2.json"
    ))
    .unwrap();
    let upstream = header["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "skill")
        .unwrap();
    assert_eq!(
        serde_json::to_string(&ours).unwrap(),
        serde_json::to_string(upstream).unwrap()
    );
}

#[test]
fn catalog_update_after_skills_change_matches_dsh() {
    let f = Fixture::new();
    let g = golden();
    let first = f
        .registry
        .pre_step(&f.cwd, &[], vec![], &CatalogHistory::default(), true);
    let events = vec![event(10, &first[0])];
    let history = CatalogHistory::from_events(&events, |seq| seq == 10);
    assert!(history.published && history.visible_digest.is_some());
    // Unchanged skills: nothing new.
    let again = f.registry.pre_step(&f.cwd, &[], vec![], &history, true);
    assert!(again.is_empty());
    // Change the skill set like the captured run did.
    let epsilon = f.root.join("proj/.dsh/skills/epsilon");
    std::fs::create_dir_all(&epsilon).unwrap();
    std::fs::write(
        epsilon.join("SKILL.md"),
        "---\nname: epsilon\ndescription: Epsilon added later.\n---\nEpsilon body\n",
    )
    .unwrap();
    std::fs::remove_dir_all(f.root.join("agentshome/skills/delta")).unwrap();
    let continued = Message::user_text("continue");
    let out = f
        .registry
        .pre_step(&f.cwd, &[], vec![continued], &history, true);
    assert_eq!(out.len(), 2);
    assert_eq!(text_of(&out[1]), g["catalog_update"].as_str().unwrap());
    assert_eq!(source_of(&out[1])["update"], true);
}

#[test]
fn catalog_republishes_when_compacted_and_withdraws_without_the_tool() {
    let f = Fixture::new();
    let first = f
        .registry
        .pre_step(&f.cwd, &[], vec![], &CatalogHistory::default(), true);
    let events = vec![event(3, &first[0])];
    // Compaction removed the catalog from the surface: an update re-publishes it.
    let hidden = CatalogHistory::from_events(&events, |_| false);
    assert!(hidden.published && hidden.visible_digest.is_none());
    let out = f.registry.pre_step(&f.cwd, &[], vec![], &hidden, true);
    assert_eq!(source_of(&out[0])["update"], true);
    // The skill tool is no longer offered: an empty replacement catalog.
    let visible = CatalogHistory::from_events(&events, |_| true);
    let out = f.registry.pre_step(&f.cwd, &[], vec![], &visible, false);
    assert!(
        text_of(&out[0]).contains("No skills are currently available through the `skill` tool.")
    );
    // Never published and nothing to show: no message at all.
    let none = apply_catalog(vec![], None, 500, &CatalogHistory::default());
    assert!(none.is_empty());
    // An identical catalog already in the batch is kept as is.
    let pending = catalog_message(
        &dshx_tools::skill::catalog_entries(&f.registry.snapshot(&f.cwd), 500),
        false,
    );
    let kept = f.registry.pre_step(
        &f.cwd,
        &[],
        vec![pending.clone()],
        &CatalogHistory::default(),
        true,
    );
    assert_eq!(kept, vec![pending]);
}

#[test]
fn dsh_codex_home_outranks_dsh_home() {
    let f = Fixture::new();
    let gamma = f.root.join("dshx/skills/gamma");
    std::fs::create_dir_all(&gamma).unwrap();
    std::fs::write(
        gamma.join("SKILL.md"),
        "---\nname: gamma\ndescription: From dsh-codex.\n---\nbody\n",
    )
    .unwrap();
    let skill = f.registry.get("gamma", &f.cwd).unwrap();
    assert_eq!(skill.summary.description, "From dsh-codex.");
    assert_eq!(skill.summary.source, "user-dsh-codex");
}
