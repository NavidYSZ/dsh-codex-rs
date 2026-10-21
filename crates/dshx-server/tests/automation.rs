//! The automation routes (goals, schedules, Agent Teams) and their stream
//! messages against fake backends.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, NotFound, PromptRequest, StreamMessage, serve};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Implements only the required methods (automation stays at its defaults)
/// or, with `automation`, answers the automation routes too.
struct Fake {
    automation: bool,
    calls: Mutex<Vec<String>>,
    tx: broadcast::Sender<StreamMessage>,
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value> {
        Ok(patch)
    }
    async fn workspaces(&self) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn add_workspace(&self, path: String) -> anyhow::Result<Value> {
        Ok(json!({ "path": path }))
    }
    async fn list_sessions(&self, _cwd: Option<String>) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn create_session(&self, _cwd: String) -> anyhow::Result<String> {
        Ok("s1".into())
    }
    async fn session(&self, _id: &str) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
    async fn events_after(
        &self,
        _id: &str,
        _after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(vec![])
    }
    async fn prompt(&self, _id: &str, _request: PromptRequest) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_session(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn approve(&self, _call_id: &str, _decision: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn save_attachment(
        &self,
        _data: Vec<u8>,
        _media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        Err(anyhow::anyhow!("unused"))
    }
    async fn load_attachment(&self, _id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Err(anyhow::anyhow!("unused"))
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }

    async fn goal(&self, id: &str) -> anyhow::Result<Value> {
        self.record(format!("goal {id}"))?;
        Ok(json!({"goal": null}))
    }
    async fn goal_command(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        self.record(format!("goal_command {id} {body}"))?;
        Ok(json!({"goal": {"phase": "active"}}))
    }
    async fn schedules(&self) -> anyhow::Result<Value> {
        self.record("schedules".into())?;
        Ok(json!([]))
    }
    async fn session_schedules(&self, id: &str) -> anyhow::Result<Value> {
        self.record(format!("session_schedules {id}"))?;
        Ok(json!([]))
    }
    async fn update_schedule(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        self.record(format!("update_schedule {id} {body}"))?;
        Ok(json!({"id": id}))
    }
    async fn delete_schedule(&self, id: &str) -> anyhow::Result<Value> {
        self.record(format!("delete_schedule {id}"))?;
        Ok(json!({"id": id, "deleted": true}))
    }
    async fn schedule_history(
        &self,
        id: &str,
        limit: Option<i64>,
        before: Option<String>,
    ) -> anyhow::Result<Value> {
        self.record(format!("schedule_history {id} {limit:?} {before:?}"))?;
        Ok(json!({"id": id, "records": []}))
    }
    async fn team(&self, id: &str) -> anyhow::Result<Value> {
        self.record(format!("team {id}"))?;
        Ok(json!({"members": [], "tasks": []}))
    }
}

impl Fake {
    fn record(&self, call: String) -> anyhow::Result<()> {
        if !self.automation {
            return Err(NotFound("automation".into()).into());
        }
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

async fn start(automation: bool) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        automation,
        calls: Mutex::new(vec![]),
        tx: broadcast::channel(16).0,
    });
    let (addr, _) = serve(fake.clone(), "secret".into(), 0).await.unwrap();
    (format!("127.0.0.1:{}", addr.port()), fake)
}

#[tokio::test]
async fn automation_routes_map_to_the_backend() {
    let (addr, fake) = start(true).await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}/api{p}");
    let get = |p: &str| http.get(url(p)).bearer_auth("secret").send();
    assert_eq!(
        get("/sessions/s1/goal")
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!({"goal": null})
    );
    let r = http
        .post(url("/sessions/s1/goal"))
        .bearer_auth("secret")
        .json(&json!({"action": "pause"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(get("/schedules").await.unwrap().status(), 200);
    assert_eq!(get("/sessions/s1/schedules").await.unwrap().status(), 200);
    assert_eq!(get("/sessions/s1/team").await.unwrap().status(), 200);
    let r = http
        .put(url("/schedules/sch-1"))
        .bearer_auth("secret")
        .json(&json!({"title": "New"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["id"], "sch-1");
    let r = http
        .delete(url("/schedules/sch-1"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["deleted"], true);
    assert_eq!(
        get("/schedules/sch-1/history?limit=5&before=m1")
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        [
            "goal s1",
            "goal_command s1 {\"action\":\"pause\"}",
            "schedules",
            "session_schedules s1",
            "team s1",
            "update_schedule sch-1 {\"title\":\"New\"}",
            "delete_schedule sch-1",
            "schedule_history sch-1 Some(5) Some(\"m1\")",
        ]
    );
}

#[tokio::test]
async fn backends_without_automation_answer_404() {
    let (addr, _) = start(false).await;
    let r = reqwest::Client::new()
        .get(format!("http://{addr}/api/schedules"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn goal_and_schedule_messages_reach_their_clients() {
    let (addr, fake) = start(true).await;
    let next = |m: Option<Result<tokio_tungstenite::tungstenite::Message, _>>| -> Value {
        serde_json::from_str(m.unwrap().unwrap().to_text().unwrap()).unwrap()
    };
    let (mut session_ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/stream?session=s1&token=secret"))
            .await
            .unwrap();
    let (mut sidebar_ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/stream?token=secret"))
            .await
            .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    fake.tx
        .send(StreamMessage::Goal {
            session_id: "other".into(),
            goal: None,
        })
        .unwrap();
    fake.tx
        .send(StreamMessage::Goal {
            session_id: "s1".into(),
            goal: Some(json!({"phase": "active"})),
        })
        .unwrap();
    fake.tx.send(StreamMessage::ScheduleChanged).unwrap();
    assert_eq!(
        next(session_ws.next().await),
        json!({"kind": "goal", "sessionId": "s1", "goal": {"phase": "active"}})
    );
    assert_eq!(
        next(session_ws.next().await),
        json!({"kind": "schedule-changed"})
    );
    assert_eq!(
        next(sidebar_ws.next().await),
        json!({"kind": "goal", "sessionId": "other", "goal": null})
    );
    assert_eq!(next(sidebar_ws.next().await)["sessionId"], "s1");
    assert_eq!(
        next(sidebar_ws.next().await),
        json!({"kind": "schedule-changed"})
    );
}
