//! Plugin folders in the Codex, Claude, Agent Plugins and bare layouts, and
//! marketplaces from a local folder and a local git repository.
use std::path::{Path, PathBuf};
use std::process::Command;

use dshx_plugins::{ManifestFormat, PluginHome, PluginSource, SourceKind, load_plugin};
use serde_json::{Value, json};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn json_file(path: &Path, value: Value) {
    write(path, &serde_json::to_string_pretty(&value).unwrap());
}

fn skill(dir: &Path, name: &str) {
    write(
        &dir.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\nDo {name}.\n"),
    );
}

/// A Codex plugin: `skills` replaces `skills/`, MCP from `.mcp.json`, an
/// `interface` with brand color and logo.
fn codex_plugin(root: &Path) {
    json_file(
        &root.join(".codex-plugin/plugin.json"),
        json!({
            "name": "linear",
            "version": "1.2.0",
            "description": "Plan and track work in Linear.",
            "author": {"name": "OpenAI"},
            "skills": "./custom-skills/",
            "mcpServers": "./.mcp.json",
            "interface": {
                "displayName": "Linear",
                "shortDescription": "Issues, projects and cycles",
                "brandColor": "#5E6AD2",
                "category": "Productivity",
                "logo": "./assets/logo.png",
                "composerIcon": "./assets/missing.png"
            }
        }),
    );
    skill(&root.join("skills"), "ignored-default");
    skill(&root.join("custom-skills"), "triage");
    write(&root.join("assets/logo.png"), "png");
    json_file(
        &root.join(".mcp.json"),
        json!({"mcpServers": {
            "linear": {"type": "http", "url": "https://mcp.linear.app/mcp", "http_headers": {"X-Client": "dsh"}},
            "local": {"command": "${PLUGIN_ROOT}/bin/srv", "args": ["--data", "${CLAUDE_PLUGIN_DATA}"], "tool_timeout_sec": 5}
        }}),
    );
}

/// A Claude plugin: `skills` adds to `skills/`, `.mcp.json` plus inline
/// `mcpServers`, unsupported components.
fn claude_plugin(root: &Path) {
    json_file(
        &root.join(".claude-plugin/plugin.json"),
        json!({
            "name": "notes",
            "version": "0.3.0",
            "description": "Notes helpers.",
            "author": {"name": "Ann"},
            "homepage": "https://notes.example",
            "skills": ["./extra-skills"],
            "mcpServers": {
                "files": {"command": "python3", "args": ["${CLAUDE_PLUGIN_ROOT}/srv.py"], "env": {"A": "1"}},
                "legacy": {"type": "sse", "url": "https://example.com/sse"}
            }
        }),
    );
    skill(&root.join("skills"), "note-taking");
    skill(&root.join("extra-skills"), "summarize");
    json_file(
        &root.join(".mcp.json"),
        json!({"mcpServers": {"web": {"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer ${DSHX_TEST_UNSET_TOKEN}"}}}}),
    );
    write(&root.join("commands/hello.md"), "# hello");
}

#[test]
fn codex_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("linear");
    codex_plugin(&root);
    let data = dir.path().join("data");
    let p = load_plugin(&root, "linear", "openai-curated", &data);
    assert_eq!(p.id, "linear@openai-curated");
    assert_eq!(p.format, ManifestFormat::Codex);
    assert!(p.error.is_none(), "{:?}", p.error);
    assert_eq!(p.version.as_deref(), Some("1.2.0"));
    assert_eq!(p.skill_dirs, [root.join("custom-skills")]);
    assert_eq!(p.interface["displayName"], "Linear");
    assert_eq!(p.interface["brandColor"], "#5E6AD2");
    assert_eq!(p.interface["logo"], "assets/logo.png");
    assert_eq!(p.interface["developerName"], "OpenAI");
    assert!(p.interface.get("composerIcon").is_none());
    assert!(
        p.warnings.iter().any(|w| w.contains("missing.png")),
        "{:?}",
        p.warnings
    );
    let names: Vec<&str> = p.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["linear", "linear_local"]);
    let http = &p.mcp_servers[0].config;
    assert_eq!(http.transport.as_deref(), Some("streamable-http"));
    assert_eq!(http.url.as_deref(), Some("https://mcp.linear.app/mcp"));
    assert_eq!(http.headers["X-Client"], "dsh");
    let stdio = &p.mcp_servers[1].config;
    assert_eq!(
        stdio.command.as_deref(),
        Some(root.join("bin/srv").to_str().unwrap())
    );
    assert_eq!(stdio.args, ["--data", data.to_str().unwrap()]);
    assert_eq!(stdio.cwd.as_deref(), Some(root.as_path()));
    assert_eq!(stdio.env["CLAUDE_PLUGIN_ROOT"], root.display().to_string());
    assert_eq!(stdio.tool_call_timeout_ms, 5000);
    // The listing shape the UI gets.
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["format"], "codex");
    assert_eq!(v["mcpServers"][1]["originalName"], "local");
}

#[test]
fn claude_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("notes");
    claude_plugin(&root);
    let p = load_plugin(&root, "notes", "local", &dir.path().join("data"));
    assert_eq!(p.format, ManifestFormat::Claude);
    assert_eq!(
        p.skill_dirs,
        [root.join("skills"), root.join("extra-skills")]
    );
    assert_eq!(p.interface["displayName"], "notes");
    assert_eq!(p.interface["shortDescription"], "Notes helpers.");
    assert_eq!(p.interface["developerName"], "Ann");
    assert_eq!(p.interface["websiteURL"], "https://notes.example");
    let by_name = |n: &str| p.mcp_servers.iter().find(|s| s.name == n).unwrap();
    let files = by_name("notes_files");
    assert_eq!(files.config.args, [format!("{}/srv.py", root.display())]);
    assert_eq!(files.config.env["A"], "1");
    assert!(
        by_name("notes_legacy")
            .error
            .as_ref()
            .unwrap()
            .contains("SSE")
    );
    let web = by_name("notes_web");
    assert_eq!(web.config.headers["Authorization"], "Bearer ");
    assert!(web.warnings[0].contains("DSHX_TEST_UNSET_TOKEN"));
    assert!(p.warnings.iter().any(|w| w.contains("commands")));
}

#[test]
fn bare_layout_and_agent_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare");
    skill(&bare.join("skills"), "x");
    json_file(
        &bare.join("mcp.json"),
        json!({"srv": {"command": "node", "args": ["./index.js"]}}),
    );
    let p = load_plugin(&bare, "bare", "local", dir.path());
    assert_eq!(p.format, ManifestFormat::Layout);
    assert_eq!(p.skill_dirs, [bare.join("skills")]);
    assert_eq!(p.mcp_servers[0].name, "bare_srv");
    assert_eq!(p.interface["displayName"], "bare");

    let agent = dir.path().join("agent");
    json_file(
        &agent.join("plugin.json"),
        json!({
            "$schema": "https://agentplugins.dev/schemas/1.0/plugin.schema.json",
            "name": "agent",
            "skills": ["./s"],
            "interface": {"displayName": "Agent Kit"}
        }),
    );
    skill(&agent.join("skills"), "replaced");
    skill(&agent.join("s"), "kept");
    let p = load_plugin(&agent, "agent", "local", dir.path());
    assert_eq!(p.format, ManifestFormat::AgentPlugins);
    assert_eq!(p.skill_dirs, [agent.join("s")]);

    // A root plugin.json without the schema is no manifest.
    let other = dir.path().join("other");
    json_file(&other.join("plugin.json"), json!({"name": "x"}));
    assert_eq!(
        load_plugin(&other, "other", "local", dir.path()).format,
        ManifestFormat::Layout
    );

    let broken = dir.path().join("broken");
    write(&broken.join(".codex-plugin/plugin.json"), "{nope");
    let p = load_plugin(&broken, "broken", "local", dir.path());
    assert!(p.error.unwrap().contains("invalid JSON"));
}

/// A Claude marketplace in a local folder with a relative, a
/// `pluginRoot`-relative and a `strict: false` entry.
fn claude_marketplace(root: &Path) {
    json_file(
        &root.join(".claude-plugin/marketplace.json"),
        json!({
            "name": "team-tools",
            "owner": {"name": "Team"},
            "metadata": {"description": "Our tools", "pluginRoot": "./plugins"},
            "plugins": [
                {"name": "notes", "source": "./plugins/notes", "description": "Notes helpers.", "category": "productivity"},
                {"name": "linear", "source": "linear", "version": "1.2.0"},
                {"name": "inline", "source": "./plugins/inline", "strict": false,
                 "description": "Defined by the marketplace", "skills": ["./my-skills"]},
                {"name": "npm-thing", "source": {"source": "npm", "package": "x"}}
            ]
        }),
    );
    claude_plugin(&root.join("plugins/notes"));
    codex_plugin(&root.join("plugins/linear"));
    skill(&root.join("plugins/inline/my-skills"), "inline-skill");
}

#[test]
fn local_marketplace_install_update_remove() {
    let dir = tempfile::tempdir().unwrap();
    let market_dir = dir.path().join("market");
    claude_marketplace(&market_dir);
    let home = PluginHome::new(dir.path().join("home"));

    let (record, market) = home
        .add_marketplace(market_dir.to_str().unwrap(), None)
        .unwrap();
    assert_eq!(record.name, "team-tools");
    assert_eq!(record.kind, SourceKind::Local);
    assert_eq!(market.format, "claude");
    assert_eq!(market.description.as_deref(), Some("Our tools"));
    assert_eq!(market.plugins.len(), 4);
    assert_eq!(
        market.plugins[1].source,
        PluginSource::Path {
            path: market_dir.canonicalize().unwrap().join("plugins/linear")
        }
    );
    assert!(
        home.add_marketplace(market_dir.to_str().unwrap(), None)
            .is_err()
    );

    let notes = home.install("team-tools", "notes").unwrap();
    assert_eq!(notes.id, "notes@team-tools");
    assert_eq!(notes.root, home.plugin_dir("notes", "team-tools"));
    assert!(notes.root.join("skills/note-taking/SKILL.md").is_file());
    let linear = home.install("team-tools", "linear").unwrap();
    assert_eq!(linear.format, ManifestFormat::Codex);
    let inline = home.install("team-tools", "inline").unwrap();
    assert_eq!(inline.format, ManifestFormat::Claude);
    assert_eq!(inline.skill_dirs, [inline.root.join("my-skills")]);
    let err = home.install("team-tools", "npm-thing").unwrap_err();
    assert!(err.to_string().contains("not supported"), "{err}");
    assert!(home.install("team-tools", "missing").is_err());

    let ids: Vec<String> = home.list().into_iter().map(|p| p.id).collect();
    assert_eq!(
        ids,
        ["inline@team-tools", "linear@team-tools", "notes@team-tools"]
    );
    let records = home.installed();
    assert_eq!(
        records["notes@team-tools"].version.as_deref(),
        Some("0.3.0")
    );

    // Update picks up changes of the source.
    skill(&market_dir.join("plugins/notes/skills"), "new-skill");
    let updated = home.update("notes@team-tools").unwrap();
    assert!(updated.root.join("skills/new-skill/SKILL.md").is_file());
    assert!(
        home.installed()["notes@team-tools"].updated_at >= records["notes@team-tools"].updated_at
    );

    home.remove("notes@team-tools").unwrap();
    assert!(!home.plugin_dir("notes", "team-tools").exists());
    assert!(!home.installed().contains_key("notes@team-tools"));
    assert!(home.remove("notes@team-tools").is_err());
    home.remove_marketplace("team-tools").unwrap();
    assert!(home.marketplaces().is_empty());
    // Local marketplaces are never deleted, installed plugins stay.
    assert!(market_dir.is_dir());
    assert_eq!(home.list().len(), 2);
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.canonicalize().unwrap().display())
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn init_repo(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    dir
}

/// A Codex marketplace in a git repository: a local entry and a
/// `git-subdir` entry from a second repository, then an update.
#[test]
fn git_marketplace_with_codex_catalog() {
    if !git_available() {
        eprintln!("git missing; skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mono = init_repo(dir.path(), "mono");
    claude_plugin(&mono.join("tools/notes"));
    commit_all(&mono, "notes");

    let market = init_repo(dir.path(), "market");
    json_file(
        &market.join(".agents/plugins/marketplace.json"),
        json!({
            "name": "curated",
            "interface": {"displayName": "Curated"},
            "plugins": [
                {"name": "linear", "source": {"source": "local", "path": "./plugins/linear"},
                 "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"}, "category": "Productivity"},
                {"name": "notes", "source": {"source": "git-subdir", "url": file_url(&mono), "path": "tools/notes"}}
            ]
        }),
    );
    codex_plugin(&market.join("plugins/linear"));
    commit_all(&market, "catalog");

    let home = PluginHome::new(dir.path().join("home"));
    let (record, m) = home.add_marketplace(&file_url(&market), None).unwrap();
    assert_eq!(record.kind, SourceKind::Git);
    assert_eq!(record.path, home.marketplaces_dir().join("curated"));
    assert_eq!(m.format, "codex");
    assert_eq!(m.display_name, "Curated");
    assert_eq!(
        m.plugins[0].policy.as_ref().unwrap()["authentication"],
        "ON_INSTALL"
    );

    let linear = home.install("curated", "linear").unwrap();
    assert_eq!(linear.interface["brandColor"], "#5E6AD2");
    let notes = home.install("curated", "notes").unwrap();
    assert!(notes.root.join("extra-skills/summarize/SKILL.md").is_file());
    assert!(!notes.root.join(".git").exists());
    assert!(home.installed()["notes@curated"].commit.is_some());

    // A new commit in the marketplace reaches the installed plugin on update.
    skill(&market.join("plugins/linear/custom-skills"), "fresh");
    commit_all(&market, "fresh skill");
    let updated = home.update("linear@curated").unwrap();
    assert!(updated.root.join("custom-skills/fresh/SKILL.md").is_file());
    home.remove_marketplace("curated").unwrap();
    assert!(!home.marketplaces_dir().join("curated").exists());
}
