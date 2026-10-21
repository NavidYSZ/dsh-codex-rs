//! End-to-end tests against a real headless Chromium and a local HTTP server.
//!
//! Chrome comes from `$DSHX_TEST_CHROME`, the Playwright build of the dev
//! server, or auto-detection; without one the tests pass vacuously. Every
//! test runs alone (one Chrome at a time), uses a temp profile and checks
//! that no Chrome process is left behind.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dshx_browser::{
    Browser, BrowserOptions, BrowserTool, MiniReply, MiniShow, MiniSink, MiniStep, NoMini,
};
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, Dimensions, ImageAttachmentRef, NewImage,
    SandboxMode, Tool, ToolContext, ToolError, ToolOutput, ToolResultPart,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const PLAYWRIGHT_CHROME: &str = "/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome";

/// One Chrome at a time: this host also runs production services.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn chrome() -> Option<PathBuf> {
    let explicit = std::env::var_os("DSHX_TEST_CHROME").map(PathBuf::from);
    let found = explicit
        .filter(|p| p.exists())
        .or_else(|| Some(PathBuf::from(PLAYWRIGHT_CHROME)).filter(|p| p.exists()))
        .or_else(|| dshx_browser::find_chrome(None));
    if found.is_none() {
        eprintln!("no Chrome found; skipping");
    }
    found
}

// ---- local test site --------------------------------------------------------

const HOME: &str = r#"<!doctype html><html><head><title>Test Home</title>
<style>body{font-family:sans-serif;margin:16px} .row{margin:6px 0}</style></head><body>
<h1>Home</h1>
<div class="row"><label for="q">Query</label> <input id="q" type="text" placeholder="Search here"></div>
<div class="row"><button id="search" onclick="document.getElementById('out').textContent='Searched: '+document.getElementById('q').value">Search</button> <span id="out"></span></div>
<div class="row"><input id="pw" type="password" aria-label="Password"></div>
<div class="row"><select id="color" aria-label="Color"><option value="r">Red</option><option value="g">Green</option><option value="b">Blue</option></select></div>
<div class="row"><label><input id="cb" type="checkbox"> Remember me</label></div>
<div class="row"><button disabled>Disabled</button></div>
<div class="row"><a href="/page2">Second page</a></div>
<div class="row"><a href="/popup" target="_blank">Popup link</a></div>
<div class="row"><button onclick="window.open('/popup')">Open window</button></div>
<div class="row"><a href="/download/DOWNLOAD_NAME" download>Download file</a></div>
<div class="row"><button onclick="alert('Hello alert')">Alert</button></div>
<div class="row"><textarea aria-label="Notes"></textarea></div>
<div class="row"><button onclick="window.open('/closer')">Open closer</button></div>
</body></html>"#;

const PAGE2: &str = r#"<!doctype html><html><head><title>Second Page</title></head><body>
<main><p>Welcome to page two.</p><a href="/">Back home</a></main></body></html>"#;

const CLOSER: &str = r#"<!doctype html><html><head><title>Closer</title></head><body>
<button onclick="window.close()">Close me</button></body></html>"#;

const POPUP: &str = r#"<!doctype html><html><head><title>Popup Page</title></head><body>
<p>I am the popup.</p></body></html>"#;

fn long_page() -> String {
    let mut body =
        String::from("<!doctype html><html><head><title>Long Page</title></head><body><main>");
    for i in 0..400 {
        body += &format!("<p>Paragraph {i}: the quick brown fox jumps over the lazy dog.</p>");
    }
    body += "</main></body></html>";
    body
}

struct Site {
    base: String,
    download: String,
}

async fn start_site() -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let download = format!(
        "dshx-test-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = download.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let name = name.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 65536 {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let path = path.split('?').next().unwrap_or("/");
                let (ctype, extra, body) = match path {
                    "/" => (
                        "text/html; charset=utf-8",
                        String::new(),
                        HOME.replace("DOWNLOAD_NAME", &name),
                    ),
                    "/page2" => ("text/html; charset=utf-8", String::new(), PAGE2.to_owned()),
                    "/closer" => ("text/html; charset=utf-8", String::new(), CLOSER.to_owned()),
                    "/popup" => ("text/html; charset=utf-8", String::new(), POPUP.to_owned()),
                    "/long" => ("text/html; charset=utf-8", String::new(), long_page()),
                    p if p.starts_with("/download/") => (
                        "application/octet-stream",
                        format!("Content-Disposition: attachment; filename=\"{name}\"\r\n"),
                        "x".repeat(4096),
                    ),
                    _ => ("text/plain", String::new(), "not found".to_owned()),
                };
                let status = if body == "not found" {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Site { base, download }
}

// ---- host fakes -------------------------------------------------------------

/// In-memory attachment store; `scale` > 1 pretends to downscale.
struct MemStore {
    scale: u32,
    saved: Mutex<Vec<ImageAttachmentRef>>,
}

impl MemStore {
    fn new(scale: u32) -> Arc<Self> {
        Arc::new(MemStore {
            scale,
            saved: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl AttachmentStore for MemStore {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        assert_eq!(image.media_type, "image/jpeg");
        assert_eq!(&image.data[..2], &[0xFF, 0xD8], "a JPEG");
        let mut saved = self.saved.lock().unwrap();
        let r = ImageAttachmentRef {
            attachment_id: format!("att-{}", saved.len() + 1),
            media_type: image.media_type,
            bytes: image.data.len() as u64,
            width: image.width / self.scale,
            height: image.height / self.scale,
            name: image.name,
            original_dimensions: (self.scale > 1).then_some(Dimensions {
                width: image.width,
                height: image.height,
            }),
        };
        saved.push(r.clone());
        Ok(r)
    }
    async fn load(&self, _id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        anyhow::bail!("not kept")
    }
}

struct AllowAll;

#[async_trait]
impl Approver for AllowAll {
    async fn approve(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> ApprovalDecision {
        ApprovalDecision::AllowedOnce
    }
}

fn ctx_with(store: Arc<MemStore>, images: bool, cancel: CancellationToken) -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        call_id: "call".into(),
        cwd: std::env::temp_dir(),
        sandbox: SandboxMode::WorkspaceWrite,
        cancel,
        attachments: store,
        model_accepts_images: images,
        approver: Arc::new(AllowAll),
        state: Arc::new(()),
    }
}

fn options(chrome: PathBuf, profile: &Path) -> BrowserOptions {
    BrowserOptions {
        chrome: Some(chrome),
        profile_dir: profile.to_path_buf(),
        width: 1280,
        height: 800,
        idle: Duration::from_secs(15 * 60),
        locale: "en".into(),
        quality: 0.7,
        // Throwaway profiles: never touch the developer's login keychain.
        mock_keychain: true,
    }
}

struct Harness {
    tool: BrowserTool,
    store: Arc<MemStore>,
    _profile: tempfile::TempDir,
}

impl Harness {
    fn new(chrome: PathBuf, mini: Arc<dyn MiniSink>) -> Harness {
        Self::with(chrome, mini, |_| {})
    }
    fn with(
        chrome: PathBuf,
        mini: Arc<dyn MiniSink>,
        tweak: impl FnOnce(&mut BrowserOptions),
    ) -> Harness {
        let profile = tempfile::tempdir().unwrap();
        let mut opts = options(chrome, &profile.path().join("profile"));
        tweak(&mut opts);
        Harness {
            tool: BrowserTool::new(Browser::new(opts, mini)),
            store: MemStore::new(1),
            _profile: profile,
        }
    }
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.tool
            .execute(
                args,
                ctx_with(self.store.clone(), true, CancellationToken::new()),
            )
            .await
    }
    async fn ok(&self, args: Value) -> ToolOutput {
        match self.call(args.clone()).await {
            Ok(out) => out,
            Err(e) => panic!("{args} failed: {e}"),
        }
    }
    async fn err(&self, args: Value) -> String {
        match self.call(args.clone()).await {
            Ok(out) => panic!("{args} should fail, got:\n{}", summary(&out)),
            Err(e) => e.to_string(),
        }
    }
    /// Closes Chrome and asserts that its whole process group is gone.
    async fn finish(self) {
        let pid = self.tool.browser().pid();
        self.tool.browser().close().await;
        if let Some(pid) = pid {
            assert_group_gone(pid).await;
        }
    }
}

fn summary(out: &ToolOutput) -> String {
    out.value.as_ref().unwrap()["summary"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `[N]` of the first outline line containing `needle`.
fn index_of(text: &str, needle: &str) -> u64 {
    let line = text
        .lines()
        .find(|l| l.starts_with('[') && l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} in\n{text}"));
    line[1..line.find(']').unwrap()].parse().unwrap()
}

/// `(number, current, title)` from the `Tabs:` line.
fn tab_list(text: &str) -> Vec<(u64, bool, String)> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("Tabs: "))
        .unwrap_or_else(|| panic!("no tabs in\n{text}"));
    line.split(" | ")
        .map(|t| {
            let (head, title) = t.split_once(' ').unwrap();
            let current = head.ends_with('*');
            (
                head.trim_end_matches('*').parse().unwrap(),
                current,
                title.to_owned(),
            )
        })
        .collect()
}

// ---- process inspection -----------------------------------------------------

fn group_members(pgid: i32) -> Vec<i32> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(rest) = stat.rfind(')').map(|i| &stat[i + 2..]) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // state ppid pgrp ...; zombies of other parents are not ours to judge
        if fields.get(2).and_then(|g| g.parse::<i32>().ok()) == Some(pgid) && fields[0] != "Z" {
            out.push(pid);
        }
    }
    out
}

async fn assert_group_gone(pgid: i32) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let left = group_members(pgid);
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < end,
            "Chrome processes left behind: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// TCP sockets in LISTEN state owned by any process of the group.
fn listening_sockets(pgid: i32) -> Vec<String> {
    let mut inodes = std::collections::HashSet::new();
    for pid in group_members(pgid) {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(link) = std::fs::read_link(fd.path()) {
                let link = link.to_string_lossy().into_owned();
                if let Some(inode) = link
                    .strip_prefix("socket:[")
                    .and_then(|s| s.strip_suffix(']'))
                {
                    inodes.insert(inode.to_owned());
                }
            }
        }
    }
    let mut found = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() > 9 && f[3] == "0A" && inodes.contains(f[9]) {
                found.push(format!("{table}: {line}"));
            }
        }
    }
    found
}

fn cmdline(pid: i32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with(name) {
            return Some(path);
        }
        if entry.file_type().is_ok_and(|t| t.is_dir())
            && let Some(hit) = find_file(&path, name)
        {
            return Some(hit);
        }
    }
    None
}

// ---- tests ------------------------------------------------------------------

#[tokio::test]
async fn actions_end_to_end() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::new(chrome, Arc::new(NoMini));
    let host = site.base.trim_start_matches("http://").to_owned();

    // open: outline, screenshot, value and meta like index.js
    let out = h
        .ok(json!({ "action": "open", "url": format!("{}/", site.base) }))
        .await;
    let text = summary(&out);
    assert!(text.starts_with("Background browser (headless, the user sees it in the mini window) · tab 1/1\nTitle: Test Home\n"), "{text}");
    assert!(text.contains(&format!("URL: {}/\n", site.base)), "{text}");
    assert!(
        text.contains(
            "Viewport 1280×800, scrolled 0 of 0 px. Screenshot pixels = page coordinates."
        ),
        "{text}"
    );
    assert!(
        text.contains("Interactive elements, [index] kind \"name\" @(x,y):"),
        "{text}"
    );
    for needle in [
        "input[text] \"Query\" @(",
        "button \"Search\" @(",
        "input[password] \"Password\" (password) @(",
        "select \"Color\" value=\"r\" @(",
        "input[checkbox] \"Remember me\" unchecked @(",
        "button \"Disabled\" disabled @(",
        "a \"Second page\" @(",
        "textarea \"Notes\" @(",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    let value = out.value.clone().unwrap();
    assert_eq!(value["place"], host.as_str());
    assert_eq!(value["screen"], json!({ "width": 1280, "height": 800 }));
    assert_eq!(value["image"]["mediaType"], "image/jpeg");
    assert_eq!(value["image"]["width"], 1280);
    assert_eq!(value["image"]["name"], "screen.jpg");
    assert!(value.get("cursor").is_none() && value.get("target").is_none());
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["summary", "place", "screen", "image"]);
    assert_eq!(out.content.len(), 2);
    assert!(matches!(&out.content[0], ToolResultPart::Text { text: t } if *t == text));
    assert!(
        matches!(&out.content[1], ToolResultPart::Image { attachment } if attachment.attachment_id == "att-1")
    );
    let meta = out.meta.clone().unwrap();
    assert_eq!(
        meta,
        json!({ "tool": "browser", "action": "open", "label": format!("Open {}/", site.base), "x": null, "y": null,
                "toX": null, "toY": null, "screen": { "width": 1280, "height": 800 }, "cursor": null,
                "place": host, "target": null })
    );
    assert!(!out.is_error);

    // no debugging port, only the pipe
    let pid = h.tool.browser().pid().expect("running");
    assert!(cmdline(pid).contains("--remote-debugging-pipe"));
    assert!(!cmdline(pid).contains("--remote-debugging-port"));
    assert!(
        group_members(pid).len() > 1,
        "Chrome runs helpers in its group"
    );
    assert_eq!(listening_sockets(pid), Vec::<String>::new());

    // type into the query field
    let q = index_of(&text, "input[text] \"Query\"");
    let out = h
        .ok(json!({ "action": "type", "index": q, "text": "hello" }))
        .await;
    let text = summary(&out);
    assert!(
        text.contains("input[text] \"Query\" value=\"hello\" focused @("),
        "{text}"
    );
    let value = out.value.clone().unwrap();
    assert_eq!(value["target"], "Search here");
    assert!(value["cursor"]["x"].is_i64() && value["cursor"]["y"].is_i64());
    assert_eq!(
        out.meta.as_ref().unwrap()["label"],
        format!("Type “hello” [{q}]")
    );

    // clear + retype, then append and delete with a key
    let out = h
        .ok(json!({ "action": "type", "index": q, "text": "world", "clear": true }))
        .await;
    assert!(summary(&out).contains("value=\"world\""));
    h.ok(json!({ "action": "type", "text": "xy" })).await;
    let out = h.ok(json!({ "action": "key", "keys": "Backspace" })).await;
    assert!(
        summary(&out).contains("value=\"worldx\""),
        "{}",
        summary(&out)
    );
    assert!(
        h.err(json!({ "action": "key", "keys": "F5" }))
            .await
            .contains("unknown key \"F5\"")
    );

    // click a button by index, then read the result
    let text = summary(&out);
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Search\"") }))
        .await;
    assert_eq!(out.value.as_ref().unwrap()["target"], "Search");
    let read = h.ok(json!({ "action": "read" })).await;
    let read_text = summary(&read);
    assert!(
        read_text.starts_with(&format!("Title: Test Home\nURL: {}/\n\n", site.base)),
        "{read_text}"
    );
    assert!(read_text.contains("Searched: worldx"), "{read_text}");
    assert_eq!(read.content.len(), 1, "read has no screenshot");
    assert_eq!(
        read.value.as_ref().unwrap().as_object().unwrap().len(),
        1,
        "only the summary"
    );

    // password fields are refused, by index and by coordinates
    let text = summary(&out);
    let pw = index_of(&text, "(password)");
    let e = h
        .err(json!({ "action": "type", "index": pw, "text": "secret" }))
        .await;
    assert_eq!(
        e,
        "Refused: that is a password field. Call \"show\" and ask the user to sign in themselves."
    );
    let line = text.lines().find(|l| l.contains("(password)")).unwrap();
    let (x, y) = line[line.rfind("@(").unwrap() + 2..line.len() - 1]
        .split_once(',')
        .unwrap();
    let e = h.err(json!({ "action": "type", "x": x.parse::<f64>().unwrap(), "y": y.parse::<f64>().unwrap(), "text": "secret" })).await;
    assert!(e.starts_with("Refused:"), "{e}");
    let e = h.err(json!({ "action": "type", "text": "secret" })).await;
    assert_eq!(
        e,
        "Refused: the focused field is a password field. Call \"show\" and ask the user to sign in themselves."
    );
    let read = summary(&h.ok(json!({ "action": "state" })).await);
    assert!(
        read.contains("input[password] \"Password\" (password) focused"),
        "{read}"
    );
    assert!(
        !h.ok(json!({ "action": "read" }))
            .await
            .value
            .unwrap()
            .to_string()
            .contains("secret")
    );

    // select and checkbox
    let out = h
        .ok(json!({ "action": "select", "index": index_of(&read, "\"Color\""), "value": "blue" }))
        .await;
    let text = summary(&out);
    assert!(text.contains("select \"Color\" value=\"b\""), "{text}");
    let e = h
        .err(
            json!({ "action": "select", "index": index_of(&text, "\"Color\""), "value": "purple" }),
        )
        .await;
    assert_eq!(e, "select: no option matches; options: Red | Green | Blue");
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Remember me\"") }))
        .await;
    assert!(summary(&out).contains("input[checkbox] \"Remember me\" checked"));

    // dialogs are answered and reported once
    let text = summary(&out);
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Alert\"") }))
        .await;
    assert!(
        summary(&out).contains("\nDialogs: alert \"Hello alert\" was accepted\n"),
        "{}",
        summary(&out)
    );
    let text = summary(&h.ok(json!({ "action": "state" })).await);
    assert!(!text.contains("Dialogs:"));

    // link navigation, history, reload
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Second page\"") }))
        .await;
    let text = summary(&out);
    assert!(
        text.contains("Title: Second Page") && text.contains(&format!("URL: {}/page2", site.base)),
        "{text}"
    );
    assert!(summary(&h.ok(json!({ "action": "back" })).await).contains("Title: Test Home"));
    assert!(summary(&h.ok(json!({ "action": "forward" })).await).contains("Title: Second Page"));
    assert!(summary(&h.ok(json!({ "action": "reload" })).await).contains("Title: Second Page"));
    assert_eq!(
        h.err(json!({ "action": "forward" })).await,
        "nothing to go forward to"
    );

    // a link with target=_blank: the new tab is adopted
    let text = summary(
        &h.ok(json!({ "action": "open", "url": format!("{}/", site.base) }))
            .await,
    );
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Popup link\"") }))
        .await;
    let text = summary(&out);
    assert!(text.starts_with("The click opened a new tab; you are now in it.\nBackground browser (headless, the user sees it in the mini window) · tab "), "{text}");
    assert!(text.contains("/2\nTitle: Popup Page"), "{text}");
    let tabs = tab_list(&text);
    assert_eq!(tabs.len(), 2);
    assert!(
        tabs.iter()
            .any(|(_, current, title)| *current && title == "Popup Page"),
        "{text}"
    );
    let home_tab = tabs.iter().find(|t| t.2 == "Test Home").unwrap().0;
    let popup_tab = tabs.iter().find(|t| t.2 == "Popup Page").unwrap().0;
    let text = summary(
        &h.ok(json!({ "action": "switch_tab", "tab": home_tab }))
            .await,
    );
    assert!(
        text.contains(&format!("tab {home_tab}/2\nTitle: Test Home")),
        "{text}"
    );
    assert!(
        tab_list(&text)
            .iter()
            .any(|(n, current, _)| *n == home_tab && *current),
        "{text}"
    );
    let text = summary(
        &h.ok(json!({ "action": "close_tab", "tab": popup_tab }))
            .await,
    );
    assert!(
        text.contains("tab 1/1\nTitle: Test Home") && !text.contains("Tabs:"),
        "{text}"
    );
    assert!(summary(&h.ok(json!({ "action": "tabs" })).await).ends_with("\nTabs: 1* Test Home"));

    // window.open from a button
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Open window\"") }))
        .await;
    let text = summary(&out);
    assert!(
        text.starts_with("The click opened a new tab; you are now in it."),
        "{text}"
    );
    assert!(text.contains("Title: Popup Page"), "{text}");

    // new tab with a long page: read is truncated, scroll moves the viewport
    let text = summary(
        &h.ok(json!({ "action": "new_tab", "url": format!("{}/long", site.base) }))
            .await,
    );
    assert!(text.contains("/3\nTitle: Long Page"), "{text}");
    let long_tab = tab_list(&text).iter().find(|t| t.1).unwrap().0;
    let read = summary(&h.ok(json!({ "action": "read" })).await);
    assert!(read.contains("Paragraph 0: the quick brown fox"));
    assert!(
        read.ends_with(" more characters; scroll and read again for more)"),
        "{}",
        &read[read.len() - 200..]
    );
    let text = summary(
        &h.ok(json!({ "action": "scroll", "direction": "down", "amount": 2 }))
            .await,
    );
    assert!(text.contains("scrolled 1280 of"), "{text}");
    let out = h.ok(json!({ "action": "scroll", "direction": "up" })).await;
    assert!(
        summary(&out).contains("scrolled 640 of"),
        "{}",
        summary(&out)
    );
    assert_eq!(
        out.value.as_ref().unwrap()["cursor"],
        json!({ "x": 640, "y": 400 })
    );
    let started = Instant::now();
    h.ok(json!({ "action": "wait", "seconds": 0.5 })).await;
    assert!(started.elapsed() >= Duration::from_millis(500));

    // closing the current tab moves to the first remaining one
    let text = summary(
        &h.ok(json!({ "action": "close_tab", "tab": long_tab }))
            .await,
    );
    let tabs = tab_list(&text);
    assert_eq!(tabs.len(), 2, "{text}");
    assert!(tabs.iter().all(|t| t.2 != "Long Page"), "{text}");
    let home_tab = tabs.iter().find(|t| t.2 == "Test Home").unwrap().0;
    let text = summary(
        &h.ok(json!({ "action": "switch_tab", "tab": home_tab }))
            .await,
    );
    assert!(text.contains("Title: Test Home"), "{text}");
    assert_eq!(
        h.err(json!({ "action": "switch_tab", "tab": 9 })).await,
        "there is no tab 9; there are 2"
    );
    assert_eq!(
        h.err(json!({ "action": "switch_tab" })).await,
        "there is no tab undefined; there are 2"
    );

    // downloads are blocked
    let text = summary(
        &h.ok(json!({ "action": "click", "index": index_of(&text, "\"Download file\"") }))
            .await,
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    for dir in [
        home.join("Downloads"),
        h._profile.path().to_path_buf(),
        std::env::current_dir().unwrap(),
        std::env::temp_dir(),
    ] {
        assert_eq!(
            find_file(&dir, &site.download),
            None,
            "download landed in {}",
            dir.display()
        );
    }
    assert!(text.contains("Title: Test Home"));

    // a popup that closes itself: another tab takes over without an error
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Open closer\"") }))
        .await;
    let text = summary(&out);
    assert!(
        text.contains("tab ") && text.contains("/3\nTitle: Closer"),
        "{text}"
    );
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Close me\"") }))
        .await;
    let text = summary(&out);
    assert!(
        !text.contains("Title: Closer") && text.contains("/2\nTitle: "),
        "{text}"
    );
    assert_eq!(tab_list(&text).len(), 2);
    h.ok(json!({ "action": "state" })).await;

    // errors that never reach Chrome
    assert_eq!(
        h.err(json!({ "action": "fly" })).await,
        "unknown action \"fly\""
    );
    assert_eq!(
        h.err(json!({ "action": "open" })).await,
        "\"open\" needs \"url\""
    );
    assert_eq!(
        h.err(json!({ "action": "click" })).await,
        "\"click\" needs \"index\" or x/y"
    );
    assert!(
        h.err(json!({ "action": "click", "index": 999 }))
            .await
            .starts_with("element [999] is gone")
    );
    assert_eq!(
        h.err(json!({ "action": "open", "url": "file:///etc/passwd" }))
            .await,
        "only http(s) pages can be opened in the background browser"
    );

    // still exactly one Chrome, still no port
    assert_eq!(h.tool.browser().pid(), Some(pid));
    assert_eq!(listening_sockets(pid), Vec::<String>::new());
    h.finish().await;
}

#[tokio::test]
async fn text_only_models_and_scaled_attachments() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let mut h = Harness::new(chrome, Arc::new(NoMini));

    // a model without image input gets the outline only
    let ctx = ctx_with(h.store.clone(), false, CancellationToken::new());
    let out = h
        .tool
        .execute(
            json!({ "action": "open", "url": format!("{}/page2", site.base) }),
            ctx,
        )
        .await
        .unwrap();
    assert_eq!(out.content.len(), 1);
    let value = out.value.unwrap();
    assert_eq!(value["screen"], json!({ "width": 1280, "height": 800 }));
    assert!(value.get("image").is_none());
    assert!(h.store.saved.lock().unwrap().is_empty());

    // a store that downscales: the model is told to keep page coordinates
    h.store = MemStore::new(2);
    let out = h.ok(json!({ "action": "state" })).await;
    let text = summary(&out);
    assert!(
        text.ends_with(" (The attached image is scaled to 640×400; keep using coordinates of the 1280×800 screenshot.)"),
        "{text}"
    );
    assert!(matches!(&out.content[0], ToolResultPart::Text { text: t } if *t == text));
    assert_eq!(
        out.value.unwrap()["image"]["originalDimensions"],
        json!({ "width": 1280, "height": 800 })
    );
    h.finish().await;
}

#[tokio::test]
async fn idle_exit_and_lazy_restart() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::with(chrome, Arc::new(NoMini), |o| {
        o.idle = Duration::from_secs(1)
    });
    assert_eq!(
        h.tool.browser().pid(),
        None,
        "lazy: nothing runs before the first action"
    );
    h.ok(json!({ "action": "open", "url": format!("{}/page2", site.base) }))
        .await;
    let first = h.tool.browser().pid().unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(h.tool.browser().pid(), None, "closed after the idle time");
    assert_group_gone(first).await;
    // the profile survives; the next action simply starts Chrome again
    let text = summary(
        &h.ok(json!({ "action": "open", "url": format!("{}/popup", site.base) }))
            .await,
    );
    assert!(text.contains("Title: Popup Page"));
    // a fresh start: Chrome's own session restore stays off
    assert!(
        text.contains(" · tab 1/1\n") && !text.contains("Tabs:"),
        "{text}"
    );
    let second = h.tool.browser().pid().unwrap();
    assert_ne!(first, second);
    h.finish().await;
}

#[tokio::test]
async fn cancellation_stops_the_call_and_frees_the_browser() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::new(chrome, Arc::new(NoMini));
    h.ok(json!({ "action": "open", "url": format!("{}/", site.base) }))
        .await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let ctx = ctx_with(h.store.clone(), true, cancel);
    let result = h
        .tool
        .execute(json!({ "action": "wait", "seconds": 10 }), ctx)
        .await;
    assert!(
        matches!(result, Err(ToolError::Cancelled)),
        "{:?}",
        result.err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // an already cancelled token never starts the action
    let ctx = ctx_with(h.store.clone(), true, {
        let c = CancellationToken::new();
        c.cancel();
        c
    });
    assert!(matches!(
        h.tool.execute(json!({ "action": "state" }), ctx).await,
        Err(ToolError::Cancelled)
    ));
    // the browser is free again and still on the same page
    let started = Instant::now();
    assert!(summary(&h.ok(json!({ "action": "state" })).await).contains("Title: Test Home"));
    assert!(started.elapsed() < Duration::from_secs(5));
    h.finish().await;
}

#[tokio::test]
async fn show_fails_gracefully_without_a_display() {
    let Some(chrome) = chrome() else { return };
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!("a display is available; skipping");
        return;
    }
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::new(chrome, Arc::new(NoMini));
    // before Chrome runs: nothing starts
    let e = h.err(json!({ "action": "show" })).await;
    assert!(
        e.starts_with("Cannot show the browser window: this machine has no display"),
        "{e}"
    );
    assert_eq!(h.tool.browser().pid(), None);
    // while it runs: it keeps running headless on the same page
    h.ok(json!({ "action": "open", "url": format!("{}/page2", site.base) }))
        .await;
    let pid = h.tool.browser().pid().unwrap();
    assert!(
        h.err(json!({ "action": "show" }))
            .await
            .contains("no display")
    );
    assert_eq!(h.tool.browser().pid(), Some(pid));
    let text = summary(&h.ok(json!({ "action": "hide" })).await);
    assert!(
        text.contains("(headless, the user sees it in the mini window)")
            && text.contains("Title: Second Page"),
        "{text}"
    );
    assert_eq!(
        h.tool.browser().pid(),
        Some(pid),
        "hide on a headless browser is a no-op"
    );
    h.finish().await;
}

#[tokio::test]
async fn dropping_the_tool_kills_chrome() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::new(chrome, Arc::new(NoMini));
    h.ok(json!({ "action": "open", "url": format!("{}/page2", site.base) }))
        .await;
    let pid = h.tool.browser().pid().unwrap();
    drop(h);
    assert_group_gone(pid).await;
}

/// Records what the browser sends to the mini window.
#[derive(Default)]
struct RecordingMini {
    shows: Mutex<Vec<MiniShow>>,
    steps: Mutex<Vec<MiniStep>>,
    frames: AtomicUsize,
    polls: AtomicUsize,
    dones: AtomicUsize,
    stop: Mutex<Option<String>>,
}

#[async_trait]
impl MiniSink for RecordingMini {
    fn enabled(&self) -> bool {
        true
    }
    async fn show(&self, show: MiniShow) -> MiniReply {
        self.shows.lock().unwrap().push(show);
        MiniReply {
            requests: vec![],
            stopped: self.stop.lock().unwrap().clone(),
        }
    }
    async fn step(&self, step: MiniStep) -> MiniReply {
        self.steps.lock().unwrap().push(step);
        MiniReply::default()
    }
    async fn frame(&self, jpeg_base64: String) -> MiniReply {
        assert!(jpeg_base64.starts_with("/9j/"), "base64 JPEG");
        self.frames.fetch_add(1, Ordering::SeqCst);
        MiniReply::default()
    }
    async fn poll(&self) -> MiniReply {
        self.polls.fetch_add(1, Ordering::SeqCst);
        MiniReply::default()
    }
    async fn done(&self) {
        self.dones.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn mini_window_gets_title_steps_frames_and_can_stop() {
    let Some(chrome) = chrome() else { return };
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let mini = Arc::new(RecordingMini::default());
    let h = Harness::with(chrome.clone(), mini.clone(), |o| o.locale = "de".into());
    let host = site.base.trim_start_matches("http://").to_owned();

    let text = summary(
        &h.ok(json!({ "action": "open", "url": format!("{}/", site.base) }))
            .await,
    );
    let out = h
        .ok(json!({ "action": "click", "index": index_of(&text, "\"Search\"") }))
        .await;
    let cursor = out.value.as_ref().unwrap()["cursor"].clone();
    {
        let shows = mini.shows.lock().unwrap();
        assert_eq!(shows.len(), 2);
        // `open` shows before navigating: still about:blank
        assert_eq!(shows[0].title, "Browser");
        assert_eq!(shows[0].activity, format!("Öffnet {host}"));
        assert_eq!(shows[0].icon, "globe");
        assert_eq!((shows[0].width, shows[0].height), (1280, 800));
        assert_eq!(shows[1].title, format!("Browser · {host}"));
        assert_eq!(shows[1].activity, "Klickt „Search“");
        let steps = mini.steps.lock().unwrap();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].click);
        assert!((steps[0].x * 1280.0 - cursor["x"].as_f64().unwrap()).abs() <= 1.0);
        assert!((steps[0].y * 800.0 - cursor["y"].as_f64().unwrap()).abs() <= 1.0);
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        mini.frames.load(Ordering::SeqCst) > 0,
        "screencast frames reach the mini window"
    );
    assert!(
        mini.polls.load(Ordering::SeqCst) > 0,
        "the mini window is polled for clicks"
    );

    // Stop in the mini window ends the action with the helper's message
    *mini.stop.lock().unwrap() = Some("The user stopped the agent.".into());
    assert_eq!(
        h.err(json!({ "action": "state" })).await,
        "The user stopped the agent."
    );
    *mini.stop.lock().unwrap() = None;
    h.ok(json!({ "action": "state" })).await;

    h.finish().await;
    assert_eq!(mini.dones.load(Ordering::SeqCst), 1, "close says done");
}

#[tokio::test]
async fn browser_tool_factory() {
    use dshx_core::config::BrowserConfig;
    let disabled = BrowserConfig {
        enabled: false,
        ..BrowserConfig::default()
    };
    assert!(dshx_browser::browser_tool(&disabled, Arc::new(NoMini)).is_none());
    let Some(chrome) = chrome() else { return };
    let config = BrowserConfig {
        chrome: Some(chrome),
        ..BrowserConfig::default()
    };
    let tool = dshx_browser::browser_tool(&config, Arc::new(NoMini)).expect("Chrome found");
    let schema = tool.schema();
    assert_eq!(schema.name, "browser");
    assert_eq!(schema.description, dshx_browser::DESCRIPTION);
    assert_eq!(schema.parameters, dshx_browser::input_schema());
    assert!(!tool.concurrency_safe());
    assert_eq!(tool.timeout(), Some(Duration::from_secs(120)));
    assert_eq!(
        tool.describe(&json!({ "action": "click", "index": 3 })),
        "Click [3]"
    );
    // lazy: building the tool starts nothing, so dropping it here is free
}

/// Needs a display (e.g. `xvfb-run -a cargo test -p dshx-browser --test chrome show_and_hide`).
#[tokio::test]
async fn show_and_hide_keep_tabs_with_a_display() {
    let Some(chrome) = chrome() else { return };
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("no display; skipping");
        return;
    }
    let _serial = SERIAL.lock().await;
    let site = start_site().await;
    let h = Harness::new(chrome, Arc::new(NoMini));
    h.ok(json!({ "action": "open", "url": format!("{}/page2", site.base) }))
        .await;
    h.ok(json!({ "action": "new_tab", "url": format!("{}/popup", site.base) }))
        .await;
    let headless = h.tool.browser().pid().unwrap();

    let text = summary(&h.ok(json!({ "action": "show" })).await);
    assert!(
        text.starts_with("Background browser (visible window) · tab "),
        "{text}"
    );
    assert!(text.contains("Title: Popup Page"), "{text}");
    let mut titles: Vec<String> = tab_list(&text).into_iter().map(|t| t.2).collect();
    titles.sort();
    assert_eq!(titles, ["Popup Page", "Second Page"]);
    let visible = h.tool.browser().pid().unwrap();
    assert_ne!(visible, headless);
    assert_group_gone(headless).await;
    assert!(!cmdline(visible).contains("--headless"));
    assert_eq!(listening_sockets(visible), Vec::<String>::new());
    // a second show is a no-op, actions work in the window
    let text = summary(&h.ok(json!({ "action": "show" })).await);
    assert!(text.contains("(visible window)"));
    assert_eq!(h.tool.browser().pid(), Some(visible));

    let text = summary(&h.ok(json!({ "action": "hide" })).await);
    assert!(
        text.starts_with("Background browser (headless, the user sees it in the mini window)"),
        "{text}"
    );
    assert!(
        text.contains("Title: Popup Page") && text.contains("Viewport 1280×800"),
        "{text}"
    );
    assert_eq!(tab_list(&text).len(), 2, "{text}");
    assert_group_gone(visible).await;
    h.finish().await;
}
