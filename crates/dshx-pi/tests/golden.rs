//! Replays event streams recorded from the pinned Pi 1.5.0 with the faux
//! provider (`extension/aether-faux.ts`, no network) through the mapper and
//! checks the resulting Aether log with dsh's own admission rules.
use dshx_core::{Message, NewEvent, SESSION_FORMAT_VERSION, SessionEvent, SessionHeader};
use dshx_pi::mapper::{Mapper, Route, Target};
use serde_json::{Value, json};

const CWD: &str = "/work/project";

fn header() -> SessionHeader {
    let mut extra = serde_json::Map::new();
    extra.insert("engine".into(), json!("pi"));
    extra.insert("engineVersion".into(), json!(dshx_pi::PI_VERSION));
    SessionHeader {
        kind: "session".into(),
        version: SESSION_FORMAT_VERSION,
        id: "session-golden".into(),
        created_at: 1,
        cwd: CWD.into(),
        is_seeded: false,
        delegation_depth: 0,
        extra,
    }
}

struct Replay {
    mapper: Mapper,
    events: Vec<SessionEvent>,
    deltas: usize,
}

impl Replay {
    fn new() -> Self {
        let mut mapper = Mapper::new(CWD);
        mapper.set_route(
            "pi-faux",
            "faux-1",
            Route {
                provider: "pi-faux".into(),
                model: "faux-1".into(),
                context_window: Some(64000),
            },
        );
        Replay {
            mapper,
            events: Vec::new(),
            deltas: 0,
        }
    }

    fn push(&mut self, new: Vec<NewEvent>) {
        for e in new {
            let seq = self.events.len() as u64;
            self.events.push(SessionEvent {
                kind: e.kind,
                seq,
                time: 1000 + seq,
                data: e.data,
                extra: e.extra,
            });
        }
        assert_eq!(self.mapper.next_seq(), self.events.len() as u64);
    }

    /// Drives the mapper the way `PiChat` does for each recorded line.
    fn line(&mut self, record: Value) {
        if let Some(sent) = record.get("_sent") {
            if sent["type"] == "prompt" {
                let message = Message::user_text(sent["message"].as_str().unwrap());
                if sent["streamingBehavior"] == "steer" {
                    let events = self.mapper.enqueue(Target::NextStep, message);
                    self.push(events);
                } else {
                    let events = self.mapper.enqueue(Target::NextTurn, message);
                    self.push(events);
                    if self.mapper.running().is_none() {
                        let (events, _) = self.mapper.begin_turn().unwrap();
                        self.push(events);
                    }
                }
            }
            return;
        }
        if record["type"] == "response" {
            if record["command"] == "clear_queue" {
                let texts: Vec<String> =
                    serde_json::from_value(record["data"]["steering"].clone()).unwrap();
                let events = self.mapper.restore_steers(&texts);
                self.push(events);
            }
            if record["command"] == "get_entries"
                && let Some(leaf) = record["data"]["leafId"].as_str()
            {
                let events = self.mapper.mark_cursor(leaf);
                self.push(events);
            }
            return;
        }
        let out = self.mapper.on_record(&record);
        self.deltas += out.deltas.len();
        self.push(out.events);
    }

    fn run(name: &str) -> Replay {
        let path = format!("{}/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        let mut replay = Replay::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            replay.line(serde_json::from_str(line).unwrap());
        }
        if std::env::var("DSHX_PI_DUMP").as_deref() == Ok(name) {
            for e in &replay.events {
                eprintln!("{} {} {} {:?}", e.seq, e.kind, e.data, e.extra);
            }
        }
        if let Err(e) = dshx_session::validate::validate_log(&header(), &replay.events) {
            for e in &replay.events {
                eprintln!("{} {} {}", e.seq, e.kind, e.data);
            }
            panic!("{name}: {e}");
        }
        replay
    }

    fn of(&self, kind: &str) -> Vec<&SessionEvent> {
        self.events.iter().filter(|e| e.kind == kind).collect()
    }

    fn end_reason(&self) -> Value {
        self.of("turn/end").last().unwrap().data["reason"].clone()
    }
}

#[test]
fn a_text_answer() {
    let r = Replay::run("text");
    assert!(r.deltas > 0);
    let answer = &r.of("assistant/message")[0].data;
    assert_eq!(answer["message"]["content"][0]["type"], "reasoning");
    assert_eq!(answer["message"]["content"][1]["text"], "Echo: hello there");
    assert_eq!(answer["message"]["source"]["provider"], "pi-faux");
    assert!(answer["usage"]["outputTokens"].as_u64().unwrap() > 0);
    assert_eq!(answer["usage"]["costUsd"], 0.0);
    assert_eq!(r.of("session/title")[0].data["title"], "hello there");
    assert_eq!(r.of("request/context")[0].data["contextWindow"], 64000);
    assert_eq!(r.end_reason()["kind"], "completed");
    assert_eq!(r.of("pi/cursor").len(), 1);
    // The prompt left the queue when Pi took it.
    assert!(r.mapper.queued_turns().is_empty());
    let messages = dshx_session::derive_messages(&r.events);
    assert_eq!(messages.len(), 2);
}

#[test]
fn bash_rows_carry_the_exit_code() {
    let r = Replay::run("bash");
    let result = &r.of("tool/result")[0];
    assert_eq!(result.data["meta"]["exitCode"], 0);
    assert_eq!(result.data["message"]["isError"], false);
    let call = &r.of("tool/call")[0].data;
    assert_eq!(call["name"], "bash");
    assert_eq!(
        result.extra["sourceEventSeqs"],
        json!([r.of("tool/call")[0].seq])
    );
    // Two Pi turns: the call and the answer after it.
    assert_eq!(r.of("step/start").len(), 2);

    let failed = Replay::run("bash-fail");
    let result = &failed.of("tool/result")[0].data;
    assert_eq!(result["meta"]["exitCode"], 3);
    assert_eq!(result["message"]["isError"], true);
}

#[test]
fn file_rows_use_aether_argument_names() {
    let r = Replay::run("files");
    let results = r.of("tool/result");
    assert_eq!(results[0].data["meta"]["path"], "/work/project/notes2.txt");
    let edit = &results[1].data["meta"];
    assert_eq!(edit["diffs"][0]["oldText"], "hello");
    assert_eq!(edit["diffs"][0]["newText"], "bye");
    assert_eq!(results[2].data["meta"]["path"], "/work/project/notes2.txt");
    assert_eq!(r.of("turn/end").len(), 3);
}

#[test]
fn a_steer_joins_the_running_turn() {
    let r = Replay::run("steer");
    assert_eq!(r.of("turn/start").len(), 1);
    let users = r.of("user/message");
    assert_eq!(users.len(), 2);
    assert_eq!(users[1].data["content"][0]["text"], "steered text");
    // The steer waited in next-step and left it when Pi delivered it,
    // before the step that answers it.
    let splices = r.of("agent/inbox/spliced");
    assert!(
        splices
            .iter()
            .any(|e| e.data["target"] == "next-step" && e.data["removedCount"] == 1)
    );
    let second_step = r.of("step/start")[1].seq;
    assert!(users[1].seq < second_step);
    assert!(r.mapper.queued_steers().is_empty());

    let q = Replay::run("queue");
    assert_eq!(q.of("turn/start").len(), 2);
    assert_eq!(q.of("turn/end").len(), 2);
    assert!(q.mapper.queued_turns().is_empty());
}

#[test]
fn stop_keeps_the_steer_visible() {
    let r = Replay::run("abort");
    assert_eq!(r.end_reason()["kind"], "aborted");
    // The steer Pi still held is back at the front of the next-turn queue.
    let queued = r.mapper.queued_turns();
    assert_eq!(queued.len(), 1);
    assert_eq!(dshx_pi::mapper::prompt_text(&queued[0]), "queued steer");
    let result = &r.of("tool/result")[0].data;
    assert_eq!(result["message"]["isError"], true);

    let s = Replay::run("abort-stream");
    assert_eq!(s.end_reason()["kind"], "aborted");
    assert_eq!(s.of("assistant/attempt").len(), 1);
}

#[test]
fn provider_errors_end_the_turn() {
    let r = Replay::run("error");
    let reason = r.end_reason();
    assert_eq!(reason["kind"], "error");
    assert_eq!(reason["error"]["message"], "faux provider failure");
}

#[test]
fn compaction_folds_the_older_surface() {
    let r = Replay::run("compact");
    assert_eq!(r.of("compaction/start").len(), 1);
    let summary = &r.of("compaction/summary")[0].data;
    assert!(
        summary["summary"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Echo:")
    );
    assert_eq!(
        summary["compactionId"],
        r.of("compaction/end")[0].data["compactionId"]
    );
    assert!(r.of("compaction/end")[0].data.get("error").is_none());
    assert!(
        r.of("user/message")
            .iter()
            .any(|e| e.data["source"]["kind"] == "compact-checkpoint")
    );
    // The latest prompt and its answer stay; the checkpoint replaces the rest.
    let messages = dshx_session::derive_messages(&r.events);
    assert_eq!(
        messages[0].source,
        dshx_core::MessageSource::Other(
            json!({"kind": "compact-checkpoint", "compactionId": summary["compactionId"]})
        )
    );
    assert_eq!(messages.len(), 3);
}

#[test]
fn a_reopened_chat_continues_its_queue_and_numbering() {
    let r = Replay::run("abort");
    let resumed = Mapper::resume(CWD, &r.events);
    assert_eq!(resumed.next_seq(), r.events.len() as u64);
    assert_eq!(resumed.queued_turns().len(), 1);
    assert_eq!(resumed.cursor(), r.mapper.cursor());
}

#[test]
fn retries_stay_in_their_step() {
    let mut r = Replay::new();
    r.line(json!({"_sent": {"type": "prompt", "message": "hi"}}));
    let attempt = |n: u64| {
        vec![
            json!({"type": "turn_start"}),
            json!({"type": "message_start", "message": {"role": "assistant", "content": [], "provider": "pi-faux", "model": "faux-1", "stopReason": "pending"}}),
            json!({"type": "message_end", "message": {"role": "assistant", "content": [], "provider": "pi-faux", "model": "faux-1", "stopReason": "error", "errorMessage": "529 overloaded"}}),
            json!({"type": "turn_end"}),
            json!({"type": "agent_end", "willRetry": true}),
            json!({"type": "auto_retry_start", "attempt": n, "maxAttempts": 3, "delayMs": 10, "errorMessage": "529 overloaded"}),
        ]
    };
    r.line(json!({"type": "agent_start"}));
    r.line(json!({"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}}));
    for n in 1..=2 {
        for rec in attempt(n) {
            r.line(rec);
        }
    }
    for rec in [
        json!({"type": "auto_retry_end", "success": true, "attempt": 3}),
        json!({"type": "turn_start"}),
        json!({"type": "message_start", "message": {"role": "assistant", "content": [], "provider": "pi-faux", "model": "faux-1"}}),
        json!({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}], "provider": "pi-faux", "model": "faux-1", "stopReason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": {"total": 0.25}}}}),
        json!({"type": "turn_end"}),
        json!({"type": "agent_settled", "aborted": false}),
    ] {
        r.line(rec);
    }
    dshx_session::validate::validate_log(&header(), &r.events).unwrap();
    let retries = r.of("llm/retry");
    assert_eq!(retries.len(), 2);
    assert_eq!(retries[1].data["retry"], 2);
    assert_eq!(r.of("step/start").len(), 1);
    assert_eq!(r.end_reason()["kind"], "completed");
    assert_eq!(r.of("assistant/message")[0].data["usage"]["costUsd"], 0.25);
}

#[test]
fn a_crash_closes_open_calls() {
    let mut r = Replay::new();
    r.line(json!({"_sent": {"type": "prompt", "message": "bash: sleep 9"}}));
    for rec in [
        json!({"type": "turn_start"}),
        json!({"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "bash: sleep 9"}]}}),
        json!({"type": "message_start", "message": {"role": "assistant", "content": [], "provider": "pi-faux", "model": "faux-1"}}),
        json!({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "sleep 9"}}, {"type": "toolCall", "id": "c2", "name": "read", "arguments": {"path": "a"}}], "provider": "pi-faux", "model": "faux-1", "stopReason": "toolUse"}}),
        json!({"type": "tool_execution_start", "toolCallId": "c1", "toolName": "bash", "args": {"command": "sleep 9"}}),
    ] {
        r.line(rec);
    }
    let events = r.mapper.close_turn(dshx_pi::TurnEnd::Error {
        code: "ENGINE_EXITED".into(),
        message: "the Pi engine exited".into(),
    });
    r.push(events);
    dshx_session::validate::validate_log(&header(), &r.events).unwrap();
    assert_eq!(r.of("tool/result").len(), 2);
    assert_eq!(r.end_reason()["error"]["code"], "ENGINE_EXITED");
}
