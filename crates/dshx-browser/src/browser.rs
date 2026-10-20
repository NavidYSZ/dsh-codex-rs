//! Background browser for the `browser` tool (port of `lib/browser.js`): its
//! own Chrome profile driven over the DevTools protocol through a pipe (no
//! debugging port), headless unless the user asks to see it. Screencast
//! frames and the agent cursor go to the mini window when there is one.
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
    /// Chrome quits after this long without a browser action.
    pub idle: Duration,
    /// `de` or `en`: Chrome's UI language and the mini window's status line.
    pub locale: String,
    /// Screenshot JPEG quality, 0…1.
    pub quality: f32,
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
    child: Child,
    pid: i32,
    cdp: Arc<Cdp>,
}

impl Drop for Proc {
    // Last line of defence (cancelled close, dropped browser): close the pipe
    // so Chrome quits by itself, and kill the whole group if it does not.
    fn drop(&mut self) {
        self.cdp.shutdown();
        let running = matches!(self.child.try_wait(), Ok(None));
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

#[derive(Default)]
struct Ops {
    proc: Option<Proc>,
    visible: bool,
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
/// the first action and quits after `idle` without one.
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

    /// Whether a Chrome binary was found.
    pub fn available(&self) -> bool {
        self.inner.opts.chrome.is_some()
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
                drop(st);
                let ack = json!({ "sessionId": ev.params["sessionId"] });
                cdp.post("Page.screencastFrameAck", ack, ev.session_id);
                if let Some(data) = ev.params["data"].as_str() {
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

    pub fn opts(&self) -> &BrowserOptions {
        &self.inner.opts
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
            Some(p) => !p.cdp.is_closed() && matches!(p.child.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub async fn ensure(&mut self) -> Result<()> {
        self.touch();
        self.arm_idle();
        if self.alive() {
            if self.st().page.is_some() {
                return Ok(());
            }
            // The tab went away (closed by the page or the user): take another.
            if self.reattach().await.is_ok() {
                return Ok(());
            }
        }
        let visible = self.ops.visible;
        self.launch(visible, Vec::new()).await
    }

    /// Attaches to the first tab, or a new blank one when there is none.
    pub async fn reattach(&mut self) -> Result<()> {
        let first = self.tabs().await?.into_iter().next().map(|t| t.target_id);
        let target_id = match first {
            Some(id) => id,
            None => self.create_target("about:blank", false).await?,
        };
        self.attach(&target_id).await
    }

    /// Whether the current tab still exists.
    pub fn has_page(&self) -> bool {
        self.st().page.is_some()
    }

    async fn launch(&mut self, visible: bool, urls: Vec<String>) -> Result<()> {
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
        self.ops.proc = Some(Proc { child, pid, cdp });
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
        let _ = self
            .browser_send("Browser.setDownloadBehavior", json!({ "behavior": "deny" }))
            .await;
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
        res["targetId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Target.createTarget returned no target"))
    }

    /// Kills a Chrome that is gone wrong (crashed, half started) without asking.
    async fn discard(&mut self) {
        let Some(mut p) = self.ops.proc.take() else {
            return;
        };
        self.forget_page();
        p.cdp.shutdown();
        if matches!(p.child.try_wait(), Ok(None)) {
            cdp::kill_group(p.pid);
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), p.child.wait()).await;
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
    }

    /// Quits Chrome: `Browser.close`, then SIGKILL to the group after 2 s.
    pub async fn close(&mut self) {
        self.inner.shared.stop_timers();
        let Some(mut p) = self.ops.proc.take() else {
            return;
        };
        self.forget_page();
        let _ = p
            .cdp
            .send("Browser.close", json!({}), None, Duration::from_secs(3))
            .await;
        p.cdp.shutdown();
        if tokio::time::timeout(Duration::from_secs(2), p.child.wait())
            .await
            .is_err()
        {
            cdp::kill_group(p.pid);
            let _ = p.child.wait().await;
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
        if self.ops.visible {
            return; // the user is looking at it; never pull it away
        }
        let weak = Arc::downgrade(self.inner);
        let idle = self.inner.opts.idle;
        *slot = Some(tokio::spawn(idle_watch(weak, idle)).abort_handle());
    }

    /// Shows the browser window to the user (or hides it again), keeping tabs.
    pub async fn set_visible(&mut self, on: bool) -> Result<()> {
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
        let icon = opts
            .chrome
            .as_deref()
            .and_then(app_bundle)
            .unwrap_or_else(|| "globe".to_owned());
        let show = sink
            .show(MiniShow {
                title,
                icon,
                width: opts.width,
                height: opts.height,
                activity: activity.to_owned(),
            })
            .await;
        handle_requests(&Arc::downgrade(self.inner), &show);
        if let Some(message) = show.stopped {
            return Err(anyhow::Error::msg(message));
        }
        if let Some(p) = point {
            let step = MiniStep {
                x: p.x / f64::from(opts.width),
                y: p.y / f64::from(opts.height),
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
                match weak.upgrade() {
                    Some(inner) if inner.shared.st().pid.is_some() => {}
                    _ => return,
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
        Ok(infos
            .into_iter()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| serde_json::from_value(t).ok())
            .collect())
    }

    pub async fn state(&self, quality: f32) -> Result<PageView> {
        let snap: Snap = serde_json::from_value(self.evaluate(scripts::SNAPSHOT).await?)?;
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
        let dialogs = std::mem::take(&mut self.st().dialogs);
        describe_state(
            &view.snap,
            &view.tabs,
            self.current_target().as_deref(),
            self.ops.visible,
            &dialogs,
        )
    }

    pub async fn read_text(&self, max: usize) -> Result<String> {
        let r = self.evaluate(scripts::TEXT).await?;
        let text = r["text"].as_str().unwrap_or("");
        let n = js::len(text);
        let text = if n > max {
            format!(
                "{}\n… ({} more characters; scroll and read again for more)",
                js::slice(text, max),
                n - max
            )
        } else {
            text.to_owned()
        };
        Ok(format!(
            "Title: {}\nURL: {}\n\n{text}",
            js::display(r.get("title")),
            js::display(r.get("url"))
        ))
    }
}

/// Closes Chrome once nothing used it for `idle`.
async fn idle_watch(weak: Weak<Inner>, idle: Duration) {
    let mut wait = idle;
    loop {
        tokio::time::sleep(wait).await;
        let Some(inner) = weak.upgrade() else { return };
        let mut ctl = Ctl::lock(&inner).await;
        if ctl.ops.visible || ctl.ops.proc.is_none() {
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

/// browser.js `describe(state)`.
pub(crate) fn describe_state(
    snap: &Snap,
    tabs: &[Tab],
    current: Option<&str>,
    visible: bool,
    dialogs: &[String],
) -> String {
    let index = tabs
        .iter()
        .position(|t| Some(t.target_id.as_str()) == current)
        .map_or(0, |i| i + 1);
    let mode = if visible {
        "visible window"
    } else {
        "headless, the user sees it in the mini window"
    };
    let mut lines = vec![
        format!("Background browser ({mode}) · tab {index}/{}", tabs.len()),
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
