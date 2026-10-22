//! The Skills page's detail view and "New skill → blank template": the
//! full `SKILL.md` and the files of a skill's folder, and a new folder with
//! a `SKILL.md` skeleton in the personal (`~/.agents/skills`) or the
//! project's skill folder (`<project>/.agents/skills`).
use std::path::{Path, PathBuf};

use dshx_tools::skill::is_skill_name;
use serde_json::{Map, Value, json};

/// Files a detail view lists at most.
const MAX_FILES: usize = 300;
/// Folder depth below the skill folder.
const MAX_DEPTH: usize = 4;
/// `SKILL.md` bigger than this is cut for display.
const MAX_CONTENT: usize = 512 * 1024;

/// A readable title: frontmatter `title`, else the name in words
/// ("visual-brand-foundation" → "Visual Brand Foundation").
pub(crate) fn title(frontmatter: Option<&Map<String, Value>>, name: &str) -> String {
    if let Some(t) = frontmatter
        .and_then(|f| f.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return t.to_owned();
    }
    name.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The frontmatter `title` of a skill file, if it has one.
pub(crate) fn frontmatter_title(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let (data, _) = dshx_tools::skill::split_frontmatter(&raw);
    data?
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// Whether `file` is a folder bundle's `SKILL.md` (a flat `<name>.md`
/// skill has no folder of its own).
fn bundle_dir(file: &Path) -> Option<&Path> {
    (file.file_name()? == "SKILL.md")
        .then(|| file.parent())
        .flatten()
}

/// The files of a skill: `[{path, size}]` relative to its folder, folders
/// first per level, without `.git` and `node_modules`.
fn files(dir: &Path) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    walk(dir, dir, 0, &mut out, &mut truncated);
    (out, truncated)
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Value>, truncated: &mut bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(String, PathBuf, bool, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = std::fs::metadata(e.path()).ok()?;
            Some((name, e.path(), meta.is_dir(), meta.len()))
        })
        .filter(|(name, _, is_dir, _)| {
            !(*is_dir && matches!(name.as_str(), ".git" | "node_modules")) && name != ".DS_Store"
        })
        .collect();
    entries.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    for (_, path, is_dir, size) in entries {
        if out.len() >= MAX_FILES {
            *truncated = true;
            return;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if is_dir {
            out.push(json!({"path": rel, "dir": true}));
            if depth + 1 < MAX_DEPTH {
                walk(root, &path, depth + 1, out, truncated);
            } else {
                *truncated = true;
            }
        } else {
            out.push(json!({"path": rel, "size": size}));
        }
    }
}

/// `{content, frontmatter, body, dir, files, truncated}` of a skill file.
pub(crate) fn detail(file: &Path) -> anyhow::Result<Value> {
    let mut raw = std::fs::read_to_string(file)?;
    let cut = raw.len() > MAX_CONTENT;
    if cut {
        let mut end = MAX_CONTENT;
        while !raw.is_char_boundary(end) {
            end -= 1;
        }
        raw.truncate(end);
    }
    let (frontmatter, body) = dshx_tools::skill::split_frontmatter(&raw);
    let (dir, files, truncated) = match bundle_dir(file) {
        Some(dir) => {
            let (files, truncated) = files(dir);
            (dir.to_path_buf(), files, truncated)
        }
        None => {
            let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                file.parent().map(Path::to_path_buf).unwrap_or_default(),
                vec![json!({"path": name, "size": size})],
                false,
            )
        }
    };
    Ok(json!({
        "content": raw,
        "contentTruncated": cut,
        "frontmatter": frontmatter,
        "body": body.trim_start_matches(['\n', '\r']),
        "dir": dir,
        "files": files,
        "filesTruncated": truncated,
    }))
}

/// Nearest ancestor holding `.git`, else `cwd` (where discovery looks for
/// `<project>/.agents/skills`).
pub(crate) fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// YAML for a one-line string value.
fn yaml_string(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    serde_json::to_string(&line).unwrap_or_else(|_| "\"\"".into())
}

/// The `SKILL.md` of a new skill.
pub(crate) fn skeleton(name: &str, description: &str) -> String {
    let description = if description.trim().is_empty() {
        "Describe what this skill does and when to use it."
    } else {
        description.trim()
    };
    format!(
        "---\nname: {name}\ndescription: {}\n---\n\n# {}\n\n## When to use\n\n- \n\n## Steps\n\n1. \n",
        yaml_string(description),
        title(None, name),
    )
}

/// Creates `<dir>/<name>/SKILL.md`; refuses an existing folder.
pub(crate) fn create(dir: &Path, name: &str, description: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        is_skill_name(name),
        "use lowercase letters, digits and single hyphens for the name, e.g. release-notes"
    );
    anyhow::ensure!(name.len() <= 64, "the name is longer than 64 characters");
    let folder = dir.join(name);
    anyhow::ensure!(
        folder.symlink_metadata().is_err(),
        "{} already exists",
        folder.display()
    );
    std::fs::create_dir_all(&folder)?;
    let file = folder.join("SKILL.md");
    std::fs::write(&file, skeleton(name, description))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_tools::skill::{SkillConfig, SkillRegistry};

    #[test]
    fn titles_read_like_words() {
        assert_eq!(
            title(None, "visual-brand-foundation"),
            "Visual Brand Foundation"
        );
        let fm: Map<String, Value> =
            serde_json::from_value(json!({"title": " Brand CI "})).unwrap();
        assert_eq!(title(Some(&fm), "x"), "Brand CI");
    }

    #[test]
    fn a_blank_template_is_a_valid_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        let file = create(
            &skills,
            "release-notes",
            "Write release notes: \"short\".\n",
        )
        .unwrap();
        assert_eq!(file, skills.join("release-notes/SKILL.md"));
        let registry = SkillRegistry::new(SkillConfig {
            include_default_roots: false,
            custom_dirs: vec![skills.clone()],
            ..SkillConfig::default()
        });
        let found = registry.list(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "release-notes");
        assert_eq!(found[0].description, "Write release notes: \"short\".");
        assert!(found[0].user_invocable);
        let again = create(&skills, "release-notes", "")
            .unwrap_err()
            .to_string();
        assert!(again.contains("already exists"), "{again}");
        assert!(create(&skills, "Release Notes", "").is_err());
        assert!(create(&skills, "../escape", "").is_err());
        let blank = std::fs::read_to_string(create(&skills, "blank", "").unwrap()).unwrap();
        assert!(blank.contains("# Blank"));
    }

    #[test]
    fn detail_lists_the_folder_and_splits_the_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("brand");
        std::fs::create_dir_all(skill.join("assets/fonts")).unwrap();
        std::fs::create_dir_all(skill.join("node_modules/x")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: brand\ndescription: d\ntitle: Brand\n---\n\n# Brand\n",
        )
        .unwrap();
        std::fs::write(skill.join("assets/logo.svg"), "<svg/>").unwrap();
        std::fs::write(skill.join("assets/fonts/a.woff2"), "x").unwrap();
        let d = detail(&skill.join("SKILL.md")).unwrap();
        assert_eq!(d["frontmatter"]["title"], "Brand");
        assert_eq!(d["body"], "# Brand\n");
        let paths: Vec<&str> = d["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "assets",
                "assets/fonts",
                "assets/fonts/a.woff2",
                "assets/logo.svg",
                "SKILL.md"
            ]
        );
        // A flat skill lists only its own file.
        std::fs::write(
            dir.path().join("flat.md"),
            "---\nname: flat\ndescription: d\n---\n",
        )
        .unwrap();
        let flat = detail(&dir.path().join("flat.md")).unwrap();
        assert_eq!(flat["files"], json!([{"path": "flat.md", "size": 34}]));
        assert_eq!(flat["dir"], json!(dir.path()));
    }

    #[test]
    fn project_root_is_the_git_root() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("repo/src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(dir.path().join("repo/.git")).unwrap();
        assert_eq!(project_root(&sub), dir.path().join("repo"));
        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(project_root(&loose), loose);
    }
}
