//! Marketplaces: catalogs of plugins in a git repository or local folder.
//!
//! A marketplace is added by git URL, GitHub `owner/repo` (both optionally
//! with `#ref`) or local path. Git marketplaces are cloned to
//! `$DSHX_HOME/marketplaces/<name>/`; local ones are read in place. The
//! registry lives in `$DSHX_HOME/marketplaces.json`. The catalog is
//! `.agents/plugins/marketplace.json` (Codex) or
//! `.claude-plugin/marketplace.json` (Claude).
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::install::{git, git_clone, remove_dir};
use crate::manifest::{read_object, resolve_inside, string, strings};
use crate::{PluginHome, Unknown, is_component_name, now_ms, write_json};

/// Catalog file locations, in lookup order.
const CATALOGS: [&str; 3] = [
    ".agents/plugins/marketplace.json",
    ".claude-plugin/marketplace.json",
    "marketplace.json",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Git,
    Github,
    Local,
}

/// A registered marketplace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceRecord {
    pub name: String,
    /// As the user gave it.
    pub source: String,
    pub kind: SourceKind,
    /// Clone URL of git marketplaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// The checkout (git) or the folder or catalog file (local).
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub added_at: u64,
    pub updated_at: u64,
}

/// Where a marketplace entry's plugin comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PluginSource {
    /// A folder inside the marketplace.
    Path { path: PathBuf },
    /// A git repository, optionally a subfolder of it (`git-subdir`).
    Git {
        url: String,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
    /// npm, pip and anything else this harness cannot fetch.
    Unsupported { reason: String },
}

impl PluginSource {
    /// Human-readable origin for the install record.
    pub fn describe(&self) -> String {
        match self {
            PluginSource::Path { path } => path.display().to_string(),
            PluginSource::Git {
                url,
                git_ref,
                sha,
                subdir,
            } => {
                let mut s = url.clone();
                if let Some(r) = sha.as_ref().or(git_ref.as_ref()) {
                    s.push('#');
                    s.push_str(r);
                }
                if let Some(d) = subdir {
                    s.push_str(" (");
                    s.push_str(d);
                    s.push(')');
                }
                s
            }
            PluginSource::Unsupported { reason } => reason.clone(),
        }
    }
}

/// One plugin offered by a marketplace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceEntry {
    pub name: String,
    pub source: PluginSource,
    pub description: Option<String>,
    pub version: Option<String>,
    pub category: Option<String>,
    pub author: Option<Value>,
    pub homepage: Option<String>,
    pub keywords: Vec<String>,
    /// Codex `interface` of the entry, with `displayName` and
    /// `shortDescription` always set.
    pub interface: Map<String, Value>,
    /// Codex install/auth policy, passed through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<Value>,
    /// The entry as written (Claude `strict: false` entries carry the manifest).
    #[serde(skip)]
    pub raw: Map<String, Value>,
}

/// A parsed marketplace catalog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Marketplace {
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub owner: Option<Value>,
    /// `codex` or `claude`.
    pub format: &'static str,
    /// Folder relative plugin sources resolve against.
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub plugins: Vec<MarketplaceEntry>,
}

/// A parsed `add` argument.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Origin {
    Git {
        url: String,
        kind: SourceKind,
        git_ref: Option<String>,
    },
    Local(PathBuf),
}

fn split_ref(s: &str) -> (&str, Option<String>) {
    match s.rsplit_once('#') {
        Some((base, r)) if !r.is_empty() => (base, Some(r.to_owned())),
        _ => (s, None),
    }
}

fn is_git_url(s: &str) -> bool {
    ["https://", "http://", "git@", "ssh://", "git://", "file://"]
        .iter()
        .any(|p| s.starts_with(p))
        || s.ends_with(".git")
}

fn github_url(repo: &str) -> String {
    format!("https://github.com/{}.git", repo.trim_end_matches(".git"))
}

/// `owner/repo`.
fn is_github_shorthand(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && !p.starts_with('.')
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

pub(crate) fn parse_origin(source: &str, git_ref: Option<&str>) -> anyhow::Result<Origin> {
    let source = source.trim();
    anyhow::ensure!(!source.is_empty(), "the marketplace source is empty");
    let explicit_ref = git_ref.map(str::to_owned).filter(|r| !r.is_empty());
    let looks_local = source.starts_with('/')
        || source.starts_with("~/")
        || source.starts_with("./")
        || source.starts_with("../");
    if !looks_local && is_git_url(source) {
        let (url, r) = split_ref(source);
        return Ok(Origin::Git {
            url: url.to_owned(),
            kind: SourceKind::Git,
            git_ref: explicit_ref.or(r),
        });
    }
    if looks_local || Path::new(source).exists() {
        let path = match source.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => PathBuf::from(source),
        };
        let path = path
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        return Ok(Origin::Local(path));
    }
    let (repo, r) = split_ref(source);
    anyhow::ensure!(
        is_github_shorthand(repo),
        "unknown marketplace source {source:?}: use a git URL, owner/repo or a local path"
    );
    Ok(Origin::Git {
        url: github_url(repo),
        kind: SourceKind::Github,
        git_ref: explicit_ref.or(r),
    })
}

/// Finds the catalog of a marketplace folder (or takes a catalog file) and
/// the root its relative sources resolve against.
fn locate(path: &Path) -> anyhow::Result<(PathBuf, PathBuf, &'static str)> {
    let (file, root) = if path.is_file() {
        let parent = path.parent().unwrap_or(Path::new("/"));
        let root = match parent.file_name().and_then(|n| n.to_str()) {
            Some(".claude-plugin") => parent.parent().unwrap_or(parent),
            Some("plugins")
                if parent
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n == ".agents") =>
            {
                parent.parent().and_then(Path::parent).unwrap_or(parent)
            }
            _ => parent,
        };
        (path.to_path_buf(), root.to_path_buf())
    } else {
        let file = CATALOGS
            .iter()
            .map(|c| path.join(c))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: no .agents/plugins/marketplace.json or .claude-plugin/marketplace.json",
                    path.display()
                )
            })?;
        (file, path.to_path_buf())
    };
    let format = if file.ends_with(".agents/plugins/marketplace.json") {
        "codex"
    } else {
        "claude"
    };
    Ok((file, root, format))
}

/// Reads the catalog of the marketplace at `path`.
pub(crate) fn read(path: &Path) -> anyhow::Result<Marketplace> {
    let (file, root, format) = locate(path)?;
    let data = read_object(&file).map_err(|e| anyhow::anyhow!(e))?;
    let name = string(&data, "name")
        .ok_or_else(|| anyhow::anyhow!("{}: the catalog has no name", file.display()))?;
    let metadata = match data.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let interface = match data.get("interface") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let plugin_root = string(&metadata, "pluginRoot");
    let mut plugins = Vec::new();
    for raw in data
        .get("plugins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(raw) = raw.as_object() else { continue };
        let Some(entry_name) = string(raw, "name") else {
            continue;
        };
        let source = parse_source(raw.get("source"), &root, plugin_root.as_deref());
        let mut iface = match raw.get("interface") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        iface
            .entry("displayName")
            .or_insert_with(|| Value::String(entry_name.clone()));
        if let Some(d) = string(raw, "description") {
            iface.entry("shortDescription").or_insert(Value::String(d));
        }
        plugins.push(MarketplaceEntry {
            source,
            description: string(raw, "description"),
            version: string(raw, "version"),
            category: string(raw, "category"),
            author: raw.get("author").cloned(),
            homepage: string(raw, "homepage"),
            keywords: strings(raw.get("keywords")),
            interface: iface,
            policy: raw.get("policy").cloned(),
            raw: raw.clone(),
            name: entry_name,
        });
    }
    Ok(Marketplace {
        display_name: string(&interface, "displayName").unwrap_or_else(|| name.clone()),
        description: string(&metadata, "description").or_else(|| string(&data, "description")),
        owner: data.get("owner").cloned(),
        name,
        format,
        root,
        manifest_path: file,
        plugins,
    })
}

fn parse_source(value: Option<&Value>, root: &Path, plugin_root: Option<&str>) -> PluginSource {
    let local = |p: &str| -> PluginSource {
        let rel = match plugin_root {
            Some(base) if !p.starts_with("./") && !p.starts_with("../") => {
                format!("{}/{p}", base.trim_end_matches('/'))
            }
            _ => p.to_owned(),
        };
        match resolve_inside(root, &rel) {
            Some(path) => PluginSource::Path { path },
            None => PluginSource::Unsupported {
                reason: format!("the source path {p:?} leaves the marketplace"),
            },
        }
    };
    let git = |url: String, o: &Map<String, Value>| PluginSource::Git {
        url,
        git_ref: string(o, "ref"),
        sha: string(o, "sha"),
        subdir: None,
    };
    match value {
        Some(Value::String(s)) if is_git_url(s) => {
            let (url, r) = split_ref(s);
            PluginSource::Git {
                url: url.to_owned(),
                git_ref: r,
                sha: None,
                subdir: None,
            }
        }
        Some(Value::String(s)) => local(s),
        Some(Value::Object(o)) => {
            let kind = string(o, "source").unwrap_or_default();
            match kind.as_str() {
                "local" | "path" | "relative" => match string(o, "path") {
                    Some(p) => local(&p),
                    None => PluginSource::Unsupported {
                        reason: "local source without a path".into(),
                    },
                },
                "github" => match string(o, "repo") {
                    Some(repo) => git(github_url(&repo), o),
                    None => PluginSource::Unsupported {
                        reason: "github source without a repo".into(),
                    },
                },
                "url" | "git" => match string(o, "url") {
                    Some(url) => git(url, o),
                    None => PluginSource::Unsupported {
                        reason: format!("{kind} source without a url"),
                    },
                },
                "git-subdir" => {
                    let url = string(o, "url")
                        .or_else(|| string(o, "repo"))
                        .map(|u| if is_git_url(&u) { u } else { github_url(&u) });
                    match (url, string(o, "path")) {
                        (Some(url), Some(path)) => PluginSource::Git {
                            url,
                            git_ref: string(o, "ref"),
                            sha: string(o, "sha"),
                            subdir: Some(path),
                        },
                        _ => PluginSource::Unsupported {
                            reason: "git-subdir source needs url and path".into(),
                        },
                    }
                }
                other => PluginSource::Unsupported {
                    reason: format!("{other:?} sources are not supported"),
                },
            }
        }
        _ => PluginSource::Unsupported {
            reason: "the entry has no source".into(),
        },
    }
}

impl PluginHome {
    fn registry_path(&self) -> PathBuf {
        self.home().join("marketplaces.json")
    }

    /// `$DSHX_HOME/marketplaces`, where git marketplaces are cloned.
    pub fn marketplaces_dir(&self) -> PathBuf {
        self.home().join("marketplaces")
    }

    /// Registered marketplaces, sorted by name.
    pub fn marketplaces(&self) -> Vec<MarketplaceRecord> {
        let mut list: Vec<MarketplaceRecord> = std::fs::read(self.registry_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    fn save_marketplaces(&self, list: &[MarketplaceRecord]) -> anyhow::Result<()> {
        write_json(&self.registry_path(), &list)
    }

    pub fn marketplace_record(&self, name: &str) -> anyhow::Result<MarketplaceRecord> {
        self.marketplaces()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| Unknown(format!("marketplace {name}")).into())
    }

    /// Reads a registered marketplace's catalog.
    pub fn marketplace(&self, name: &str) -> anyhow::Result<Marketplace> {
        read(&self.marketplace_record(name)?.path)
    }

    /// Registers a marketplace by git URL, `owner/repo` or local path
    /// (`#ref` or `git_ref` pick a branch or tag). Its name comes from the catalog.
    pub fn add_marketplace(
        &self,
        source: &str,
        git_ref: Option<&str>,
    ) -> anyhow::Result<(MarketplaceRecord, Marketplace)> {
        let origin = parse_origin(source, git_ref)?;
        let now = now_ms();
        let (record, market) = match origin {
            Origin::Local(path) => {
                let market = read(&path)?;
                let record = MarketplaceRecord {
                    name: market.name.clone(),
                    source: source.to_owned(),
                    kind: SourceKind::Local,
                    url: None,
                    git_ref: None,
                    path,
                    commit: None,
                    added_at: now,
                    updated_at: now,
                };
                (record, market)
            }
            Origin::Git { url, kind, git_ref } => {
                std::fs::create_dir_all(self.marketplaces_dir())?;
                let staging = self
                    .marketplaces_dir()
                    .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
                let cloned = git_clone(&url, git_ref.as_deref(), None, &staging)
                    .and_then(|commit| Ok((commit, read(&staging)?)));
                let (commit, market) = match cloned {
                    Ok(v) => v,
                    Err(e) => {
                        remove_dir(&staging);
                        return Err(e);
                    }
                };
                if !is_component_name(&market.name) {
                    remove_dir(&staging);
                    anyhow::bail!("invalid marketplace name {:?}", market.name);
                }
                let target = self.marketplaces_dir().join(&market.name);
                if self.marketplaces().iter().any(|m| m.name == market.name) {
                    remove_dir(&staging);
                    anyhow::bail!("a marketplace named {} is already added", market.name);
                }
                remove_dir(&target);
                std::fs::rename(&staging, &target)?;
                let record = MarketplaceRecord {
                    name: market.name.clone(),
                    source: source.to_owned(),
                    kind,
                    url: Some(url),
                    git_ref,
                    path: target.clone(),
                    commit: Some(commit),
                    added_at: now,
                    updated_at: now,
                };
                (record, read(&target)?)
            }
        };
        anyhow::ensure!(
            is_component_name(&record.name),
            "invalid marketplace name {:?}",
            record.name
        );
        let mut list = self.marketplaces();
        anyhow::ensure!(
            !list.iter().any(|m| m.name == record.name),
            "a marketplace named {} is already added",
            record.name
        );
        list.push(record.clone());
        self.save_marketplaces(&list)?;
        Ok((record, market))
    }

    /// Pulls a git marketplace (local ones are read in place anyway).
    pub fn update_marketplace(
        &self,
        name: &str,
    ) -> anyhow::Result<(MarketplaceRecord, Marketplace)> {
        let mut list = self.marketplaces();
        let record = list
            .iter_mut()
            .find(|m| m.name == name)
            .ok_or_else(|| Unknown(format!("marketplace {name}")))?;
        if record.kind != SourceKind::Local {
            let target = record.git_ref.clone().unwrap_or_else(|| "HEAD".into());
            git(
                &["fetch", "--depth", "1", "origin", &target],
                Some(&record.path),
            )?;
            git(&["reset", "--hard", "FETCH_HEAD"], Some(&record.path))?;
            record.commit = Some(git(&["rev-parse", "HEAD"], Some(&record.path))?);
        }
        record.updated_at = now_ms();
        let record = record.clone();
        let market = read(&record.path)?;
        self.save_marketplaces(&list)?;
        Ok((record, market))
    }

    /// Unregisters a marketplace and deletes its clone. Installed plugins stay.
    pub fn remove_marketplace(&self, name: &str) -> anyhow::Result<()> {
        let mut list = self.marketplaces();
        let Some(pos) = list.iter().position(|m| m.name == name) else {
            return Err(Unknown(format!("marketplace {name}")).into());
        };
        let record = list.remove(pos);
        if record.kind != SourceKind::Local && record.path.starts_with(self.marketplaces_dir()) {
            remove_dir(&record.path);
        }
        self.save_marketplaces(&list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert_eq!(
            parse_origin("openai/codex-plugins", None).unwrap(),
            Origin::Git {
                url: "https://github.com/openai/codex-plugins.git".into(),
                kind: SourceKind::Github,
                git_ref: None
            }
        );
        assert_eq!(
            parse_origin("https://example.com/m.git#v2", None).unwrap(),
            Origin::Git {
                url: "https://example.com/m.git".into(),
                kind: SourceKind::Git,
                git_ref: Some("v2".into())
            }
        );
        assert_eq!(
            parse_origin("owner/repo#main", Some("dev")).unwrap(),
            Origin::Git {
                url: "https://github.com/owner/repo.git".into(),
                kind: SourceKind::Github,
                git_ref: Some("dev".into())
            }
        );
        assert!(parse_origin("not a source", None).is_err());
        assert!(parse_origin("/definitely/missing/dir", None).is_err());
    }

    #[test]
    fn sources() {
        let root = Path::new("/m");
        assert_eq!(
            parse_source(Some(&Value::String("./plugins/a".into())), root, None),
            PluginSource::Path {
                path: "/m/plugins/a".into()
            }
        );
        assert_eq!(
            parse_source(Some(&Value::String("a".into())), root, Some("./plugins")),
            PluginSource::Path {
                path: "/m/plugins/a".into()
            }
        );
        assert!(matches!(
            parse_source(Some(&Value::String("../escape".into())), root, None),
            PluginSource::Unsupported { .. }
        ));
        let gh = serde_json::json!({"source": "github", "repo": "o/r", "ref": "v1"});
        assert_eq!(
            parse_source(Some(&gh), root, None),
            PluginSource::Git {
                url: "https://github.com/o/r.git".into(),
                git_ref: Some("v1".into()),
                sha: None,
                subdir: None
            }
        );
        let sub = serde_json::json!({"source": "git-subdir", "url": "o/mono", "path": "tools/p"});
        assert!(matches!(
            parse_source(Some(&sub), root, None),
            PluginSource::Git {
                subdir: Some(_),
                ..
            }
        ));
        let npm = serde_json::json!({"source": "npm", "package": "x"});
        assert!(matches!(
            parse_source(Some(&npm), root, None),
            PluginSource::Unsupported { .. }
        ));
    }
}
