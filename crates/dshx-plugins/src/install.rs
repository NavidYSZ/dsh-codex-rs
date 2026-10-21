//! Installing, updating and removing plugins, plus the git and copy helpers.
//!
//! An install stages the plugin in `plugins/.staging/`, checks that it
//! loads, then swaps it into `plugins/<marketplace>/<name>/`, so a failed
//! update leaves the old copy in place.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::manifest::resolve_inside;
use crate::marketplace::PluginSource;
use crate::{
    InstallRecord, Plugin, PluginHome, Unknown, is_component_name, load_plugin, now_ms, parse_id,
    plugin_id,
};

/// Upper bound of one git command (clone, fetch).
const GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs git without prompts; returns trimmed stdout.
pub(crate) fn git(args: &[&str], cwd: Option<&Path>) -> anyhow::Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("git is required for git sources: {e}"))?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > GIT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("git {} timed out", args.first().unwrap_or(&""));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Shallow clone of `url` at `git_ref` or `sha` into the new folder `dir`;
/// returns the checked-out commit.
pub(crate) fn git_clone(
    url: &str,
    git_ref: Option<&str>,
    sha: Option<&str>,
    dir: &Path,
) -> anyhow::Result<String> {
    anyhow::ensure!(!url.starts_with('-'), "invalid git URL {url:?}");
    let dir_str = dir.to_string_lossy().into_owned();
    match sha {
        Some(sha) => {
            std::fs::create_dir_all(dir)?;
            git(&["init", "--quiet"], Some(dir))?;
            git(&["remote", "add", "origin", url], Some(dir))?;
            git(&["fetch", "--depth", "1", "origin", sha], Some(dir))?;
            git(
                &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
                Some(dir),
            )?;
        }
        None => {
            let mut args = vec!["clone", "--quiet", "--depth", "1", "--no-tags"];
            if let Some(r) = git_ref {
                args.extend(["--branch", r]);
            }
            args.extend(["--", url, &dir_str]);
            git(&args, None)?;
        }
    }
    git(&["rev-parse", "HEAD"], Some(dir))
}

/// Best-effort recursive delete.
pub(crate) fn remove_dir(dir: &Path) {
    if dir.symlink_metadata().is_ok()
        && let Err(e) = std::fs::remove_dir_all(dir)
    {
        tracing::warn!("could not remove {}: {e}", dir.display());
    }
}

/// Copies a folder tree without `.git`, following symlinks to files and
/// folders (bounded depth, so a link cycle cannot run away).
pub(crate) fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    fn walk(from: &Path, to: &Path, depth: u32) -> anyhow::Result<()> {
        anyhow::ensure!(depth < 32, "{}: folders nested too deeply", from.display());
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let src = entry.path();
            let dst = to.join(&name);
            let meta = match std::fs::metadata(&src) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("skipping {}: {e}", src.display());
                    continue;
                }
            };
            if meta.is_dir() {
                walk(&src, &dst, depth + 1)?;
            } else if meta.is_file() {
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }
    walk(from, to, 0)
}

/// Fields that make a Claude marketplace entry a manifest of its own.
const COMPONENT_FIELDS: [&str; 5] = ["skills", "mcpServers", "commands", "agents", "hooks"];

impl PluginHome {
    /// Installs (or reinstalls) `plugin` from `marketplace` and returns it.
    /// Enabling it in the config is the caller's business.
    pub fn install(&self, marketplace: &str, plugin: &str) -> anyhow::Result<Plugin> {
        let market = self.marketplace(marketplace)?;
        let entry = market
            .plugins
            .iter()
            .find(|e| e.name == plugin)
            .ok_or_else(|| Unknown(plugin_id(plugin, marketplace)))?;
        anyhow::ensure!(
            is_component_name(plugin) && is_component_name(marketplace),
            "invalid plugin id {}",
            plugin_id(plugin, marketplace)
        );
        let staging_root = self.plugins_dir().join(".staging");
        std::fs::create_dir_all(&staging_root)?;
        let staging = staging_root.join(uuid::Uuid::new_v4().simple().to_string());
        let fetched = self.fetch(&entry.source, &staging);
        let commit = match fetched {
            Ok(commit) => commit,
            Err(e) => {
                remove_dir(&staging);
                return Err(e);
            }
        };
        // Claude `strict: false` entries describe the plugin themselves.
        let has_manifest = [
            "plugin.json",
            ".codex-plugin/plugin.json",
            ".claude-plugin/plugin.json",
        ]
        .iter()
        .any(|m| staging.join(m).is_file());
        if !has_manifest && COMPONENT_FIELDS.iter().any(|f| entry.raw.contains_key(*f)) {
            let mut manifest = entry.raw.clone();
            for key in ["source", "strict", "category", "tags", "policy"] {
                manifest.remove(key);
            }
            std::fs::create_dir_all(staging.join(".claude-plugin"))?;
            std::fs::write(
                staging.join(".claude-plugin/plugin.json"),
                serde_json::to_vec_pretty(&Value::Object(manifest))?,
            )?;
        }
        let data_dir = self.data_dir(plugin, marketplace);
        let staged = load_plugin(&staging, plugin, marketplace, &data_dir);
        if let Some(e) = staged.error {
            remove_dir(&staging);
            anyhow::bail!("{}: {e}", plugin_id(plugin, marketplace));
        }
        let target = self.plugin_dir(plugin, marketplace);
        std::fs::create_dir_all(target.parent().unwrap_or(&self.plugins_dir()))?;
        let old = staging_root.join(format!("old-{}", uuid::Uuid::new_v4().simple()));
        if target.exists() {
            std::fs::rename(&target, &old)?;
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            if old.exists() {
                let _ = std::fs::rename(&old, &target);
            }
            remove_dir(&staging);
            return Err(e.into());
        }
        remove_dir(&old);
        let id = plugin_id(plugin, marketplace);
        let mut records = self.installed();
        let now = now_ms();
        let installed_at = records.get(&id).map_or(now, |r| r.installed_at);
        let installed = load_plugin(&target, plugin, marketplace, &data_dir);
        records.insert(
            id,
            InstallRecord {
                marketplace: marketplace.to_owned(),
                name: plugin.to_owned(),
                version: installed.version.clone().or_else(|| entry.version.clone()),
                source: entry.source.describe(),
                commit,
                installed_at,
                updated_at: now,
            },
        );
        self.save_installed(&records)?;
        Ok(installed)
    }

    /// Copies or clones a plugin source into the new folder `dest`.
    fn fetch(&self, source: &PluginSource, dest: &Path) -> anyhow::Result<Option<String>> {
        match source {
            PluginSource::Path { path } => {
                anyhow::ensure!(path.is_dir(), "{}: plugin folder not found", path.display());
                copy_dir(path, dest)?;
                Ok(None)
            }
            PluginSource::Git {
                url,
                git_ref,
                sha,
                subdir,
            } => {
                let checkout = match subdir {
                    Some(_) => dest.with_extension("src"),
                    None => dest.to_path_buf(),
                };
                let result = git_clone(url, git_ref.as_deref(), sha.as_deref(), &checkout)
                    .and_then(|commit| {
                        if let Some(sub) = subdir {
                            let inner = resolve_inside(&checkout, sub).ok_or_else(|| {
                                anyhow::anyhow!("the subfolder {sub:?} leaves the repository")
                            })?;
                            anyhow::ensure!(inner.is_dir(), "subfolder {sub:?} not found");
                            copy_dir(&inner, dest)?;
                        } else {
                            remove_dir(&dest.join(".git"));
                        }
                        Ok(commit)
                    });
                if subdir.is_some() {
                    remove_dir(&checkout);
                }
                result.map(Some)
            }
            PluginSource::Unsupported { reason } => anyhow::bail!("cannot install: {reason}"),
        }
    }

    /// Pulls the plugin's marketplace (git) and reinstalls the plugin from it.
    pub fn update(&self, id: &str) -> anyhow::Result<Plugin> {
        let (name, market) = parse_id(id).ok_or_else(|| Unknown(id.to_owned()))?;
        anyhow::ensure!(
            self.plugin_dir(name, market).is_dir(),
            Unknown(id.to_owned())
        );
        anyhow::ensure!(
            self.marketplaces().iter().any(|m| m.name == market),
            "{id} was not installed from a registered marketplace"
        );
        self.update_marketplace(market)?;
        self.install(market, name)
    }

    /// Deletes an installed plugin and its install record (its data folder stays).
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let (name, market) = parse_id(id).ok_or_else(|| Unknown(id.to_owned()))?;
        let dir = self.plugin_dir(name, market);
        if !dir.is_dir() {
            return Err(Unknown(id.to_owned()).into());
        }
        std::fs::remove_dir_all(&dir)?;
        let market_dir = self.plugins_dir().join(market);
        if std::fs::read_dir(&market_dir).is_ok_and(|mut d| d.next().is_none()) {
            let _ = std::fs::remove_dir(&market_dir);
        }
        let mut records = self.installed();
        if records.remove(id).is_some() {
            self.save_installed(&records)?;
        }
        Ok(())
    }
}
