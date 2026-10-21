//! Local skill discovery (dsh `skill-filesystem` provider plus the winner
//! selection of the `skill` registry).
//!
//! Roots, highest precedence first: `<project>/.dsh/skills`,
//! `<project>/.agents/skills`, custom roots, `~/.dsh-codex/skills`,
//! `~/.dsh/skills`, `~/.agents/skills`, an optional bundled root. A root holds
//! directory bundles (`<name>/SKILL.md`) and flat Markdown skills (`<name>.md`).
use std::cmp::Ordering;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

use serde_json::{Map, Value};

use super::frontmatter;

/// Precedence ranks of the dsh roots (lower wins a duplicate name).
const PROJECT_DSH_RANK: u32 = 100;
const PROJECT_AGENTS_RANK: u32 = 200;
const CUSTOM_RANK: u32 = 300;
const USER_DSH_RANK: u32 = 400;
const USER_AGENTS_RANK: u32 = 500;
const BUNDLED_RANK: u32 = 600;

/// The provider name dsh reports for local skills.
pub const PROVIDER: &str = "filesystem";

/// Where skills are looked up. [`SkillConfig::default`] mirrors dsh's
/// `skill-filesystem` defaults plus the dsh-codex home.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillConfig {
    /// Include the project and user roots around the custom roots.
    pub include_default_roots: bool,
    /// dsh-codex home (`$DSHX_HOME` or `~/.dsh-codex`); its `skills/` is a user root.
    pub harness_home: Option<PathBuf>,
    /// dsh home (`$DSH_HOME` or `~/.dsh`); its `skills/` is a user root.
    pub dsh_home: Option<PathBuf>,
    /// Shared agents home (`$DSH_AGENTS_HOME` or `~/.agents`).
    pub agents_home: Option<PathBuf>,
    /// Extra roots scanned after the project roots and before the user roots
    /// (plugin skill folders); [`SkillRegistry::set_custom_dirs`] replaces them.
    pub custom_dirs: Vec<PathBuf>,
    /// Bundled skills, scanned last.
    pub bundled_dir: Option<PathBuf>,
    /// Maximum description length in the model-facing catalog (dsh: 500, minimum 3).
    pub catalog_description_max_length: usize,
}

impl Default for SkillConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let env_dir = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.to_string_lossy().trim().is_empty())
                .map(PathBuf::from)
        };
        SkillConfig {
            include_default_roots: true,
            harness_home: Some(dshx_core::config::home_dir()),
            dsh_home: env_dir("DSH_HOME").or_else(|| home.as_ref().map(|h| h.join(".dsh"))),
            agents_home: env_dir("DSH_AGENTS_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".agents"))),
            custom_dirs: Vec::new(),
            bundled_dir: None,
            catalog_description_max_length: 500,
        }
    }
}

/// Resource base of a loaded skill (dsh `SkillResourceBase`; local skills
/// always use `directory`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceBase {
    Directory(PathBuf),
    Url(String),
    Opaque(String),
}

impl ResourceBase {
    pub(crate) fn json(&self) -> Value {
        match self {
            ResourceBase::Directory(path) => {
                serde_json::json!({"kind": "directory", "path": path.to_string_lossy()})
            }
            ResourceBase::Url(url) => serde_json::json!({"kind": "url", "url": url}),
            ResourceBase::Opaque(description) => {
                serde_json::json!({"kind": "opaque", "description": description})
            }
        }
    }
}

/// Invocation-neutral metadata of one winning skill (dsh `SkillSummary`).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    /// Listed in the model catalog and loadable by the `skill` tool.
    pub model_invocable: bool,
    /// Loadable through a `/name` gesture in a user message.
    pub user_invocable: bool,
    /// `project-dsh`, `project-agents`, `custom`, `user-dsh-codex`, `user-dsh`,
    /// `user-agents` or `bundled`.
    pub source: String,
    pub provider: String,
    /// Instruction file (canonical path).
    pub path: PathBuf,
    pub resource_base: ResourceBase,
    pub metadata: Option<Map<String, Value>>,
}

/// A loaded skill: summary plus the Markdown body.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDefinition {
    pub summary: SkillSummary,
    /// Body after the frontmatter, trimmed.
    pub content: String,
}

/// One catalog observation; `complete` is false when a root could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillSnapshot {
    /// Winners sorted by name.
    pub skills: Vec<SkillSummary>,
    pub complete: bool,
}

#[derive(Debug, Clone)]
struct Root {
    path: PathBuf,
    source: &'static str,
    rank: u32,
    skip_system: bool,
}

/// Discovers skills on every call; no caching, so edits apply at the next step.
/// The custom roots can change while the registry is shared.
#[derive(Debug)]
pub struct SkillRegistry {
    config: RwLock<SkillConfig>,
}

impl Clone for SkillRegistry {
    fn clone(&self) -> Self {
        SkillRegistry::new(self.config())
    }
}

impl SkillRegistry {
    pub fn new(config: SkillConfig) -> Self {
        SkillRegistry {
            config: RwLock::new(config),
        }
    }

    /// The current settings (custom roots included).
    pub fn config(&self) -> SkillConfig {
        self.config.read().unwrap().clone()
    }

    /// Replaces the custom roots; every holder of this registry sees them
    /// from its next discovery on.
    pub fn set_custom_dirs(&self, dirs: Vec<PathBuf>) {
        self.config.write().unwrap().custom_dirs = dirs;
    }

    /// Winning skills for a workspace, sorted by name (all invocation policies).
    pub fn snapshot(&self, cwd: &Path) -> SkillSnapshot {
        let (candidates, complete) = self.discover(cwd);
        let mut winners: Vec<SkillSummary> = Vec::new();
        for skill in candidates {
            if winners.iter().any(|w| w.name == skill.name) {
                tracing::warn!(
                    "skill \"{}\" from {} ignored because a higher-priority skill already exists",
                    skill.name,
                    skill.source
                );
                continue;
            }
            winners.push(skill);
        }
        winners.sort_by(|a, b| a.name.cmp(&b.name));
        SkillSnapshot {
            skills: winners,
            complete,
        }
    }

    /// Every valid skill of every root, highest precedence first; the first
    /// of each name is the one [`SkillRegistry::snapshot`] picks.
    pub fn candidates(&self, cwd: &Path) -> Vec<SkillSummary> {
        self.discover(cwd).0
    }

    fn discover(&self, cwd: &Path) -> (Vec<SkillSummary>, bool) {
        let mut complete = true;
        let mut candidates = Vec::new();
        for root in self.roots(cwd) {
            match discover_root(&root) {
                Ok(found) => candidates.extend(found.into_iter().map(|s| (root.rank, s))),
                Err(e) => {
                    tracing::warn!(
                        "skill root {} skipped: {e}; the skill catalog is incomplete",
                        root.path.display()
                    );
                    complete = false;
                }
            }
        }
        // Stable sort keeps discovery order inside one rank (dsh localOrder).
        candidates.sort_by_key(|(rank, _)| *rank);
        (candidates.into_iter().map(|(_, s)| s).collect(), complete)
    }

    /// Winning skills for a workspace, sorted by name.
    pub fn list(&self, cwd: &Path) -> Vec<SkillSummary> {
        self.snapshot(cwd).skills
    }

    /// Loads the winning skill `name` with its body; `None` when unknown,
    /// invalid or gone since discovery.
    pub fn get(&self, name: &str, cwd: &Path) -> Option<SkillDefinition> {
        if !is_skill_name(name) {
            return None;
        }
        let winner = self.list(cwd).into_iter().find(|s| s.name == name)?;
        let (summary, content) =
            parse_skill_file(&winner.path, &winner.source, winner.resource_base.clone())?;
        (summary.name == winner.name).then_some(SkillDefinition { summary, content })
    }

    fn roots(&self, cwd: &Path) -> Vec<Root> {
        let c = &self.config();
        let mut roots = Vec::new();
        let mut push = |path: PathBuf, source: &'static str, rank: u32, skip_system: bool| {
            let path = absolute(&path);
            if !roots.iter().any(|r: &Root| r.path == path) {
                roots.push(Root {
                    path,
                    source,
                    rank,
                    skip_system,
                });
            }
        };
        if c.include_default_roots {
            let project = find_project_root(&absolute(cwd));
            push(
                project.join(".dsh/skills"),
                "project-dsh",
                PROJECT_DSH_RANK,
                false,
            );
            push(
                project.join(".agents/skills"),
                "project-agents",
                PROJECT_AGENTS_RANK,
                false,
            );
        }
        for dir in &c.custom_dirs {
            push(dir.clone(), "custom", CUSTOM_RANK, false);
        }
        if c.include_default_roots {
            if let Some(home) = &c.harness_home {
                push(home.join("skills"), "user-dsh-codex", USER_DSH_RANK, true);
            }
            if let Some(home) = &c.dsh_home {
                push(home.join("skills"), "user-dsh", USER_DSH_RANK, true);
            }
            if let Some(home) = &c.agents_home {
                push(home.join("skills"), "user-agents", USER_AGENTS_RANK, false);
            }
        }
        if let Some(dir) = &c.bundled_dir {
            push(dir.clone(), "bundled", BUNDLED_RANK, false);
        }
        roots
    }
}

/// The valid skills of one folder (bundles and flat Markdown files), as a
/// custom root would yield them; a missing folder yields none.
pub fn discover_dir(dir: &Path) -> Vec<SkillSummary> {
    let root = Root {
        path: absolute(dir),
        source: "custom",
        rank: CUSTOM_RANK,
        skip_system: false,
    };
    discover_root(&root).unwrap_or_default()
}

/// Kebab-case skill names (`^[a-z0-9]+(?:-[a-z0-9]+)*$`).
pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty() && part.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'))
        })
}

/// Node `path.resolve`: absolute and lexically normalized.
fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Nearest ancestor holding `.git`, else `cwd` itself.
fn find_project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Approximates the ICU collation of JS `localeCompare` for entry names:
/// case-insensitive first, lower case before upper case on ties.
fn locale_compare(a: &str, b: &str) -> Ordering {
    let fold = |s: &str| s.to_lowercase();
    fold(a).cmp(&fold(b)).then_with(|| b.cmp(a))
}

fn discover_root(root: &Root) -> std::io::Result<Vec<SkillSummary>> {
    let entries = match std::fs::read_dir(&root.path) {
        Ok(entries) => entries,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    names.sort_by(|a, b| locale_compare(&a.0, &b.0));
    let mut skills = Vec::new();
    for (name, path) in names {
        if root.skip_system && name == ".system" {
            continue;
        }
        let (file, directory) = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => (path.join("SKILL.md"), path.clone()),
            Ok(meta) if meta.is_file() && name.ends_with(".md") => {
                (path.clone(), root.path.clone())
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(
                    "skill entry {} ignored: failed to follow symbolic link: {e}",
                    path.display()
                );
                continue;
            }
        };
        if let Some((summary, _)) =
            parse_skill_file(&file, root.source, ResourceBase::Directory(directory))
        {
            skills.push(summary);
        }
    }
    Ok(skills)
}

/// Parses one skill file; `None` (with a warning) when it is not a valid skill.
fn parse_skill_file(
    path: &Path,
    source: &str,
    resource_base: ResourceBase,
) -> Option<(SkillSummary, String)> {
    let real = std::fs::canonicalize(path).ok()?;
    if !real.is_file() {
        return None;
    }
    let bytes = match std::fs::read(&real) {
        Ok(bytes) => bytes,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return None;
        }
        Err(e) => {
            tracing::warn!("skill file {} ignored: {e}", path.display());
            return None;
        }
    };
    let Ok(raw) = String::from_utf8(bytes) else {
        tracing::warn!(
            "skill file {} ignored: not a UTF-8 text file",
            path.display()
        );
        return None;
    };
    let ignored = |why: String| {
        tracing::warn!("skill file {} ignored: {why}", path.display());
        None
    };
    let (data, body) = match frontmatter::parse(&raw) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ignored("missing YAML frontmatter".into()),
        Err(e) => return ignored(format!("invalid YAML frontmatter: {e}")),
    };
    let (Some(name), Some(description)) = (
        frontmatter::string_field(&data, "name"),
        frontmatter::string_field(&data, "description"),
    ) else {
        return ignored("frontmatter requires name and description".into());
    };
    if !is_skill_name(&name) {
        return ignored(format!("invalid skill name \"{name}\""));
    }
    let (model_invocable, user_invocable) = match frontmatter::invocation(&data) {
        Ok(policy) => policy,
        Err(e) => return ignored(format!("invalid invocation frontmatter: {e}")),
    };
    let metadata = match data.get("metadata") {
        Some(Value::Object(map)) => Some(map.clone()),
        _ => None,
    };
    let summary = SkillSummary {
        when_to_use: frontmatter::string_field(&data, "whenToUse"),
        name,
        description,
        model_invocable,
        user_invocable,
        source: source.to_owned(),
        provider: PROVIDER.to_owned(),
        path: real,
        resource_base,
        metadata,
    };
    Some((summary, js_trim(body).to_owned()))
}

/// JavaScript whitespace (`\s` and `String.prototype.trim`).
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_names() {
        for ok in ["a", "abc", "a-b", "a1-2b", "0"] {
            assert!(is_skill_name(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "a--b", "A", "a_b", "a.b", "a b"] {
            assert!(!is_skill_name(bad), "{bad}");
        }
    }

    #[test]
    fn resolves_paths_like_node() {
        assert_eq!(absolute(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn locale_order() {
        let mut v = vec!["b", "B", "a", "C"];
        v.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(v, ["a", "b", "B", "C"]);
    }
}
