//! The user's own browser: a running Chrome-family browser publishes its
//! DevTools endpoint in `DevToolsActivePort` inside its profile directory
//! once the user allows remote debugging in `chrome://inspect/#remote-debugging`
//! (Chrome 136+ no longer accepts `--remote-debugging-port` for the default
//! profile). The browser tool attaches to that endpoint instead of starting
//! a second Chrome, so the user's tabs, cookies and sign-ins are used.
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Where to look, in order: profile directory, browser name, `.app` bundle.
/// Explicit `profiles` (tests, a dedicated automation Chrome) replace the
/// standard locations.
fn candidates(profiles: &[PathBuf]) -> Vec<(PathBuf, &'static str, Option<&'static str>)> {
    if !profiles.is_empty() {
        return profiles
            .iter()
            .map(|p| (p.clone(), "Chrome", None))
            .collect();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        let support = home.join("Library/Application Support");
        vec![
            (
                support.join("Google/Chrome"),
                "Google Chrome",
                Some("/Applications/Google Chrome.app"),
            ),
            (
                support.join("Chromium"),
                "Chromium",
                Some("/Applications/Chromium.app"),
            ),
            (
                support.join("BraveSoftware/Brave-Browser"),
                "Brave",
                Some("/Applications/Brave Browser.app"),
            ),
            (
                support.join("Microsoft Edge"),
                "Microsoft Edge",
                Some("/Applications/Microsoft Edge.app"),
            ),
        ]
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| home.join(".config"));
        vec![
            (config.join("google-chrome"), "Google Chrome", None),
            (config.join("chromium"), "Chromium", None),
            (config.join("BraveSoftware/Brave-Browser"), "Brave", None),
            (config.join("microsoft-edge"), "Microsoft Edge", None),
        ]
    }
}

/// `DevToolsActivePort`: the port on the first line, the browser WebSocket
/// path on the second.
fn active_port(profile: &Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(profile.join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next().map(str::trim).unwrap_or("").to_owned();
    (port > 0).then_some((port, path))
}

fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// `chrome://inspect`'s "Allow remote debugging for this browser instance",
/// as recorded in the profile's `Local State`; `None` when never set.
fn toggle(profile: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(profile.join("Local State")).ok()?;
    let state: Value = serde_json::from_str(&text).ok()?;
    state["devtools"]["remote_debugging"]["user-enabled"].as_bool()
}

/// Whether a browser process holds this profile (`SingletonLock` → `host-pid`).
fn profile_in_use(profile: &Path) -> bool {
    let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else {
        return false;
    };
    let pid: i32 = match target
        .to_string_lossy()
        .rsplit_once('-')
        .and_then(|(_, pid)| pid.parse().ok())
    {
        Some(pid) => pid,
        None => return false,
    };
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// A running browser the tool can attach to.
#[derive(Debug, Clone, PartialEq)]
pub struct UserChrome {
    pub name: &'static str,
    pub profile: PathBuf,
    pub port: u16,
    /// `ws://127.0.0.1:<port>/devtools/browser/<id>`
    pub ws_url: String,
    /// The `.app` bundle on macOS, for the mini window's icon.
    pub app: Option<String>,
}

/// What Settings → Browser Use shows about the user's own browser.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserChromeStatus {
    /// The tool can attach right now.
    pub available: bool,
    pub name: Option<&'static str>,
    pub profile: Option<PathBuf>,
    pub port: Option<u16>,
    /// `ready`, `port-closed` (toggle on, browser needs a restart),
    /// `toggle-off` (allow it in chrome://inspect) or `no-browser`.
    pub state: &'static str,
}

/// Cheap, synchronous: file reads and one short TCP connect per candidate.
pub fn user_chrome_status() -> UserChromeStatus {
    user_chrome_status_in(&[])
}

pub fn user_chrome_status_in(profiles: &[PathBuf]) -> UserChromeStatus {
    let mut best: Option<UserChromeStatus> = None;
    for (profile, name, _) in candidates(profiles) {
        let running = profile_in_use(&profile);
        let toggle = toggle(&profile);
        let port = active_port(&profile);
        let status = match port {
            Some((port, _)) if port_open(port) => UserChromeStatus {
                available: true,
                name: Some(name),
                profile: Some(profile.clone()),
                port: Some(port),
                state: "ready",
            },
            _ if !running => continue,
            Some((port, _)) if toggle == Some(true) => UserChromeStatus {
                available: false,
                name: Some(name),
                profile: Some(profile.clone()),
                port: Some(port),
                state: "port-closed",
            },
            _ => UserChromeStatus {
                available: false,
                name: Some(name),
                profile: Some(profile.clone()),
                port: None,
                state: if toggle == Some(true) {
                    "port-closed"
                } else {
                    "toggle-off"
                },
            },
        };
        if status.available {
            return status;
        }
        best.get_or_insert(status);
    }
    best.unwrap_or(UserChromeStatus {
        available: false,
        name: None,
        profile: None,
        port: None,
        state: "no-browser",
    })
}

/// The instructions the model relays when no browser can be attached.
fn help(status: &UserChromeStatus) -> String {
    match status.state {
        "port-closed" => format!(
            "{} is running and remote debugging is allowed, but its DevTools port {} is closed. Ask the user to quit and reopen {} (the setting takes effect at the next start), then try again.",
            status.name.unwrap_or("The browser"),
            status.port.unwrap_or(0),
            status.name.unwrap_or("the browser")
        ),
        "toggle-off" => format!(
            "{} is running, but it does not allow remote debugging. Ask the user to open chrome://inspect/#remote-debugging in {}, tick \"Allow remote debugging for this browser instance\", click Allow if Chrome asks, and then try again. Until then only the background browser is available.",
            status.name.unwrap_or("The browser"),
            status.name.unwrap_or("the browser")
        ),
        _ => "no running Google Chrome, Chromium, Brave or Edge was found. Ask the user to open their browser (with remote debugging allowed in chrome://inspect/#remote-debugging) or use the background browser.".to_owned(),
    }
}

/// `GET http://127.0.0.1:<port>/json/version` → `webSocketDebuggerUrl`.
/// Chrome 147+ answers 404 for the default profile; the path from
/// `DevToolsActivePort` still works then. 403 means the per-session prompt
/// was declined.
async fn version_ws_url(port: u16) -> Result<Option<String>> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream =
        tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| anyhow!("timeout"))??;
    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| anyhow!("timeout"))??;
    let text = String::from_utf8_lossy(&raw);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    match status {
        200 => {
            let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
            let json: Value = serde_json::from_str(body.trim())?;
            Ok(json["webSocketDebuggerUrl"].as_str().map(str::to_owned))
        }
        403 => bail!(
            "the browser refused the DevTools connection (HTTP 403): Chrome's \"Allow remote debugging?\" prompt was not accepted for this browser session"
        ),
        _ => Ok(None),
    }
}

/// Finds the running browser and the WebSocket URL to attach to. Errors
/// carry the instructions for the user.
pub async fn resolve_in(profiles: &[PathBuf]) -> Result<UserChrome> {
    let status = user_chrome_status_in(profiles);
    if !status.available {
        bail!("{}", help(&status));
    }
    let (profile, name, app) = candidates(profiles)
        .into_iter()
        .find(|(p, _, _)| Some(p) == status.profile.as_ref())
        .ok_or_else(|| anyhow!("{}", help(&status)))?;
    let port = status.port.unwrap_or(0);
    let (_, path) = active_port(&profile).ok_or_else(|| anyhow!("{}", help(&status)))?;
    let from_file = (!path.is_empty()).then(|| format!("ws://127.0.0.1:{port}{path}"));
    let ws_url = match version_ws_url(port).await {
        Ok(Some(url)) => url,
        Ok(None) => from_file.ok_or_else(|| {
            anyhow!(
                "{name} publishes no DevTools WebSocket path; ask the user to quit and reopen it"
            )
        })?,
        Err(e) => {
            if e.to_string().contains("403") {
                return Err(e);
            }
            from_file.ok_or(e)?
        }
    };
    Ok(UserChrome {
        name,
        profile,
        port,
        ws_url,
        app: app.filter(|a| Path::new(a).is_dir()).map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_port_parses_port_and_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(active_port(dir.path()), None);
        std::fs::write(
            dir.path().join("DevToolsActivePort"),
            "9222\n/devtools/browser/abc\n",
        )
        .unwrap();
        assert_eq!(
            active_port(dir.path()),
            Some((9222, "/devtools/browser/abc".to_owned()))
        );
        std::fs::write(dir.path().join("DevToolsActivePort"), "0\n").unwrap();
        assert_eq!(active_port(dir.path()), None);
    }

    #[test]
    fn toggle_reads_local_state() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(toggle(dir.path()), None);
        std::fs::write(
            dir.path().join("Local State"),
            r#"{"devtools":{"remote_debugging":{"user-enabled":true}}}"#,
        )
        .unwrap();
        assert_eq!(toggle(dir.path()), Some(true));
    }

    #[test]
    fn help_texts_name_the_fix() {
        let off = UserChromeStatus {
            available: false,
            name: Some("Google Chrome"),
            profile: None,
            port: None,
            state: "toggle-off",
        };
        assert!(help(&off).contains("chrome://inspect/#remote-debugging"));
        let closed = UserChromeStatus {
            port: Some(9222),
            state: "port-closed",
            ..off.clone()
        };
        assert!(help(&closed).contains("quit and reopen Google Chrome"));
        let none = UserChromeStatus {
            state: "no-browser",
            ..off
        };
        assert!(help(&none).contains("no running"));
    }
}
