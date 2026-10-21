//! Live view for the web UI's side panel: the background browser's
//! screencast and the Mac tools' screenshots, tagged with the chat whose tool
//! call produced them. Messages go through a small channel of their own, so a
//! slow viewer only misses frames, never session events.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dshx_browser::{MiniReply, MiniShow, MiniSink, MiniStep};
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolResultPart, ToolSchema, now_ms};
use dshx_server::StreamMessage;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// At most four frames a second reach the viewers.
const MIN_GAP: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    /// Chat of the browser call that runs (or ran last).
    owner: Option<String>,
    last_frame: Option<Instant>,
    /// Last message per chat, so a viewer that connects later sees something.
    latest: HashMap<String, Value>,
}

pub struct LiveHub {
    tx: broadcast::Sender<StreamMessage>,
    st: Mutex<State>,
    /// Set by the web front end; headless runs keep the browser's old behaviour.
    viewers: AtomicBool,
}

impl LiveHub {
    pub fn new() -> Arc<LiveHub> {
        Arc::new(LiveHub {
            tx: broadcast::channel(16).0,
            st: Mutex::default(),
            viewers: AtomicBool::new(false),
        })
    }

    pub fn enable_viewers(&self) {
        self.viewers.store(true, Ordering::Relaxed);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }

    pub fn latest(&self, session: &str) -> Option<StreamMessage> {
        let st = self.st.lock().unwrap();
        st.latest.get(session).map(|data| StreamMessage::Live {
            session_id: session.to_owned(),
            data: data.clone(),
        })
    }

    /// Sends `patch` merged over the chat's last message (tool, place and the
    /// last image stay until something replaces them).
    fn publish(&self, st: &mut State, session: &str, patch: Value) {
        let mut data = st.latest.remove(session).unwrap_or_else(|| json!({}));
        if let (Some(obj), Some(new)) = (data.as_object_mut(), patch.as_object()) {
            for (k, v) in new {
                obj.insert(k.clone(), v.clone());
            }
            obj.insert("time".into(), json!(now_ms()));
        }
        st.latest.insert(session.to_owned(), data.clone());
        let _ = self.tx.send(StreamMessage::Live {
            session_id: session.to_owned(),
            data,
        });
    }

    fn with_owner(&self, patch: Value) {
        let mut st = self.st.lock().unwrap();
        if let Some(owner) = st.owner.clone() {
            self.publish(&mut st, &owner, patch);
        }
    }

    /// A Codex tool call starts in `session`.
    fn begin(&self, session: &str, tool: &str, args: &Value) {
        let mut st = self.st.lock().unwrap();
        if tool == "browser" {
            st.owner = Some(session.to_owned());
        }
        self.publish(
            &mut st,
            session,
            json!({ "tool": tool, "args": args, "active": true, "ended": false, "error": null }),
        );
    }

    fn finish(&self, session: &str, output: &Result<ToolOutput, ToolError>) {
        let mut st = self.st.lock().unwrap();
        let error = match output {
            Ok(o) if o.is_error => Some("error".to_owned()),
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        let mut patch = json!({ "active": false, "error": error });
        // computer/app: the screenshot taken after the step is the live picture.
        if let Ok(o) = output {
            let image = o.content.iter().rev().find_map(|p| match p {
                ToolResultPart::Image { attachment } => Some(attachment.clone()),
                _ => None,
            });
            if let Some(image) = image {
                patch["attachment"] = json!(image);
                patch["meta"] = o.meta.clone().unwrap_or(Value::Null);
            }
        }
        self.publish(&mut st, session, patch);
    }

    fn frame(&self, jpeg: String) {
        let mut st = self.st.lock().unwrap();
        let Some(owner) = st.owner.clone() else {
            return;
        };
        let now = Instant::now();
        if st.last_frame.is_some_and(|t| now - t < MIN_GAP) {
            return;
        }
        st.last_frame = Some(now);
        self.publish(
            &mut st,
            &owner,
            json!({ "image": jpeg, "attachment": null, "ended": false }),
        );
    }
}

/// The browser's mini-window sink plus the web viewers.
pub struct LiveMini {
    pub inner: Arc<dyn MiniSink>,
    pub hub: Arc<LiveHub>,
}

#[async_trait]
impl MiniSink for LiveMini {
    fn enabled(&self) -> bool {
        self.inner.enabled() || self.hub.viewers.load(Ordering::Relaxed)
    }

    async fn show(&self, show: MiniShow) -> MiniReply {
        let place = show
            .title
            .split_once(" · ")
            .map_or("", |(_, host)| host)
            .to_owned();
        self.hub.with_owner(json!({
            "activity": show.activity,
            "place": place,
            "width": show.width,
            "height": show.height,
        }));
        if self.inner.enabled() {
            self.inner.show(show).await
        } else {
            MiniReply::default()
        }
    }

    async fn step(&self, step: MiniStep) -> MiniReply {
        self.hub
            .with_owner(json!({ "cursor": { "x": step.x, "y": step.y, "click": step.click } }));
        if self.inner.enabled() {
            self.inner.step(step).await
        } else {
            MiniReply::default()
        }
    }

    async fn frame(&self, jpeg_base64: String) -> MiniReply {
        if self.inner.enabled() {
            self.hub.frame(jpeg_base64.clone());
            self.inner.frame(jpeg_base64).await
        } else {
            self.hub.frame(jpeg_base64);
            MiniReply::default()
        }
    }

    async fn poll(&self) -> MiniReply {
        if self.inner.enabled() {
            self.inner.poll().await
        } else {
            MiniReply::default()
        }
    }

    async fn done(&self) {
        self.hub
            .with_owner(json!({ "ended": true, "active": false }));
        if self.inner.enabled() {
            self.inner.done().await;
        }
    }
}

/// A Codex tool (browser, computer, app) that reports its calls to the hub.
/// Everything the model sees is the wrapped tool's own.
pub struct Watched {
    pub inner: Arc<dyn Tool>,
    pub hub: Arc<LiveHub>,
}

#[async_trait]
impl Tool for Watched {
    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }
    fn concurrency_safe(&self) -> bool {
        self.inner.concurrency_safe()
    }
    fn timeout(&self) -> Option<Duration> {
        self.inner.timeout()
    }
    fn describe(&self, args: &Value) -> String {
        self.inner.describe(args)
    }
    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let tool = self.inner.schema().name;
        self.hub.begin(&ctx.session_id, &tool, &args);
        let session = ctx.session_id.clone();
        let output = self.inner.execute(args, ctx).await;
        self.hub.finish(&session, &output);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(m: StreamMessage) -> (String, Value) {
        match m {
            StreamMessage::Live { session_id, data } => (session_id, data),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn frames_follow_the_browser_call_and_are_throttled() {
        let hub = LiveHub::new();
        hub.enable_viewers();
        let mini = LiveMini {
            inner: Arc::new(dshx_browser::NoMini),
            hub: hub.clone(),
        };
        assert!(mini.enabled(), "viewers turn the screencast on");
        let mut rx = hub.subscribe();
        // No call yet: frames have no chat to go to.
        mini.frame("x".into()).await;
        assert!(rx.try_recv().is_err());
        hub.begin("s1", "browser", &json!({ "action": "open" }));
        let (sid, d) = data(rx.try_recv().unwrap());
        assert_eq!((sid.as_str(), d["active"].as_bool()), ("s1", Some(true)));
        mini.frame("f1".into()).await;
        mini.frame("f2".into()).await; // within 250 ms: dropped
        let (_, d) = data(rx.try_recv().unwrap());
        assert_eq!(d["image"], "f1");
        assert_eq!(d["tool"], "browser", "earlier fields stay");
        assert!(rx.try_recv().is_err());
        // A late viewer gets the merged latest state.
        let (_, d) = data(hub.latest("s1").unwrap());
        assert_eq!(
            (d["image"].as_str(), d["args"]["action"].as_str()),
            (Some("f1"), Some("open"))
        );
        mini.done().await;
        let (_, d) = data(rx.try_recv().unwrap());
        assert_eq!(d["ended"], true);
    }
}
