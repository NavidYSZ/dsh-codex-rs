//! Where the pinned Pi runtime and its Node come from.
//!
//! Pi: `DSHX_PI` → beside the program (`pi/`, the app's `../Resources/pi/`,
//! a release tarball's `../share/dsh-codex/pi/`) → `$DSHX_HOME/runtime/pi-<v>/`.
//! A `pi` on `PATH` is never used. Node: the same resolution as PTC
//! (`DSHX_NODE` → beside the program → `PATH`). The runtime counts only when
//! `cli.js --version` prints exactly [`PI_VERSION`].
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The one Pi release Aether talks to (RPC protocol and event shapes).
pub const PI_VERSION: &str = "1.5.0";

/// A verified runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub node: PathBuf,
    /// Folder with `package.json` and `dist/bundle/cli.js`.
    pub root: PathBuf,
    pub version: String,
}

impl Install {
    pub fn cli(&self) -> PathBuf {
        self.root.join("dist").join("bundle").join("cli.js")
    }
}

fn is_runtime(dir: &Path) -> bool {
    dir.join("dist").join("bundle").join("cli.js").is_file() && dir.join("package.json").is_file()
}

/// Candidate runtime folders in resolution order.
pub fn candidates(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("DSHX_PI").filter(|v| !v.is_empty()) {
        out.push(PathBuf::from(dir));
        return out;
    }
    if let Some(bin) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        out.push(bin.join("pi"));
        out.push(bin.join("..").join("Resources").join("pi"));
        out.push(bin.join("..").join("share").join("dsh-codex").join("pi"));
    }
    out.push(home.join("runtime").join(format!("pi-{PI_VERSION}")));
    out
}

/// Finds Node and the runtime and checks the version (spawns Node once,
/// about 0.3 s). `home` is `$DSHX_HOME`.
pub fn resolve(home: &Path) -> Result<Install, String> {
    let explicit = std::env::var_os("DSHX_PI").is_some_and(|v| !v.is_empty());
    let root = candidates(home)
        .into_iter()
        .find(|d| is_runtime(d))
        .ok_or_else(|| {
            if explicit {
                "DSHX_PI does not name a Pi runtime folder (dist/bundle/cli.js and package.json)"
                    .to_owned()
            } else {
                format!(
                    "the pinned Pi {PI_VERSION} runtime is not installed (no app bundle runtime, \
                     no {}); set DSHX_PI or install it with desktop/fetch-pi.py",
                    home.join("runtime")
                        .join(format!("pi-{PI_VERSION}"))
                        .display()
                )
            }
        })?;
    let root = dshx_tools::sandbox::canonical_path(&root);
    let node = dshx_tools::ptc::node_binary()
        .map_err(|e| format!("Pi needs Node.js 22.19 or newer: {e}"))?;
    let version = probe_version(&node, &root.join("dist").join("bundle").join("cli.js"))?;
    if version != PI_VERSION {
        return Err(format!(
            "Pi {version} found in {}, but Aether is pinned to {PI_VERSION}",
            root.display()
        ));
    }
    Ok(Install {
        node,
        root,
        version,
    })
}

fn probe_version(node: &Path, cli: &Path) -> Result<String, String> {
    let mut child = std::process::Command::new(node)
        .arg(cli)
        .arg("--version")
        .env_clear()
        .envs(
            ["PATH", "HOME", "TMPDIR", "LANG"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
        )
        .env("PI_OFFLINE", "1")
        .env("PI_TELEMETRY", "0")
        .env("PI_SKIP_VERSION_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", node.display()))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("pi --version did not answer within 15 s".into());
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("pi --version: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(format!(
            "pi --version failed ({}){}",
            out.status,
            if err.is_empty() {
                String::new()
            } else {
                format!(": {}", err.chars().take(300).collect::<String>())
            }
        ));
    }
    Ok(text.lines().last().unwrap_or_default().trim().to_owned())
}
