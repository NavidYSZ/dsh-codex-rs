//! What the composer's `/` and `@` menus offer (`GET
//! /api/composer/suggest`): commands and skills after `/`; MCP servers,
//! plugins and the project's files and folders after `@`. Ranking follows
//! dsh-file-reference-local: exact, prefix, word prefix, substring, then
//! subsequence.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dshx_core::config::Config;
use serde_json::{Value, json};

use super::Runtime;

/// dsh-file-reference-local's excluded directories.
const EXCLUDED: [&str; 15] = [
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
];
/// Files and folders indexed per project (dsh `maxEntries`).
const MAX_ENTRIES: usize = 50_000;
/// Candidates per section (dsh `maxResults` is 20 for files).
const MAX_FILES: usize = 20;
const MAX_ITEMS: usize = 50;
/// How long an index answers before a fresh walk.
const INDEX_TTL: Duration = Duration::from_secs(20);

/// The composer commands that work in dsh-codex, with dsh's English copy
/// where dsh has the command (the UI localizes by name).
const COMMANDS: [(&str, &str, &str); 7] = [
    ("compact", "Compact", "Compact older conversation history"),
    (
        "goal",
        "Goal",
        "Set or view the goal for a long-running task",
    ),
    ("model", "Model", "Switch the model and reasoning effort"),
    (
        "permission",
        "Permission",
        "Switch the access mode of this chat",
    ),
    (
        "status",
        "Status",
        "Show context, cache and cost of this chat",
    ),
    (
        "view",
        "View",
        "Choose how much of the agent's work the chat shows",
    ),
    (
        "export",
        "Export",
        "Download this chat's session log (JSONL)",
    ),
];

/// Match quality of `query` in `text`, case-insensitive: exact, prefix,
/// word prefix (after `-`, `_`, `/`, `.` or a space), substring, then
/// subsequence; `None` = no match. An empty query matches everything.
pub fn rank(query: &str, text: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(1);
    }
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    let len_penalty = t.chars().count().saturating_sub(q.chars().count()).min(999) as u32;
    if t == q {
        return Some(10_000);
    }
    if t.starts_with(&q) {
        return Some(9_000 - len_penalty);
    }
    if let Some(pos) = t.find(&q) {
        let word = t[..pos]
            .chars()
            .last()
            .is_some_and(|c| matches!(c, '-' | '_' | '/' | '.' | ' '));
        let base = if word { 7_000 } else { 5_000 };
        return Some(base - (pos as u32).min(999));
    }
    // Subsequence: every query character in order; fewer gaps rank higher.
    let mut gaps = 0u32;
    let mut chars = t.chars();
    for qc in q.chars() {
        let mut skipped = 0;
        loop {
            match chars.next() {
                Some(c) if c == qc => break,
                Some(_) => skipped += 1,
                None => return None,
            }
        }
        gaps += skipped.min(50);
    }
    Some(3_000u32.saturating_sub(gaps * 20).max(100))
}

/// "visual-brand-foundation" → "Visual Brand Foundation".
fn title_of(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sorted(mut items: Vec<(u32, Value)>, max: usize) -> Vec<Value> {
    items.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b.1["name"].as_str().unwrap_or_default())
        })
    });
    items.into_iter().take(max).map(|(_, v)| v).collect()
}

/// One indexed path below a project folder.
#[derive(Debug, Clone)]
struct Entry {
    /// Relative, `/`-separated.
    path: String,
    dir: bool,
}

type Index = Arc<Vec<Entry>>;

fn indexes() -> &'static Mutex<HashMap<PathBuf, (Instant, Index)>> {
    static INDEXES: OnceLock<Mutex<HashMap<PathBuf, (Instant, Index)>>> = OnceLock::new();
    INDEXES.get_or_init(Mutex::default)
}

fn excluded(name: &str) -> bool {
    EXCLUDED.contains(&name)
}

/// The bounded, gitignore-aware index of `root` (cached for a while).
fn index(root: &Path) -> Index {
    if let Some((at, index)) = indexes().lock().unwrap().get(root)
        && at.elapsed() < INDEX_TTL
    {
        return index.clone();
    }
    let mut entries = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .parents(true)
        .git_ignore(true)
        .filter_entry(|e| !excluded(&e.file_name().to_string_lossy()))
        .build();
    for entry in walker.flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = rel.to_string_lossy().replace('\\', "/");
        let dir = entry.file_type().is_some_and(|t| t.is_dir());
        entries.push(Entry { path, dir });
        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }
    let index = Arc::new(entries);
    indexes()
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), (Instant::now(), index.clone()));
    index
}

/// dsh `formatFileMention`: `@path`, a directory with a trailing slash,
/// `@"…"` around paths with whitespace; `None` for unrepresentable paths.
fn mention(path: &str, dir: bool) -> Option<String> {
    let path = if dir {
        format!("{path}/")
    } else {
        path.to_owned()
    };
    if path.chars().any(|c| c.is_control() || c == '"') {
        return None;
    }
    Some(if !path.chars().any(char::is_whitespace) {
        format!("@{path}")
    } else if dir {
        format!("@\"{path}")
    } else {
        format!("@\"{path}\"")
    })
}

fn file_item(path: &str, dir: bool) -> Option<Value> {
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    };
    Some(json!({
        "type": if dir { "folder" } else { "file" },
        "name": name,
        "path": path,
        "dir": parent,
        "insert": mention(path, dir)?,
    }))
}

/// Files and folders for `@query`: a query with `/` lists that folder's
/// entries, a bare one ranks the whole index; folders lead on ties.
fn files(root: &Path, query: &str) -> Vec<Value> {
    if !root.is_dir() {
        return Vec::new();
    }
    let query = query.trim_start_matches('"');
    if let Some((dir, name)) = query.rsplit_once('/') {
        let rel = Path::new(dir);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Vec::new();
        }
        let Ok(read) = std::fs::read_dir(root.join(rel)) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for entry in read.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with('.') || excluded(&file) {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let Some(score) = rank(name, &file) else {
                continue;
            };
            let path = if dir.is_empty() {
                file.clone()
            } else {
                format!("{dir}/{file}")
            };
            if let Some(item) = file_item(&path, is_dir) {
                items.push((score + u32::from(is_dir), item));
            }
        }
        return sorted(items, MAX_FILES);
    }
    let index = index(root);
    let mut items = Vec::new();
    for entry in index.iter() {
        if query.is_empty() && entry.path.contains('/') {
            continue;
        }
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        let score = match (rank(query, name), rank(query, &entry.path)) {
            (Some(a), Some(b)) => a.max(b.saturating_sub(500)),
            (Some(a), None) => a,
            (None, Some(b)) => b.saturating_sub(500),
            (None, None) => continue,
        };
        // Shallow paths first among equals.
        let depth = entry.path.matches('/').count() as u32;
        let score = (score + u32::from(entry.dir)).saturating_sub(depth.min(20));
        if let Some(item) = file_item(&entry.path, entry.dir) {
            items.push((score, item));
        }
    }
    sorted(items, MAX_FILES)
}

/// What one suggest request knows about its chat.
pub struct SuggestContext {
    pub kind: String,
    pub query: String,
    pub cwd: Option<PathBuf>,
    /// The chat's mode (the start page: the mode it will create).
    pub preset: dshx_agent::Preset,
    /// Session id and events (when the composer belongs to a chat).
    pub session: Option<(String, Vec<dshx_core::SessionEvent>)>,
    pub model: Option<String>,
    pub goals: bool,
}

impl Runtime {
    /// The `/` or `@` menu for `ctx` (see the module docs).
    pub async fn suggest(&self, ctx: SuggestContext) -> anyhow::Result<Value> {
        let config = self.config();
        let q = ctx.query.trim().to_owned();
        let mut sections = Vec::new();
        if ctx.kind == "slash" {
            let commands = self.command_items(&ctx, &q);
            if !commands.is_empty() {
                sections.push(json!({"id": "commands", "items": commands}));
            }
            let skills = self.skill_items(&ctx, &q).await?;
            sections.push(json!({"id": "skills", "items": skills}));
        } else {
            let apps = self.app_items(&ctx, &config, &q).await;
            sections.push(json!({"id": "apps", "items": apps}));
            if let Some(cwd) = ctx.cwd.clone() {
                let query = q.clone();
                let found = tokio::task::spawn_blocking(move || files(&cwd, &query)).await?;
                sections.push(json!({"id": "files", "items": found}));
            }
        }
        // Routes without tool updates resend the whole tool list: switching
        // a server on costs one cache miss there.
        let cache_miss = ctx
            .model
            .as_deref()
            .is_some_and(|m| self.route(m).tool_update.is_none());
        Ok(json!({
            "kind": ctx.kind,
            "query": q,
            "sections": sections,
            "cacheMiss": cache_miss,
            "preset": ctx.preset.id(),
        }))
    }

    fn command_items(&self, ctx: &SuggestContext, q: &str) -> Vec<Value> {
        let standard = ctx.preset == dshx_agent::Preset::Standard;
        let chat = ctx.session.is_some();
        let mut items = Vec::new();
        for (name, label, description) in COMMANDS {
            let works = match name {
                "compact" => chat && standard,
                "goal" => chat && standard && ctx.goals,
                "status" | "export" => chat,
                _ => true,
            };
            if !works {
                continue;
            }
            let Some(score) = rank(q, name).max(rank(q, label)) else {
                continue;
            };
            items.push((
                score,
                json!({"type": "command", "name": name, "title": label, "description": description, "insert": format!("/{name}")}),
            ));
        }
        sorted(items, MAX_ITEMS)
    }

    async fn skill_items(&self, ctx: &SuggestContext, q: &str) -> anyhow::Result<Vec<Value>> {
        if ctx.preset == dshx_agent::Preset::Minimal {
            return Ok(Vec::new());
        }
        let cwd = ctx.cwd.clone().unwrap_or_else(|| self.home.clone());
        let skills = self.skills.clone();
        let snapshot = tokio::task::spawn_blocking(move || skills.snapshot(&cwd)).await?;
        let owners = self.skill_owners().await;
        let mut items = Vec::new();
        for s in snapshot.skills.into_iter().filter(|s| s.user_invocable) {
            let title = s
                .metadata
                .as_ref()
                .and_then(|m| m.get("title"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| title_of(&s.name));
            let Some(score) = rank(q, &s.name).max(rank(q, &title)) else {
                continue;
            };
            let plugin = owners
                .iter()
                .find(|(dir, _)| s.path.starts_with(dir))
                .map(|(_, id)| id.clone());
            items.push((
                score,
                json!({
                    "type": "skill",
                    "name": s.name,
                    "title": title,
                    "description": s.description,
                    "source": if plugin.is_some() { "plugin" } else { s.source.as_str() },
                    "plugin": plugin,
                    "model": s.model_invocable,
                    "insert": format!("/{} ", s.name),
                }),
            ));
        }
        Ok(sorted(items, MAX_ITEMS))
    }

    /// Skill folders of enabled plugins with the plugin id.
    async fn skill_owners(&self) -> Vec<(PathBuf, String)> {
        self.enabled_plugins()
            .await
            .into_iter()
            .flat_map(|p| {
                p.skill_dirs
                    .iter()
                    .map(|d| (d.canonicalize().unwrap_or_else(|_| d.clone()), p.id.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    async fn app_items(&self, ctx: &SuggestContext, config: &Config, q: &str) -> Vec<Value> {
        if ctx.preset == dshx_agent::Preset::Minimal {
            return Vec::new();
        }
        let active = match &ctx.session {
            Some((id, events)) => self.active_mcp(id, events),
            None => Vec::new(),
        };
        let connectors = dshx_mcp::catalog::connectors();
        let mut items = Vec::new();
        for status in self.mcp.status() {
            let name = status.name.clone();
            let availability = config.mcp_availability(&name);
            let connector = config
                .mcp_servers
                .get(&name)
                .and_then(|c| c.url.as_deref())
                .and_then(|url| connectors.iter().find(|c| c.url == url));
            let title = connector.map_or_else(|| name.clone(), |c| c.name.clone());
            let Some(score) = rank(q, &name).max(rank(q, &title)) else {
                continue;
            };
            let tools = status
                .tools
                .iter()
                .filter(|t| !availability.disabled_tools.contains(&t.raw_name))
                .count();
            items.push((
                score + 1,
                json!({
                    "type": "server",
                    "name": name,
                    "title": title,
                    "description": connector.map(|c| c.description.clone()),
                    "status": status.status,
                    "tools": tools,
                    "needsLogin": status.status == "needs-login",
                    "active": active.contains(&name) || (ctx.session.is_none() && availability.always),
                    "always": availability.always,
                    "connector": connector.map(|c| c.id.clone()),
                    "brandColor": connector.map(|c| c.brand_color.clone()),
                    "insert": format!("@{name} "),
                }),
            ));
        }
        for plugin in self.enabled_plugins().await {
            if plugin.mcp_servers.is_empty() {
                continue;
            }
            let title = plugin
                .interface
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&plugin.name)
                .to_owned();
            // A plugin whose only server carries its name is that server.
            if plugin.mcp_servers.iter().any(|s| s.name == plugin.name)
                && plugin.mcp_servers.len() == 1
            {
                continue;
            }
            let Some(score) = rank(q, &plugin.name).max(rank(q, &title)) else {
                continue;
            };
            let servers: Vec<&String> = plugin.mcp_servers.iter().map(|s| &s.name).collect();
            let skills: usize = plugin
                .skill_dirs
                .iter()
                .map(|d| dshx_tools::skill::discover_dir(d).len())
                .sum();
            items.push((
                score,
                json!({
                    "type": "plugin",
                    "name": plugin.name,
                    "id": plugin.id,
                    "title": title,
                    "description": plugin.interface.get("shortDescription").cloned().unwrap_or(Value::Null),
                    "servers": servers,
                    "skills": skills,
                    "active": servers.iter().all(|s| active.contains(s)),
                    "brandColor": plugin.interface.get("brandColor").cloned().unwrap_or(Value::Null),
                    "insert": format!("@{} ", plugin.name),
                }),
            ));
        }
        sorted(items, MAX_ITEMS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_prefers_prefix_then_substring_then_fuzzy() {
        let exact = rank("lin", "lin").unwrap();
        let prefix = rank("lin", "linear").unwrap();
        let word = rank("brand", "visual-brand-foundation").unwrap();
        let sub = rank("near", "linear").unwrap();
        let fuzzy = rank("vbf", "visual-brand-foundation").unwrap();
        assert!(exact > prefix && prefix > word && word > sub && sub > fuzzy);
        assert!(rank("xyz", "linear").is_none());
        assert_eq!(rank("", "anything"), Some(1));
    }

    #[test]
    fn mentions_quote_paths_with_spaces() {
        assert_eq!(mention("src/main.rs", false).unwrap(), "@src/main.rs");
        assert_eq!(mention("src", true).unwrap(), "@src/");
        assert_eq!(
            mention("my docs/a b.md", false).unwrap(),
            "@\"my docs/a b.md\""
        );
        assert_eq!(mention("my docs", true).unwrap(), "@\"my docs/");
        assert!(mention("a\"b", false).is_none());
    }

    #[test]
    fn files_respect_gitignore_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(root.join("secret.txt"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("src/deep/mod.rs"), "").unwrap();
        let names = |items: Vec<Value>| -> Vec<String> {
            items
                .iter()
                .map(|i| i["path"].as_str().unwrap().to_owned())
                .collect()
        };
        let found = names(files(root, "main"));
        assert_eq!(found, ["src/main.rs"]);
        assert!(names(files(root, "secret")).is_empty());
        assert!(
            names(files(root, "x"))
                .iter()
                .all(|p| !p.starts_with("node_modules"))
        );
        // A query with a slash lists that folder.
        assert_eq!(names(files(root, "src/")), ["src/deep", "src/main.rs"]);
        assert!(files(root, "../").is_empty());
        let top = files(root, "");
        assert_eq!(top[0]["insert"], "@src/");
    }
}
