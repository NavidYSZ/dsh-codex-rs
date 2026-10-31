//! The `browser` tool of dsh-codex: a real Chrome in the background with its
//! own profile, driven over the DevTools protocol through a pipe (no
//! debugging port). Port of the plugin's `lib/browser.js` and
//! `lib/browser-tool.js`; see `docs/rust/SPEC.md` §5.5.
//!
//! The host gets the tool from [`browser_tool`]. On macOS `dshx-macos`
//! passes its [`MiniSink`] so the floating mini window shows the browser's
//! steps and a live preview; elsewhere [`NoMini`] keeps it headless and silent.
mod browser;
mod cdp;
mod diagnostics;
mod js;
mod mini;
mod scripts;
mod tool;

#[cfg(test)]
mod golden_tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use browser::{Browser, BrowserOptions};
pub use diagnostics::{BrowserCheck, BrowserDiagnostic, diagnose};
pub use mini::{MiniReply, MiniShow, MiniSink, MiniStep, NoMini};
pub use tool::{ACTIONS, BrowserTool, DESCRIPTION, TOOL_NAME, describe, input_schema};

use dshx_core::Tool;
use dshx_core::config::BrowserConfig;

/// Where Chrome-family browsers usually live (macOS first, as in browser.js).
fn candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut list: Vec<PathBuf> = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    list.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    list.extend(
        [
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/google-chrome-stable",
            "/opt/google/chrome/chrome",
            "/snap/bin/chromium",
            "/usr/bin/brave-browser",
            "/usr/bin/microsoft-edge",
        ]
        .iter()
        .map(PathBuf::from),
    );
    list
}

/// An executable file, rather than a directory or an unusable browser path.
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// Resolve an explicit executable exactly; auto-detect only when none was set.
/// A broken explicit path must not silently launch a different browser.
pub fn find_chrome(explicit: Option<&Path>) -> Option<PathBuf> {
    match explicit {
        Some(path) => is_executable(path).then(|| path.to_path_buf()),
        None => candidates().into_iter().find(|p| is_executable(p)),
    }
}

/// `~/.dsh-codex/browser-profile` (`$DSHX_HOME` respected); on macOS the
/// plugin's `~/Library/Application Support/dsh-codex/browser-profile`, so
/// existing sign-ins carry over. An explicit `$DSHX_HOME` (another instance,
/// a dev or test run) always gets its own profile next to it.
pub fn default_profile_dir() -> PathBuf {
    let own_home = std::env::var_os("DSHX_HOME").is_some_and(|h| !h.is_empty());
    if cfg!(target_os = "macos")
        && !own_home
        && let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
    {
        return PathBuf::from(home).join("Library/Application Support/dsh-codex/browser-profile");
    }
    dshx_core::config::home_dir().join("browser-profile")
}

/// `de` or `en` from the environment, else macOS's first preferred language
/// (index.js `systemLocale`).
pub fn system_locale() -> String {
    let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty());
    let posix = |v: &str| {
        let word = v
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        word == "C" || word == "POSIX"
    };
    let mut lang = env.filter(|v| !posix(v)).unwrap_or_default();
    if lang.is_empty() && cfg!(target_os = "macos") {
        lang = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .and_then(|s| {
                let start = s.find(|c: char| c.is_ascii_alphabetic())?;
                Some(
                    s[start..]
                        .chars()
                        .take_while(char::is_ascii_alphabetic)
                        .take(3)
                        .collect(),
                )
            })
            .unwrap_or_default();
    }
    if lang.to_lowercase().starts_with("de") {
        "de"
    } else {
        "en"
    }
    .to_owned()
}

/// The `browser` tool, or `None` when it is disabled or no Chrome is found.
/// Uses the system language for Chrome and the mini window; build
/// [`BrowserOptions::from_config`] + [`BrowserTool::new`] yourself to override it.
pub fn browser_tool(config: &BrowserConfig, mini: Arc<dyn MiniSink>) -> Option<Arc<dyn Tool>> {
    if !config.enabled {
        return None;
    }
    let opts = BrowserOptions::from_config(config, &system_locale());
    opts.chrome.as_ref()?;
    Some(Arc::new(BrowserTool::new(Browser::new(opts, mini))))
}
