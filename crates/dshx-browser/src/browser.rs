//! Background browser for the `browser` tool (port of `lib/browser.js`): its
//! own Chrome profile driven over the DevTools protocol through a pipe (no
//! debugging port), headless unless the user asks to see it. With
//! [`BrowserOptions::user_chrome`] the same handle attaches to the user's own
//! running Chrome instead (see `user.rs`): their tabs and sign-ins, no second
//! window, nothing is killed on close, and the approved connection is kept
//! while the app runs. Screencast frames and the agent cursor go to the mini
//! window when there is one.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Child;
use tokio::task::AbortHandle;

use crate::cdp::{self, Cdp, DEFAULT_TIMEOUT, Event};
use crate::js;
use crate::mini::{MiniReply, MiniShow, MiniSink, MiniStep};
use crate::scripts;
use crate::user::UserChrome;

/// How long one call waits for the user's browser to accept the DevTools
/// connection (Chrome 144+ first asks "Allow remote debugging?").
const USER_CONNECT_WAIT: Duration = Duration::from_secs(60);
/// How long an unanswered connection attempt stays open for the next call.
const USER_CONNECT_KEEP: Duration = Duration::from_secs(5 * 60);
/// How long the first attach looks for the tab the user is looking at.
const VISIBLE_TAB_BUDGET: Duration = Duration::from_secs(3);

/// The first probe answer that found a visible document.
fn first_visible(answers: &[Option<bool>]) -> Option<usize> {
    answers.iter().position(|a| *a == Some(true))
}

/// How the background browser runs.
#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Chrome/Chromium binary; `None` = none found (the tool is not offered).
    pub chrome: Option<PathBuf>,
    /// Persistent profile, separate from the user's own Chrome.
    pub profile_dir: PathBuf,
    /// Viewport in CSS pixels (= screenshot pixels).
    pub width: u32,
    pub height: u32,
    /// The background Chrome quits after this long without a browser action.
    /// The user's own Chrome stays attached: reconnecting would make it ask
    /// for permission again.
    pub idle: Duration,
    /// `de` or `en`: Chrome's UI language and the mini window's status line.
    pub locale: String,
    /// Screenshot JPEG quality, 0…1.
    pub quality: f32,
    /// macOS: `--use-mock-keychain`, so Chrome never asks for the login
    /// keychain (tests, dev runs, sandboxed agents).
    pub mock_keychain: bool,
    /// Attach to the user's own running Chrome (its DevTools endpoint from
    /// `DevToolsActivePort`) instead of starting a background Chrome.
    pub user_chrome: bool,
    /// Profile directories to look for the user's browser in; empty = the
    /// standard locations (tests point this at a Chrome of their own).
    pub user_profiles: Vec<PathBuf>,
}

impl BrowserOptions {
    /// Options from the `[codex.browser]` config, with auto-detected Chrome.
    pub fn from_config(config: &dshx_core::config::BrowserConfig, locale: &str) -> Self {
        BrowserOptions {
            chrome: crate::find_chrome(config.chrome.as_deref()),
            profile_dir: crate::default_profile_dir(),
            width: config.width,
            height: config.height,
            idle: Duration::from_secs(u64::from(config.idle_minutes) * 60),
            locale: locale.to_owned(),
            quality: config.quality,
            mock_keychain: config.mock_keychain
                || std::env::var("DSHX_BROWSER_MOCK_KEYCHAIN")
                    .is_ok_and(|v| !v.is_empty() && v != "0"),
            user_chrome: false,
            user_profiles: Vec::new(),
        }
    }

    /// The same settings for the user's own browser.
    pub fn for_user_chrome(&self) -> Self {
        BrowserOptions {
            user_chrome: true,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    pub target_id: String,
    pub session_id: String,
}

/// State the event reader touches as well as the actions.
#[derive(Default)]
struct PageState {
    /// Bumped on every launch/close so late events of an old Chrome are ignored.
    generation: u64,
    page: Option<Page>,
    popups: Vec<String>,
    dialogs: Vec<String>,
    loading: Option<Instant>,
    casting: bool,
    pid: Option<i32>,
    last_use: Option<Instant>,
    /// Last known viewport in CSS pixels (the user's window has its own size).
    viewport: Option<(f64, f64)>,
    /// Attached mode: the browser's name and `.app` bundle.
    user: Option<(&'static str, Option<String>)>,
    /// Tabs this tool opened; the user's own tabs are never navigated away.
    owned: HashSet<String>,
    /// Page targets in the order this connection first saw them. Chrome
    /// lists targets most recently used first, so its order changes with
    /// every focus change; tab numbers follow this order instead and stay
    /// stable (new tabs go last).
    seen: Vec<String>,
    /// Attached mode: the browser whose "Allow remote debugging?" prompt a
    /// call is waiting for right now (holding the ops lock).
    allow_wait: Option<&'static str>,
}

#[derive(Default)]
struct Shared {
    st: Mutex<PageState>,
    frame_busy: AtomicBool,
    idle_timer: Mutex<Option<AbortHandle>>,
    poll_timer: Mutex<Option<AbortHandle>>,
}

impl Shared {
    fn st(&self) -> MutexGuard<'_, PageState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn stop_timers(&self) {
        for slot in [&self.idle_timer, &self.poll_timer] {
            if let Some(h) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                h.abort();
            }
        }
    }
}

struct Proc {
    /// Our own Chrome; `None` when attached to the user's browser.
    child: Option<Child>,
    pid: i32,
    cdp: Arc<Cdp>,
}

impl Drop for Proc {
    // Last line of defence (cancelled close, dropped browser): close the pipe
    // so Chrome quits by itself, and kill the whole group if it does not.
    // An attached browser only loses the socket.
    fn drop(&mut self) {
        self.cdp.shutdown();
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let running = matches!(child.try_wait(), Ok(None));
        let pid = self.pid;
        if running || cdp::group_alive(pid) {
            std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(2);
                while Instant::now() < end {
                    if !cdp::group_alive(pid) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                cdp::kill_group(pid);
            });
        }
    }
}

/// A DevTools WebSocket handshake with the user's browser. Chrome asks
/// "Allow remote debugging?" for every new connection, so at most one runs:
/// a call whose wait ran out leaves it here and the next call adopts it.
struct Connecting {
    found: UserChrome,
    task: tokio::task::JoinHandle<Result<cdp::Ws, String>>,
}

impl Drop for Connecting {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct Ops {
    proc: Option<Proc>,
    visible: bool,
    /// Attached mode: a connection attempt still waiting for the user.
    connecting: Option<Connecting>,
}

pub(crate) struct Inner {
    opts: BrowserOptions,
    mini: Arc<dyn MiniSink>,
    /// Held for a whole action or lifecycle change: one thing at a time.
    ops: tokio::sync::Mutex<Ops>,
    shared: Arc<Shared>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.stop_timers();
        self.ops.get_mut().proc.take();
    }
}

/// Handle to the background browser; cheap to clone. Chrome starts lazily on
/// the first action and quits after `idle` without one (the user's own
/// Chrome is attached on the first action and stays attached).
#[derive(Clone)]
pub struct Browser {
    inner: Arc<Inner>,
}

impl Browser {
    pub fn new(opts: BrowserOptions, mini: Arc<dyn MiniSink>) -> Browser {
        Browser {
            inner: Arc::new(Inner {
                opts,
                mini,
                ops: tokio::sync::Mutex::new(Ops::default()),
                shared: Arc::new(Shared::default()),
            }),
        }
    }

    /// Whether a Chrome binary was found (attached mode needs none).
    pub fn available(&self) -> bool {
        self.inner.opts.user_chrome || self.inner.opts.chrome.is_some()
    }

    /// Attached to the user's own browser right now.
    pub fn attached(&self) -> bool {
        self.inner.shared.st().user.is_some()
    }

    /// Stops the screencast until the next action (another browser of the
    /// same tool takes over the mini window). Never waits: this browser may
    /// be busy with another chat's action, or with a user-Chrome connection
    /// waiting up to a minute for "Allow". Its frames stop reaching the mini
    /// window at once (`casting` is off); Chrome is told to stop streaming
    /// when the ops lock is free, else its next action takes over again.
    pub async fn pause_cast(&self) {
        if !self.inner.mini.enabled() {
            return;
        }
        let session = {
            let mut st = self.inner.shared.st();
            if !std::mem::replace(&mut st.casting, false) {
                return;
            }
            st.page.as_ref().map(|p| p.session_id.clone())
        };
        let Some(session) = session else {
            return;
        };
        if let Ok(ops) = self.inner.ops.try_lock()
            && let Some(p) = ops.proc.as_ref()
        {
            p.cdp.post("Page.stopScreencast", json!({}), Some(session));
        }
    }

    /// While a call waits for the user to allow the connection to their
    /// browser, a second call gets this answer at once instead of waiting.
    pub fn waiting_for_allow(&self) -> Option<String> {
        let name = self.inner.shared.st().allow_wait?;
        // Only while that call still holds the lock (it may just have ended).
        self.inner
            .ops
            .try_lock()
            .is_err()
            .then(|| allow_prompt(name))
    }

    pub fn options(&self) -> &BrowserOptions {
        &self.inner.opts
    }

    /// Process id (= process group) of the running Chrome, if any.
    pub fn pid(&self) -> Option<i32> {
        self.inner.shared.st().pid
    }

    /// Quits Chrome politely (cookies are flushed). Call on host shutdown;
    /// dropping the last handle closes the pipe and kills it after 2 s.
    pub async fn close(&self) {
        let running = {
            let mut ctl = self.lock().await;
            let running = ctl.ops.proc.is_some();
            ctl.close().await;
            running
        };
        if running && self.inner.mini.enabled() {
            self.inner.mini.done().await;
        }
    }

    /// Shows the browser as a normal window (or hides it again), keeping tabs.
    pub async fn set_visible(&self, on: bool) -> Result<()> {
        self.lock().await.set_visible(on).await
    }

    pub(crate) async fn lock(&self) -> Ctl<'_> {
        Ctl::lock(&self.inner).await
    }
}

/// Whether the document of `target` is visible: attach, ask, detach.
async fn tab_visible(cdp: &Cdp, target: &str) -> bool {
    let Ok(res) = cdp
        .send(
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
            None,
            DEFAULT_TIMEOUT,
        )
        .await
    else {
        return false;
    };
    let Some(session) = res["sessionId"].as_str().map(str::to_owned) else {
        return false;
    };
    let state = cdp
        .send(
            "Runtime.evaluate",
            json!({ "expression": "document.visibilityState", "returnByValue": true }),
            Some(&session),
            Duration::from_secs(3),
        )
        .await
        .ok()
        .and_then(|r| r["result"]["value"].as_str().map(str::to_owned));
    let _ = cdp
        .send(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            None,
            DEFAULT_TIMEOUT,
        )
        .await;
    state.as_deref() == Some("visible")
}

/// What a call answers while the user's browser asks "Allow remote debugging?".
fn allow_prompt(name: &str) -> String {
    format!(
        "{name} is showing \"Allow remote debugging?\" and waits for the user. Ask the user to click Allow in {name}, then call the browser tool once more: that call takes over this pending connection. Do not retry in a loop, and never connect to the browser's DevTools port yourself (curl, WebSocket/CDP scripts, Puppeteer, Playwright, project helpers): every new connection makes {name} ask again."
    )
}

/// Marks a call waiting for "Allow" (see [`Browser::waiting_for_allow`])
/// until dropped, also when the call itself is dropped.
struct AllowWait(Arc<Shared>);

impl AllowWait {
    fn start(shared: &Arc<Shared>, name: &'static str) -> AllowWait {
        shared.st().allow_wait = Some(name);
        AllowWait(shared.clone())
    }
}

impl Drop for AllowWait {
    fn drop(&mut self) {
        self.0.st().allow_wait = None;
    }
}

/// Answers "show" requests from the mini window (the user clicked the preview).
fn handle_requests(browser: &Weak<Inner>, reply: &MiniReply) {
    if reply.requests.iter().any(|r| r == "show") {
        let weak = browser.clone();
        tokio::spawn(async move {
            if let Some(inner) = weak.upgrade() {
                let _ = Browser { inner }.set_visible(true).await;
            }
        });
    }
}

/// Reacts to DevTools events of one Chrome instance.
struct Events {
    generation: u64,
    shared: Arc<Shared>,
    mini: Arc<dyn MiniSink>,
    browser: Weak<Inner>,
}

impl Events {
    fn on_event(&self, cdp: &Arc<Cdp>, ev: Event) {
        let mut st = self.shared.st();
        if st.generation != self.generation {
            return;
        }
        let page = st.page.clone();
        let on_page = page
            .as_ref()
            .is_some_and(|p| ev.session_id.as_deref() == Some(p.session_id.as_str()));
        match ev.method.as_str() {
            "Target.targetCreated" => {
                let info = &ev.params["targetInfo"];
                let opener = info["openerId"].as_str().unwrap_or("");
                let id = info["targetId"].as_str().unwrap_or("");
                if let Some(page) = &page
                    && info["type"] == "page"
                    && !opener.is_empty()
                    && opener == page.target_id
                    && id != page.target_id
                {
                    st.popups.push(id.to_owned());
                }
            }
            "Page.javascriptDialogOpening" if on_page => {
                let kind = js::display(ev.params.get("type"));
                let accept = kind == "alert" || kind == "beforeunload";
                let message = js::display(ev.params.get("message"));
                st.dialogs.push(format!(
                    "{kind} \"{}\" was {}",
                    js::slice(&message, 200),
                    if accept { "accepted" } else { "dismissed" }
                ));
                cdp.post(
                    "Page.handleJavaScriptDialog",
                    json!({ "accept": accept }),
                    ev.session_id,
                );
            }
            "Target.targetDestroyed" | "Target.detachedFromTarget" => {
                let gone = page.as_ref().is_some_and(|p| {
                    ev.params["targetId"].as_str() == Some(p.target_id.as_str())
                        || ev.params["sessionId"].as_str() == Some(p.session_id.as_str())
                });
                if gone {
                    st.page = None;
                    st.casting = false;
                }
            }
            "Page.frameStartedLoading" if on_page => {
                st.loading.get_or_insert_with(Instant::now);
            }
            "Page.loadEventFired" | "Page.frameStoppedLoading" if on_page => {
                st.loading = None;
            }
            "Page.screencastFrame" if on_page => {
                // Paused (the other browser has the mini window): ack only.
                let casting = st.casting;
                drop(st);
                let ack = json!({ "sessionId": ev.params["sessionId"] });
                cdp.post("Page.screencastFrameAck", ack, ev.session_id);
                if casting && let Some(data) = ev.params["data"].as_str() {
                    self.forward_frame(data.to_owned());
                }
            }
            _ => {}
        }
    }

    /// Drops frames while the helper is still busy with the previous one.
    fn forward_frame(&self, data: String) {
        if self.shared.frame_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (mini, shared, browser) =
            (self.mini.clone(), self.shared.clone(), self.browser.clone());
        tokio::spawn(async move {
            let reply = mini.frame(data).await;
            handle_requests(&browser, &reply);
            shared.frame_busy.store(false, Ordering::SeqCst);
        });
    }

    fn on_close(&self) {
        let mut st = self.shared.st();
        if st.generation == self.generation {
            st.page = None;
            st.casting = false;
        }
    }
}

/// A point on the page, from an element index or x/y.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
    pub password: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Snap {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "scrollY", default)]
    pub scroll_y: f64,
    #[serde(rename = "scrollHeight", default)]
    pub scroll_height: f64,
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub height: f64,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub focused: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Item {
    pub i: f64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub password: Option<bool>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Tab {
    #[serde(rename = "targetId", default)]
    pub target_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
}

/// A screenshot in page coordinates.
#[derive(Debug, Clone)]
pub(crate) struct Shot {
    /// Base64 JPEG.
    pub data: String,
    pub width: f64,
    pub height: f64,
}

pub(crate) struct PageView {
    pub snap: Snap,
    pub image: Shot,
    pub tabs: Vec<Tab>,
}

/// Exclusive access to the browser for one action or lifecycle change.
pub(crate) struct Ctl<'a> {
    inner: &'a Arc<Inner>,
    ops: tokio::sync::MutexGuard<'a, Ops>,
}

impl<'a> Ctl<'a> {
    async fn lock(inner: &'a Arc<Inner>) -> Ctl<'a> {
        Ctl {
            inner,
            ops: inner.ops.lock().await,
        }
    }

    fn st(&self) -> MutexGuard<'_, PageState> {
        self.inner.shared.st()
    }

    pub fn current_target(&self) -> Option<String> {
        self.st().page.as_ref().map(|p| p.target_id.clone())
    }

    pub fn touch(&self) {
        self.st().last_use = Some(Instant::now());
    }

    pub fn mark_loading(&self) {
        self.st().loading = Some(Instant::now());
    }

    fn loading(&self) -> bool {
        self.st().loading.is_some()
    }

    fn cdp(&self) -> Result<Arc<Cdp>> {
        self.ops
            .proc
            .as_ref()
            .map(|p| p.cdp.clone())
            .ok_or_else(|| anyhow!("browser closed"))
    }

    /// Browser-level command (targets, windows).
    pub async fn browser_send(&self, method: &str, params: Value) -> Result<Value> {
        let cdp = self.cdp()?;
        cdp.send(method, params, None, DEFAULT_TIMEOUT)
            .await
            .map_err(anyhow::Error::msg)
    }

    /// Command to the current page.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let cdp = self.cdp()?;
        let session = self
            .st()
            .page
            .as_ref()
            .map(|p| p.session_id.clone())
            .ok_or_else(|| anyhow!("browser closed"))?;
        cdp.send(method, params, Some(&session), DEFAULT_TIMEOUT)
            .await
            .map_err(anyhow::Error::msg)
    }

    // ---- lifecycle ---------------------------------------------------------

    fn alive(&mut self) -> bool {
        match self.ops.proc.as_mut() {
            Some(p) => {
                !p.cdp.is_closed()
                    && p.child
                        .as_mut()
                        .is_none_or(|c| matches!(c.try_wait(), Ok(None)))
            }
            None => false,
        }
    }

    /// Attached to the user's own browser (rather than running our own).
    pub fn attached(&self) -> bool {
        self.inner.opts.user_chrome
    }

    pub async fn ensure(&mut self) -> Result<()> {
        self.touch();
        self.arm_idle();
        if self.alive() {
            if self.st().page.is_some() {
                if self.inner.mini.enabled() && !self.st().casting {
                    let _ = self.start_cast().await;
                }
                return Ok(());
            }
            // The tab went away (closed by the page or the user): take another.
            match self.reattach().await {
                Ok(()) => return Ok(()),
                // Keep the user's approved connection: a new one would make
                // Chrome ask again. Reconnect only once Chrome closed it.
                Err(e) if self.attached() => return Err(e),
                Err(_) => {}
            }
        }
        let visible = self.ops.visible;
        self.launch(visible, Vec::new()).await
    }

    /// Attaches to the first tab (in the user's browser: the tab they are
    /// looking at), or a new blank one when there is none.
    pub async fn reattach(&mut self) -> Result<()> {
        let tabs = self.tabs().await?;
        let mut chosen = None;
        if self.attached() {
            chosen = self.visible_tab(&tabs).await;
        }
        let target_id = match chosen.or_else(|| tabs.into_iter().next().map(|t| t.target_id)) {
            Some(id) => id,
            None => self.create_target("about:blank", false).await?,
        };
        self.attach(&target_id).await
    }

    /// The tab the user sees: the first page (in list order) whose document
    /// is visible. All tabs are asked at once and the answer waits at most
    /// [`VISIBLE_TAB_BUDGET`]: frozen or discarded tabs can take the whole
    /// attach timeout. `None` lets the caller take the first page.
    async fn visible_tab(&self, tabs: &[Tab]) -> Option<String> {
        let cdp = self.cdp().ok()?;
        let tabs: Vec<String> = tabs.iter().take(40).map(|t| t.target_id.clone()).collect();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for (index, target) in tabs.iter().enumerate() {
            // Each probe runs to its end on its own, detaching what it
            // attached, also after this call stopped waiting.
            let (cdp, target, tx) = (cdp.clone(), target.clone(), tx.clone());
            tokio::spawn(async move {
                let _ = tx.send((index, tab_visible(&cdp, &target).await));
            });
        }
        drop(tx);
        let mut answers: Vec<Option<bool>> = vec![None; tabs.len()];
        let deadline = tokio::time::sleep(VISIBLE_TAB_BUDGET);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                answer = rx.recv() => {
                    let Some((index, visible)) = answer else { break };
                    answers[index] = Some(visible);
                    // Decided once every tab before the first visible one answered.
                    if let Some(i) = first_visible(&answers)
                        && answers[..i].iter().all(Option::is_some)
                    {
                        return Some(tabs[i].clone());
                    }
                }
            }
        }
        first_visible(&answers).map(|i| tabs[i].clone())
    }

    /// The current tab is one the user opened (attached mode): "open" takes
    /// a new tab instead of navigating it away.
    pub fn on_users_tab(&self) -> bool {
        let st = self.st();
        st.user.is_some()
            && st
                .page
                .as_ref()
                .is_some_and(|p| !st.owned.contains(&p.target_id))
    }

    /// Attaches to the user's running browser over its DevTools WebSocket.
    async fn attach_user(&mut self, urls: Vec<String>) -> Result<()> {
        self.discard().await;
        let (found, ws) = self.connect_user().await?;
        let generation = {
            let mut st = self.st();
            st.generation += 1;
            st.page = None;
            st.popups.clear();
            st.dialogs.clear();
            st.loading = None;
            st.casting = false;
            st.pid = None;
            st.viewport = None;
            st.owned.clear();
            st.seen.clear();
            st.user = Some((found.name, found.app.clone()));
            st.generation
        };
        let events = Arc::new(Events {
            generation,
            shared: self.inner.shared.clone(),
            mini: self.inner.mini.clone(),
            browser: Arc::downgrade(self.inner),
        });
        let closer = events.clone();
        let cdp = Cdp::start_ws(
            ws,
            move |cdp, ev| events.on_event(cdp, ev),
            move || closer.on_close(),
        );
        self.ops.proc = Some(Proc {
            child: None,
            pid: 0,
            cdp,
        });
        self.ops.visible = true;
        tracing::debug!(
            browser = found.name,
            port = found.port,
            "attached to the user's browser"
        );
        let result = self.boot(urls).await;
        // An open connection stays even when taking a tab failed: the next
        // call tries again on it instead of making Chrome ask once more.
        if result.is_err() && !self.alive() {
            self.discard().await;
        }
        result
    }

    /// The DevTools WebSocket of the user's browser: the attempt an earlier
    /// call left pending, else a new one. An attempt the user has not
    /// answered within [`USER_CONNECT_WAIT`] stays open (up to
    /// [`USER_CONNECT_KEEP`]) for the next call instead of being replaced by
    /// a second "Allow remote debugging?" prompt.
    async fn connect_user(&mut self) -> Result<(UserChrome, cdp::Ws)> {
        let mut adopted = self.ops.connecting.is_some();
        loop {
            if self.ops.connecting.is_none() {
                let found = crate::user::resolve_in(&self.inner.opts.user_profiles).await?;
                let url = found.ws_url.clone();
                let task =
                    tokio::spawn(async move { cdp::connect_ws(&url, USER_CONNECT_KEEP).await });
                self.ops.connecting = Some(Connecting { found, task });
            }
            let Some(attempt) = self.ops.connecting.as_mut() else {
                bail!("no connection attempt");
            };
            let name = attempt.found.name;
            let waiting = AllowWait::start(&self.inner.shared, name);
            let joined = tokio::time::timeout(USER_CONNECT_WAIT, &mut attempt.task).await;
            drop(waiting);
            let Ok(joined) = joined else {
                bail!("{}", allow_prompt(name));
            };
            let found = attempt.found.clone();
            self.ops.connecting = None;
            let error = match joined {
                Ok(Ok(ws)) => return Ok((found, ws)),
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            // An attempt left over from an earlier call that failed meanwhile
            // (browser restarted, nobody answered) is replaced once; a
            // declined prompt is reported.
            if adopted && !error.contains("HTTP 403") {
                adopted = false;
                continue;
            }
            bail!("could not connect to {name}: {error}");
        }
    }

    /// Whether the current tab still exists.
    pub fn has_page(&self) -> bool {
        self.st().page.is_some()
    }

    async fn launch(&mut self, visible: bool, urls: Vec<String>) -> Result<()> {
        if self.attached() {
            return self.attach_user(urls).await;
        }
        let Some(chrome) = self.inner.opts.chrome.clone() else {
            bail!("No Chrome, Chromium, Brave or Edge found for the background browser");
        };
        self.discard().await;
        create_profile_dir(&self.inner.opts.profile_dir)?;
        forget_session(&self.inner.opts.profile_dir);
        let args = launch_args(&self.inner.opts, visible);
        let (child, writer, reader) = cdp::spawn_chrome(&chrome, &args)
            .map_err(|e| anyhow!("could not start {}: {e}", chrome.display()))?;
        let pid = child.id().map_or(0, |p| p as i32);
        let generation = {
            let mut st = self.st();
            st.generation += 1;
            st.page = None;
            st.popups.clear();
            st.dialogs.clear();
            st.loading = None;
            st.casting = false;
            st.pid = Some(pid);
            st.viewport = None;
            st.owned.clear();
            st.seen.clear();
            st.user = None;
            st.generation
        };
        let events = Arc::new(Events {
            generation,
            shared: self.inner.shared.clone(),
            mini: self.inner.mini.clone(),
            browser: Arc::downgrade(self.inner),
        });
        let closer = events.clone();
        let started = Cdp::start(
            writer,
            reader,
            move |cdp, ev| events.on_event(cdp, ev),
            move || closer.on_close(),
        );
        let cdp = match started {
            Ok(cdp) => cdp,
            Err(e) => {
                let mut child = child;
                cdp::kill_group(pid);
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                self.forget_page();
                bail!("could not talk to the browser: {e}");
            }
        };
        // From here on `Proc` owns the process and cleans up after failures.
        self.ops.proc = Some(Proc {
            child: Some(child),
            pid,
            cdp,
        });
        self.ops.visible = visible;
        tracing::debug!(pid, visible, "browser started");
        let result = self.boot(urls).await;
        if result.is_err() {
            self.discard().await;
        }
        result
    }

    async fn boot(&mut self, urls: Vec<String>) -> Result<()> {
        let cdp = self.cdp()?;
        cdp.send(
            "Browser.getVersion",
            json!({}),
            None,
            Duration::from_secs(20),
        )
        .await
        .map_err(|e| anyhow!("the browser did not start ({e})"))?;
        self.browser_send("Target.setDiscoverTargets", json!({ "discover": true }))
            .await?;
        if !self.attached() {
            // The user's browser keeps its own download settings.
            let _ = self
                .browser_send("Browser.setDownloadBehavior", json!({ "behavior": "deny" }))
                .await;
        }
        self.reattach().await?;
        for (i, url) in urls.iter().enumerate() {
            if i == 0 {
                self.navigate(url).await?;
            } else {
                self.create_target(url, true).await?;
            }
        }
        Ok(())
    }

    pub async fn create_target(&self, url: &str, background: bool) -> Result<String> {
        let params = if background {
            json!({ "url": url, "background": true })
        } else {
            json!({ "url": url })
        };
        let res = self.browser_send("Target.createTarget", params).await?;
        let id = res["targetId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Target.createTarget returned no target"))?;
        self.st().owned.insert(id.clone());
        Ok(id)
    }

    /// Kills a Chrome that is gone wrong (crashed, half started) without
    /// asking; an attached browser is only let go.
    async fn discard(&mut self) {
        let Some(mut p) = self.ops.proc.take() else {
            return;
        };
        self.forget_page();
        p.cdp.shutdown();
        let Some(child) = p.child.as_mut() else {
            return;
        };
        if matches!(child.try_wait(), Ok(None)) {
            cdp::kill_group(p.pid);
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        if cdp::group_alive(p.pid) {
            cdp::kill_group(p.pid);
        }
    }

    fn forget_page(&self) {
        let mut st = self.st();
        st.generation += 1;
        st.page = None;
        st.casting = false;
        st.loading = None;
        st.pid = None;
        st.user = None;
        st.owned.clear();
        st.seen.clear();
    }

    /// Quits Chrome: `Browser.close`, then SIGKILL to the group after 2 s.
    /// The user's own browser is only detached from; it keeps running.
    pub async fn close(&mut self) {
        self.inner.shared.stop_timers();
        self.ops.connecting = None;
        if self.attached() {
            self.stop_cast().await;
            let session = self.st().page.as_ref().map(|p| p.session_id.clone());
            if let (Some(p), Some(session)) = (self.ops.proc.as_ref(), session) {
                let _ = p
                    .cdp
                    .send(
                        "Target.detachFromTarget",
                        json!({ "sessionId": session }),
                        None,
                        Duration::from_secs(3),
                    )
                    .await;
            }
        }
        let Some(mut p) = self.ops.proc.take() else {
            return;
        };
        self.forget_page();
        let Some(child) = p.child.as_mut() else {
            p.cdp.shutdown();
            tracing::debug!("detached from the user's browser");
            return;
        };
        let _ = p
            .cdp
            .send("Browser.close", json!({}), None, Duration::from_secs(3))
            .await;
        p.cdp.shutdown();
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            cdp::kill_group(p.pid);
            let _ = child.wait().await;
        }
        if cdp::group_alive(p.pid) {
            cdp::kill_group(p.pid);
        }
        tracing::debug!(pid = p.pid, "browser closed");
    }

    fn arm_idle(&self) {
        let mut slot = self
            .inner
            .shared
            .idle_timer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(h) = slot.take() {
            h.abort();
        }
        // A visible window is in use by the user; the user's own Chrome
        // stays attached, as a new connection would make it ask again.
        if self.ops.visible || self.attached() {
            return;
        }
        let weak = Arc::downgrade(self.inner);
        let idle = self.inner.opts.idle;
        *slot = Some(tokio::spawn(idle_watch(weak, idle)).abort_handle());
    }

    /// Shows the browser window to the user (or hides it again), keeping tabs.
    /// The user's own browser is always visible: "show" brings the tab to
    /// the front, "hide" changes nothing.
    pub async fn set_visible(&mut self, on: bool) -> Result<()> {
        if self.attached() {
            if on
                && self.alive()
                && let Some(id) = self.current_target()
            {
                let _ = self
                    .browser_send("Target.activateTarget", json!({ "targetId": id }))
                    .await;
            }
            return Ok(());
        }
        if self.alive() && self.ops.visible == on {
            return Ok(());
        }
        if on {
            check_display()?;
        }
        let mut urls = Vec::new();
        if self.alive() {
            let infos: Vec<Value> = match self.browser_send("Target.getTargets", json!({})).await {
                Ok(v) => v["targetInfos"].as_array().cloned().unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            let current_id = self.current_target();
            let current = infos
                .iter()
                .find(|t| current_id.is_some() && t["targetId"].as_str() == current_id.as_deref());
            if let Some(url) = current.and_then(|t| t["url"].as_str())
                && !url.is_empty()
                && url != "about:blank"
            {
                urls.push(url.to_owned());
            }
            for t in &infos {
                let url = t["url"].as_str().unwrap_or("");
                let is_current = current.is_some_and(|c| c["targetId"] == t["targetId"]);
                if t["type"] == "page"
                    && !is_current
                    && (url.starts_with("http:") || url.starts_with("https:"))
                {
                    urls.push(url.to_owned());
                }
            }
            self.close().await;
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        match self.launch(on, urls.clone()).await {
            Ok(()) => Ok(()),
            Err(e) if on => {
                // Back to the background with the same tabs rather than no browser.
                let _ = self.launch(false, urls).await;
                Err(anyhow!("could not show the browser window: {e}"))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn attach(&mut self, target_id: &str) -> Result<()> {
        let res = self
            .browser_send(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        let session_id = res["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("could not attach to the tab"))?
            .to_owned();
        self.st().page = Some(Page {
            target_id: target_id.to_owned(),
            session_id,
        });
        self.send("Page.enable", json!({})).await?;
        let _ = self
            .send("Page.setLifecycleEventsEnabled", json!({ "enabled": true }))
            .await;
        // A background tab renders no frames, so its screenshot would hang
        // (browser.js has that bug after switch_tab); make it the active tab.
        let _ = self
            .browser_send("Target.activateTarget", json!({ "targetId": target_id }))
            .await;
        if !self.ops.visible {
            let (width, height) = (self.inner.opts.width, self.inner.opts.height);
            self.send(
                "Emulation.setDeviceMetricsOverride",
                json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false }),
            )
            .await?;
        }
        self.st().casting = false;
        if self.inner.mini.enabled() {
            let _ = self.start_cast().await;
        }
        Ok(())
    }

    // ---- mini window -------------------------------------------------------

    /// Ends the screencast (the other browser of the tool gets the mini window).
    async fn stop_cast(&mut self) {
        let casting = {
            let mut st = self.st();
            std::mem::replace(&mut st.casting, false) && st.page.is_some()
        };
        if casting {
            let _ = self.send("Page.stopScreencast", json!({})).await;
        }
    }

    /// Viewport in CSS pixels: the last measured page size, else the options.
    pub fn viewport(&self) -> (f64, f64) {
        let opts = &self.inner.opts;
        self.st()
            .viewport
            .unwrap_or((f64::from(opts.width), f64::from(opts.height)))
    }

    async fn start_cast(&mut self) -> Result<()> {
        {
            let mut st = self.st();
            if st.casting || st.page.is_none() {
                return Ok(());
            }
            st.casting = true;
        }
        let params = json!({ "format": "jpeg", "quality": 55, "maxWidth": 720, "maxHeight": 460, "everyNthFrame": 2 });
        self.send("Page.startScreencast", params).await?;
        Ok(())
    }

    /// Shows the mini window with the current page and step. Fails only when
    /// the user stopped the agent there.
    pub async fn mini(&mut self, activity: &str, point: Option<Point>, click: bool) -> Result<()> {
        let sink = self.inner.mini.clone();
        if !sink.enabled() {
            return Ok(());
        }
        let host = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": "location.host", "returnByValue": true }),
            )
            .await
            .ok()
            .and_then(|r| r["result"]["value"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let title = if host.is_empty() {
            "Browser".to_owned()
        } else {
            format!("Browser · {host}")
        };
        let opts = &self.inner.opts;
        let user_app = self.st().user.as_ref().and_then(|(_, app)| app.clone());
        let icon = user_app
            .or_else(|| opts.chrome.as_deref().and_then(app_bundle))
            .unwrap_or_else(|| "globe".to_owned());
        let (width, height) = self.viewport();
        let show = sink
            .show(MiniShow {
                title,
                icon,
                width: width.round().max(1.0) as u32,
                height: height.round().max(1.0) as u32,
                activity: activity.to_owned(),
            })
            .await;
        handle_requests(&Arc::downgrade(self.inner), &show);
        if let Some(message) = show.stopped {
            return Err(anyhow::Error::msg(message));
        }
        if let Some(p) = point {
            let step = MiniStep {
                x: p.x / width,
                y: p.y / height,
                click,
            };
            if let Some(message) = sink.step(step).await.stopped {
                return Err(anyhow::Error::msg(message));
            }
        }
        self.restart_poll();
        Ok(())
    }

    /// Picks up "show" clicks on the mini window while the agent is thinking.
    fn restart_poll(&self) {
        let mut slot = self
            .inner
            .shared
            .poll_timer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(h) = slot.take() {
            h.abort();
        }
        let weak = Arc::downgrade(self.inner);
        let sink = self.inner.mini.clone();
        let until = Instant::now() + Duration::from_secs(40);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(900)).await;
                if Instant::now() > until {
                    return;
                }
                let running = weak.upgrade().is_some_and(|inner| {
                    let st = inner.shared.st();
                    st.pid.is_some() || st.user.is_some()
                });
                if !running {
                    return;
                }
                let reply = sink.poll().await;
                handle_requests(&weak, &reply);
            }
        });
        *slot = Some(task.abort_handle());
    }

    // ---- page helpers ------------------------------------------------------

    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let res = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = res.get("exceptionDetails").filter(|e| !e.is_null()) {
            let description = ex["exception"]["description"]
                .as_str()
                .filter(|s| !s.is_empty());
            bail!(
                "{}",
                description.map_or_else(|| js::display(ex.get("text")), str::to_owned)
            );
        }
        Ok(res["result"].get("value").cloned().unwrap_or(Value::Null))
    }

    /// The `eval` action: runs the model's JavaScript in the current tab
    /// (promises awaited) and returns the value as text, at most `max`
    /// UTF-16 units. A thrown exception is the result, not a failure.
    pub async fn eval_text(&self, expression: &str, max: usize) -> Result<String> {
        let res = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            )
            .await
            .map_err(|e| {
                let text = e.to_string();
                if text.contains("by value") || text.contains("reference chain") {
                    anyhow!(
                        "{text}; return plain data instead (pick the fields you need or JSON.stringify them)"
                    )
                } else {
                    e
                }
            })?;
        Ok(cap_text(eval_result(&res), max))
    }

    pub async fn settle(&self, max: Duration) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let start = Instant::now();
        while self.loading() && start.elapsed() < max {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        if !self.loading() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn navigate(&mut self, raw: &str) -> Result<()> {
        let url = normalize_url(raw)?;
        self.mark_loading();
        let res = self.send("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = res["errorText"].as_str().filter(|s| !s.is_empty()) {
            self.st().loading = None;
            bail!("could not open {url}: {err}");
        }
        self.settle(Duration::from_secs(15)).await;
        Ok(())
    }

    pub async fn center(&self, index: f64) -> Result<Point> {
        let at = self.evaluate(&scripts::center(&js::num(index))).await?;
        if !at.is_object() {
            bail!(
                "element [{}] is gone; look at the latest state and use a fresh index",
                js::num(index)
            );
        }
        Ok(Point {
            x: at["x"].as_f64().unwrap_or(0.0),
            y: at["y"].as_f64().unwrap_or(0.0),
            password: at["password"] == true,
        })
    }

    pub async fn mouse_click(&self, x: f64, y: f64, count: u32) -> Result<()> {
        let (x, y) = (js::json_num(x), js::json_num(y));
        self.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )
        .await?;
        for i in 1..=count {
            for kind in ["mousePressed", "mouseReleased"] {
                let params =
                    json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": i });
                self.send("Input.dispatchMouseEvent", params).await?;
            }
        }
        Ok(())
    }

    pub async fn press_key(&self, combo: &str) -> Result<()> {
        for params in key_events(combo)? {
            self.send("Input.dispatchKeyEvent", params).await?;
        }
        Ok(())
    }

    /// Switches to a tab the last action opened, if any.
    pub async fn adopt_popup(&mut self) -> Result<Option<String>> {
        let id = {
            let mut st = self.st();
            let id = st.popups.pop();
            st.popups.clear();
            id
        };
        let Some(id) = id else { return Ok(None) };
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.attach(&id).await?;
        self.settle(Duration::from_secs(12)).await;
        Ok(Some(id))
    }

    // ---- state -------------------------------------------------------------

    pub async fn tabs(&self) -> Result<Vec<Tab>> {
        let res = self.browser_send("Target.getTargets", json!({})).await?;
        let infos = res["targetInfos"].as_array().cloned().unwrap_or_default();
        let mut tabs: Vec<Tab> = infos
            .into_iter()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| serde_json::from_value::<Tab>(t).ok())
            // The user's browser also lists DevTools and extension pages.
            .filter(|t| {
                !(t.url.starts_with("devtools://") || t.url.starts_with("chrome-extension://"))
            })
            .collect();
        // Stable numbers: first-seen order, not Chrome's most-recently-used order.
        let mut st = self.st();
        st.seen.retain(|id| tabs.iter().any(|t| &t.target_id == id));
        for tab in &tabs {
            if !st.seen.contains(&tab.target_id) {
                st.seen.push(tab.target_id.clone());
            }
        }
        let seen = &st.seen;
        tabs.sort_by_key(|t| seen.iter().position(|id| id == &t.target_id));
        Ok(tabs)
    }

    pub async fn state(&self, quality: f32) -> Result<PageView> {
        let snap: Snap = serde_json::from_value(self.evaluate(scripts::SNAPSHOT).await?)?;
        if snap.width > 0.0 && snap.height > 0.0 {
            self.st().viewport = Some((snap.width, snap.height));
        }
        let metrics = self.send("Page.getLayoutMetrics", json!({})).await.ok();
        let dpr = if self.ops.visible {
            self.evaluate("devicePixelRatio")
                .await
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
        } else {
            1.0
        };
        let mut params = json!({ "format": "jpeg", "quality": js::json_num(js::round(f64::from(quality) * 100.0)) });
        if let Some(vv) = metrics
            .as_ref()
            .map(|m| &m["cssVisualViewport"])
            .filter(|v| v.is_object())
            && dpr != 1.0
        {
            params["clip"] = json!({
                "x": vv["pageX"], "y": vv["pageY"], "width": vv["clientWidth"], "height": vv["clientHeight"],
                "scale": 1.0 / dpr,
            });
        }
        let shot = self.send("Page.captureScreenshot", params).await?;
        let data = shot["data"]
            .as_str()
            .ok_or_else(|| anyhow!("the browser returned no screenshot"))?
            .to_owned();
        let tabs = self.tabs().await?;
        let image = Shot {
            data,
            width: snap.width,
            height: snap.height,
        };
        Ok(PageView { snap, image, tabs })
    }

    /// The outline the model reads; consumes the dialog notes.
    pub fn describe(&self, view: &PageView) -> String {
        let (dialogs, user) = {
            let mut st = self.st();
            (
                std::mem::take(&mut st.dialogs),
                st.user.as_ref().map(|u| u.0),
            )
        };
        describe_state(
            &view.snap,
            &view.tabs,
            self.current_target().as_deref(),
            &mode_text(self.ops.visible, user),
            &dialogs,
        )
    }

    pub async fn read_text(&self, offset: usize, max: usize) -> Result<String> {
        let r = self.evaluate(scripts::TEXT).await?;
        let text = r["text"].as_str().unwrap_or("");
        let text = text_page(text, offset, max);
        Ok(format!(
            "Title: {}\nURL: {}\n\n{text}",
            js::display(r.get("title")),
            js::display(r.get("url"))
        ))
    }
}

fn text_page(text: &str, offset: usize, max: usize) -> String {
    let n = js::len(text);
    let prefix = js::slice(text, offset);
    let offset = js::len(prefix);
    // A UTF-16 surrogate pair needs two units. Even a caller bypassing
    // schema validation must make progress rather than repeat one offset.
    let chunk = js::slice(&text[prefix.len()..], max.max(2));
    let next = offset + js::len(chunk);
    if n > next {
        format!(
            "{chunk}\n… ({} more characters; read again with offset: {next})",
            n - next
        )
    } else {
        chunk.to_owned()
    }
}

/// `Runtime.evaluate` result → text: strings as they are, other values as
/// JSON, an exception as "The expression threw: …".
pub(crate) fn eval_result(res: &Value) -> String {
    if let Some(ex) = res.get("exceptionDetails").filter(|e| !e.is_null()) {
        let exception = &ex["exception"];
        let what = match (exception["description"].as_str(), exception.get("value")) {
            (Some(d), _) if !d.is_empty() => d.to_owned(),
            (_, Some(Value::String(v))) => v.clone(),
            (_, Some(v)) if !v.is_null() && !(v.is_object() || v.is_array()) => v.to_string(),
            _ => js::display(ex.get("text")),
        };
        return format!("The expression threw: {what}");
    }
    let result = &res["result"];
    if let Some(v) = result["unserializableValue"].as_str() {
        return v.to_owned();
    }
    match result.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => match result["type"].as_str() {
            None | Some("undefined") => "undefined".to_owned(),
            Some(kind) => format!("({kind} without a JSON value)"),
        },
    }
}

/// Cuts `text` to `max` UTF-16 units and says how much is missing.
fn cap_text(text: String, max: usize) -> String {
    let n = js::len(&text);
    if n <= max {
        return text;
    }
    let head = js::slice(&text, max.max(2));
    format!(
        "{head}\n… ({} more characters; return a smaller part)",
        n - js::len(head)
    )
}

/// Closes the background Chrome once nothing used it for `idle`.
async fn idle_watch(weak: Weak<Inner>, idle: Duration) {
    let mut wait = idle;
    loop {
        tokio::time::sleep(wait).await;
        let Some(inner) = weak.upgrade() else { return };
        let mut ctl = Ctl::lock(&inner).await;
        if ctl.ops.visible || ctl.attached() || ctl.ops.proc.is_none() {
            return;
        }
        let since = ctl.st().last_use.map_or(idle, |t| t.elapsed());
        if since < idle {
            wait = idle - since;
            continue;
        }
        // `close` stops the timers; take our own handle first so it does not
        // abort this very task halfway through.
        inner
            .shared
            .idle_timer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        tracing::debug!("browser idle for {idle:?}, closing");
        ctl.close().await;
        drop(ctl);
        if inner.mini.enabled() {
            inner.mini.done().await;
        }
        return;
    }
}

fn create_profile_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| {
            anyhow!(
                "could not create the browser profile {}: {e}",
                dir.display()
            )
        })
}

/// Chrome reopens the tabs of its previous run on every start, even headless
/// and after a clean exit. Tabs are carried over explicitly (show/hide) or
/// not at all (restart after idle), so drop that record; cookies stay.
fn forget_session(profile: &Path) {
    let default = profile.join("Default");
    let _ = std::fs::remove_dir_all(default.join("Sessions"));
    for legacy in [
        "Current Session",
        "Current Tabs",
        "Last Session",
        "Last Tabs",
    ] {
        let _ = std::fs::remove_file(default.join(legacy));
    }
}

/// Chrome's command line, as in browser.js (plus `--no-sandbox` for root on
/// Linux, where Chrome refuses to start otherwise).
pub(crate) fn launch_args(opts: &BrowserOptions, visible: bool) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", opts.profile_dir.display()),
        "--remote-debugging-pipe".to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-sync".to_owned(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".to_owned(),
        "--hide-crash-restore-bubble".to_owned(),
        format!(
            "--window-size={},{}",
            opts.width,
            opts.height + if visible { 88 } else { 0 }
        ),
        format!(
            "--lang={}",
            if opts.locale == "de" {
                "de-DE"
            } else {
                "en-US"
            }
        ),
    ];
    if !visible {
        args.push("--headless=new".to_owned());
    }
    if running_as_root_on_linux() {
        args.push("--no-sandbox".to_owned());
    }
    // Without a reachable login keychain (other HOME, sandboxed shell) macOS
    // would ask the user to reset it on every start.
    if cfg!(target_os = "macos") && opts.mock_keychain {
        args.push("--use-mock-keychain".to_owned());
    }
    args.push("about:blank".to_owned());
    args
}

fn running_as_root_on_linux() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    cfg!(target_os = "linux") && unsafe { libc::geteuid() } == 0
}

/// A visible window needs a display; on Linux servers there is none.
fn check_display() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    let set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    if set("DISPLAY") || set("WAYLAND_DISPLAY") {
        return Ok(());
    }
    bail!(
        "Cannot show the browser window: this machine has no display (neither DISPLAY nor WAYLAND_DISPLAY is set). The browser stays in the background."
    )
}

/// The .app bundle around a Chrome binary, for the mini window's icon.
fn app_bundle(binary: &Path) -> Option<String> {
    let s = binary.to_str()?;
    let i = s.find(".app/")?;
    (i > 0).then(|| s[..i + 4].to_owned())
}

/// URL or search words → the URL to open (browser.js `navigate`).
pub(crate) fn normalize_url(raw: &str) -> Result<String> {
    let mut url = js::trim(raw).to_owned();
    if !has_scheme(&url) {
        url = if looks_like_domain(&url) || url.starts_with("localhost") {
            format!("https://{url}")
        } else {
            format!(
                "https://duckduckgo.com/?q={}",
                js::encode_uri_component(&url)
            )
        };
    }
    let lower = url.to_lowercase();
    if !(lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("about:blank"))
    {
        bail!("only http(s) pages can be opened in the background browser");
    }
    Ok(url)
}

/// `/^[a-z][a-z0-9+.-]*:/i`
fn has_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return false;
        }
    }
    false
}

/// `/^[^\s]+\.[^\s]{2,}(\/.*)?$/`
fn looks_like_domain(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let first_space = chars
        .iter()
        .position(|&c| js::is_space(c))
        .unwrap_or(chars.len());
    let line_end = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    (1..first_space.min(chars.len()))
        .filter(|&p| chars[p] == '.')
        .any(|p| {
            let rest = &chars[p + 1..];
            let k = rest
                .iter()
                .position(|&c| js::is_space(c))
                .unwrap_or(rest.len());
            (2..=k).any(|j| {
                j == rest.len() || (rest[j] == '/' && !rest[j + 1..].iter().any(|&c| line_end(c)))
            })
        })
}

/// Key name → (key, code, keyCode, text).
fn named_key(name: &str) -> Option<(&'static str, &'static str, u32, Option<&'static str>)> {
    Some(match name {
        "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" | "esc" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" => ("Delete", "Delete", 46, None),
        "space" => (" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        _ => return None,
    })
}

fn edit_command(name: &str) -> Option<&'static str> {
    Some(match name {
        "a" => "selectAll",
        "c" => "copy",
        "v" => "paste",
        "x" => "cut",
        "z" => "undo",
        _ => return None,
    })
}

/// The two `Input.dispatchKeyEvent` calls for a key or combination ("cmd+a").
pub(crate) fn key_events(combo: &str) -> Result<[Value; 2]> {
    let mut parts: Vec<&str> = combo
        .split('+')
        .map(js::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let name = parts.pop().unwrap_or("");
    let mut modifiers = 0u32;
    let mut meta = false;
    for m in parts.iter().map(|p| p.to_lowercase()) {
        match m.as_str() {
            "alt" | "option" => modifiers |= 1,
            "ctrl" | "control" => modifiers |= 2,
            "cmd" | "command" | "meta" => {
                modifiers |= 4;
                meta = true;
            }
            "shift" => modifiers |= 8,
            _ => bail!("unknown modifier \"{m}\""),
        }
    }
    let lower = name.to_lowercase();
    let (key, code, key_code, text): (String, Option<String>, u32, Option<String>) =
        if let Some((key, code, key_code, text)) = named_key(&lower) {
            (
                key.into(),
                Some(code.into()),
                key_code,
                text.map(str::to_owned),
            )
        } else if js::len(name) == 1 {
            let c = name.chars().next().unwrap_or_default();
            let code = if c.is_ascii_alphabetic() {
                Some(format!("Key{}", c.to_ascii_uppercase()))
            } else if c.is_ascii_digit() {
                Some(format!("Digit{c}"))
            } else {
                None
            };
            let upper = c.to_uppercase().next().unwrap_or(c);
            let text = (modifiers & 7 == 0).then(|| name.to_owned());
            (name.to_owned(), code, upper as u32, text)
        } else {
            bail!("unknown key \"{name}\"");
        };
    let command = if meta || modifiers & 2 != 0 {
        edit_command(&lower)
    } else {
        None
    };
    let mut down = serde_json::Map::new();
    down.insert(
        "type".into(),
        if text.is_some() {
            "keyDown"
        } else {
            "rawKeyDown"
        }
        .into(),
    );
    down.insert("key".into(), key.clone().into());
    if let Some(code) = &code {
        down.insert("code".into(), code.clone().into());
    }
    down.insert("windowsVirtualKeyCode".into(), key_code.into());
    down.insert("modifiers".into(), modifiers.into());
    if let Some(text) = text {
        down.insert("text".into(), text.into());
    }
    if let Some(command) = command {
        down.insert("commands".into(), json!([command]));
    }
    let mut up = serde_json::Map::new();
    up.insert("type".into(), "keyUp".into());
    up.insert("key".into(), key.into());
    if let Some(code) = code {
        up.insert("code".into(), code.into());
    }
    up.insert("windowsVirtualKeyCode".into(), key_code.into());
    up.insert("modifiers".into(), modifiers.into());
    Ok([Value::Object(down), Value::Object(up)])
}

/// First words of the state: which browser this is.
pub(crate) fn mode_text(visible: bool, user: Option<&str>) -> String {
    match user {
        Some(name) => format!("The user's own {name} (their tabs and sign-ins)"),
        None if visible => "Background browser (visible window)".to_owned(),
        None => "Background browser (headless, the user sees it in the mini window)".to_owned(),
    }
}

/// browser.js `describe(state)`.
pub(crate) fn describe_state(
    snap: &Snap,
    tabs: &[Tab],
    current: Option<&str>,
    mode: &str,
    dialogs: &[String],
) -> String {
    let index = tabs
        .iter()
        .position(|t| Some(t.target_id.as_str()) == current)
        .map_or(0, |i| i + 1);
    let mut lines = vec![
        format!("{mode} · tab {index}/{}", tabs.len()),
        format!(
            "Title: {}",
            if snap.title.is_empty() {
                "(none)"
            } else {
                &snap.title
            }
        ),
        format!("URL: {}", snap.url),
        format!(
            "Viewport {}×{}, scrolled {} of {} px. Screenshot pixels = page coordinates.",
            js::num(snap.width),
            js::num(snap.height),
            js::num(snap.scroll_y),
            js::num((snap.scroll_height - snap.height).max(0.0))
        ),
    ];
    if !dialogs.is_empty() {
        lines.push(format!("Dialogs: {}", dialogs.join("; ")));
    }
    lines.push("Interactive elements, [index] kind \"name\" @(x,y):".to_owned());
    for it in &snap.items {
        let mut line = format!("[{}] {}", js::num(it.i), it.kind);
        if !it.name.is_empty() {
            line += &format!(" \"{}\"", it.name);
        }
        if it.password == Some(true) {
            line += " (password)";
        } else if let Some(v) = it.value.as_deref().filter(|v| !v.is_empty()) {
            line += &format!(" value=\"{v}\"");
        }
        if let Some(checked) = it.checked {
            line += if checked { " checked" } else { " unchecked" };
        }
        if it.disabled == Some(true) {
            line += " disabled";
        }
        if snap.focused.as_deref() == Some(js::num(it.i).as_str()) {
            line += " focused";
        }
        lines.push(format!("{line} @({},{})", js::num(it.x), js::num(it.y)));
    }
    if snap.items.is_empty() {
        lines.push("(none visible; scroll, wait or read the text)".to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod pagination_tests {
    #[test]
    fn unicode_pages_always_advance_without_splitting_surrogate_pairs() {
        assert_eq!(
            super::text_page("😀tail", 0, 1),
            "😀\n… (4 more characters; read again with offset: 2)"
        );
        assert_eq!(
            super::text_page("😀tail", 1, 2),
            "😀\n… (4 more characters; read again with offset: 2)"
        );
        assert_eq!(super::text_page("😀tail", 2, 10), "tail");
        assert_eq!(super::text_page("😀tail", 100, 10), "");
    }
}
