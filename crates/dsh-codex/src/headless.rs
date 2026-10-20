//! One task from the command line, like `dsh --profile headless`: reasoning
//! deltas on stderr, the final answer on stdout, exit code 0 only when the turn
//! completed. `--json` replaces stdout with dsh's NDJSON projection.
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dshx_agent::{AgentObserver, DeltaKind, StreamDelta, TurnEndReason};
use dshx_core::{ContentBlock, Message, SessionEvent, SessionLog, kinds};
use serde_json::{Map, Value, json};

use crate::runtime::{NoApprover, Overrides, Runtime};

pub struct HeadlessArgs {
    pub task: String,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub json: bool,
    pub overrides: Overrides,
}

const MAX_STRING_BYTES: usize = 8 * 1024;
const MAX_EVENT_BYTES: usize = 32 * 1024;

/// Returns the process exit code.
pub async fn run(runtime: Arc<Runtime>, args: HeadlessArgs) -> anyhow::Result<i32> {
    let cwd = args.cwd.canonicalize().unwrap_or(args.cwd.clone());
    let cwd_text = cwd.display().to_string();
    let log: Arc<dyn SessionLog> = match &args.session_id {
        Some(id) => {
            if runtime.store.locate(id)?.is_none() {
                anyhow::bail!(
                    "session \"{id}\" does not exist; omit --session-id to start a new Session"
                );
            }
            let log = runtime.store.resume(id).await?;
            let header = log.header();
            if header.cwd != cwd_text {
                anyhow::bail!("session \"{id}\" belongs to {}, not {cwd_text}", header.cwd);
            }
            if header.extra.contains_key("parentSession") || header.is_seeded {
                anyhow::bail!("session \"{id}\" is a subagent or forked session");
            }
            Arc::new(log)
        }
        None => Arc::new(runtime.store.create(&cwd_text)?),
    };

    let reasoning = (!args.json).then(|| Arc::new(ReasoningToStderr::default()));
    let observer = reasoning.clone().map(|r| r as Arc<dyn AgentObserver>);
    let config = runtime.agent_config(&cwd, &args.overrides);
    let agent = runtime.new_agent(config, log.clone(), Arc::new(NoApprover), observer);

    let mut events = log.subscribe();
    let projector = args.json.then(|| {
        emit(&json!({ "type": "session", "sessionId": log.id(), "cwd": cwd_text }));
        Arc::new(Mutex::new(JsonProjector::default()))
    });
    let pump = projector.clone().map(|p| {
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                p.lock().unwrap().on_event(&event);
            }
        })
    });

    let first_turn_seq = log.events().last().map_or(0, |e| e.seq + 1);
    let outcome = agent.run_turn(vec![ContentBlock::text(args.task)]).await;
    runtime.close_agent(&agent).await;
    if let Some(r) = &reasoning {
        r.close_section();
    }
    if let Some(pump) = pump {
        // Give the feed a moment to deliver the turn's tail, then stop it.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        pump.abort();
    }
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            if args.json {
                emit(&json!({ "type": "error", "message": format!("{e:#}") }));
            }
            return Err(e);
        }
    };
    let answer = final_answer(&log.events(), first_turn_seq);
    if args.json {
        // Events appended after the pump stopped: project them from the log.
        if let Some(p) = &projector {
            let mut p = p.lock().unwrap();
            // `on_event` skips what the pump already projected.
            for event in log.events().iter().filter(|e| e.seq >= first_turn_seq) {
                p.on_event(event);
            }
        }
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", json!({ "type": "final", "text": answer }));
    } else if !answer.is_empty() {
        println!("{answer}");
    }
    runtime.shutdown().await;
    match outcome.reason {
        TurnEndReason::Completed => Ok(0),
        TurnEndReason::Error { error } => {
            eprintln!("dsh-codex: {}: {}", error.code, error.message);
            Ok(1)
        }
        other => {
            let reason = serde_json::to_value(&other)
                .ok()
                .and_then(|v| v["kind"].as_str().map(str::to_owned));
            eprintln!(
                "dsh-codex: turn ended: {}",
                reason.unwrap_or_else(|| "unknown".into())
            );
            Ok(1)
        }
    }
}

/// Text of the last assistant message of the run.
fn final_answer(events: &[SessionEvent], from_seq: u64) -> String {
    events
        .iter()
        .rev()
        .filter(|e| e.seq >= from_seq && e.kind == kinds::ASSISTANT_MESSAGE)
        .find_map(|e| serde_json::from_value::<Message>(e.data["message"].clone()).ok())
        .map(|m| m.plain_text())
        .unwrap_or_default()
}

fn emit(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", bound_line(value));
    let _ = out.flush();
}

/// dsh `projectJsonRun`: committed messages, tool calls/results and status.
#[derive(Default)]
struct JsonProjector {
    usage: Option<Map<String, Value>>,
    usage_complete: bool,
    last_seq: Option<u64>,
}

impl JsonProjector {
    fn on_event(&mut self, event: &SessionEvent) {
        if self.last_seq.is_some_and(|s| event.seq <= s) {
            return;
        }
        self.last_seq = Some(event.seq);
        let d = &event.data;
        match event.kind.as_str() {
            kinds::TURN_START => {
                emit(&json!({ "type": "status", "phase": "turn_start", "turn": d["turn"] }))
            }
            kinds::STEP_START => {
                self.usage = None;
                self.usage_complete = true;
                emit(
                    &json!({ "type": "status", "phase": "step_start", "turn": d["turn"], "step": d["step"] }),
                )
            }
            "assistant/attempt" => self.add_usage(stream_usage(&d["stream"])),
            kinds::ASSISTANT_MESSAGE => {
                let usage = d
                    .get("usage")
                    .filter(|u| u.is_object())
                    .cloned()
                    .or_else(|| stream_usage(&d["stream"]));
                self.add_usage(usage);
                for block in d["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("reasoning") => {
                            emit(&json!({ "type": "thinking", "text": block["text"] }))
                        }
                        Some("text") => emit(&json!({ "type": "text", "text": block["text"] })),
                        _ => {}
                    }
                }
            }
            kinds::STEP_END => {
                let mut v = json!({ "type": "status", "phase": "step_end", "turn": d["turn"], "step": d["step"] });
                if self.usage_complete
                    && let Some(u) = self.usage.take()
                {
                    v["usage"] = Value::Object(u);
                }
                self.usage = None;
                self.usage_complete = true;
                emit(&v)
            }
            kinds::TURN_END => emit(
                &json!({ "type": "status", "phase": "turn_end", "turn": d["turn"], "reason": d["reason"] }),
            ),
            kinds::TOOL_CALL => {
                let raw = d["arguments"].as_str().unwrap_or("");
                let input = if raw.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str::<Value>(raw)
                        .unwrap_or_else(|_| Value::String(raw.to_owned()))
                };
                emit(
                    &json!({ "type": "tool_call", "callId": d["callId"], "tool": d["name"], "input": input }),
                )
            }
            kinds::TOOL_RESULT if event.extra.get("surfaceOp") == Some(&json!("append")) => {
                let m = &d["message"];
                let text: String = m["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                let status = if m["isError"] == true {
                    "error"
                } else {
                    "completed"
                };
                emit(
                    &json!({ "type": "tool_result", "callId": m["toolCallId"], "status": status, "result": text }),
                )
            }
            _ => {}
        }
    }

    fn add_usage(&mut self, next: Option<Value>) {
        let Some(Value::Object(next)) = next else {
            self.usage_complete = false;
            return;
        };
        let Some(total) = &mut self.usage else {
            self.usage = Some(next);
            return;
        };
        for key in ["inputTokens", "outputTokens"] {
            let sum = total.get(key).and_then(Value::as_u64).unwrap_or(0)
                + next.get(key).and_then(Value::as_u64).unwrap_or(0);
            total.insert(key.into(), json!(sum));
        }
        // Optional counters only add up when both sides have them (dsh `addUsage`).
        for key in [
            "totalTokens",
            "cacheReadTokens",
            "cacheWriteTokens",
            "reasoningTokens",
        ] {
            match (
                total.get(key).and_then(Value::as_u64),
                next.get(key).and_then(Value::as_u64),
            ) {
                (Some(a), Some(b)) => {
                    total.insert(key.into(), json!(a + b));
                }
                _ => {
                    total.remove(key);
                }
            }
        }
    }
}

/// The last `usage` chunk of a persisted assistant stream.
fn stream_usage(stream: &Value) -> Option<Value> {
    stream
        .as_array()?
        .iter()
        .rev()
        .find(|c| c["type"] == "usage")
        .map(|c| c["usage"].clone())
}

/// dsh `boundJsonLine`: strings capped at 8 KiB, a line at 32 KiB.
fn bound_line(value: &Value) -> String {
    let mut truncated = false;
    let mut bounded = bound(value, &mut truncated, 0);
    if truncated && let Value::Object(map) = &mut bounded {
        map.insert("truncated".into(), json!(true));
    }
    let line = bounded.to_string();
    if line.len() < MAX_EVENT_BYTES {
        return line;
    }
    let mut scalars: Map<String, Value> = bounded
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, v)| !v.is_object() && !v.is_array())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    scalars.insert("truncated".into(), json!(true));
    let short = Value::Object(scalars).to_string();
    if short.len() < MAX_EVENT_BYTES {
        return short;
    }
    json!({ "type": bounded["type"], "truncated": true }).to_string()
}

fn bound(value: &Value, truncated: &mut bool, depth: usize) -> Value {
    match value {
        Value::String(s) if s.len() > MAX_STRING_BYTES => {
            *truncated = true;
            let mut end = MAX_STRING_BYTES;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(s[..end].to_owned())
        }
        Value::Array(_) | Value::Object(_) if depth >= 64 => {
            *truncated = true;
            json!("[truncated: depth]")
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| bound(v, truncated, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), bound(v, truncated, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Default mode: reasoning deltas go to stderr under one `dsh-codex: reasoning:` heading.
#[derive(Default)]
struct ReasoningToStderr {
    open: Mutex<Option<bool>>, // Some(ends_with_newline) while a section is open
}

impl ReasoningToStderr {
    fn close_section(&self) {
        let mut open = self.open.lock().unwrap();
        if let Some(false) = *open {
            eprintln!();
        }
        *open = None;
    }
}

impl AgentObserver for ReasoningToStderr {
    fn on_delta(&self, delta: &StreamDelta) {
        if delta.text.is_empty() {
            return;
        }
        let mut open = self.open.lock().unwrap();
        let mut err = std::io::stderr().lock();
        match delta.block_type {
            DeltaKind::Reasoning => {
                if open.is_none() {
                    let _ = writeln!(err, "dsh-codex: reasoning:");
                }
                let _ = write!(err, "{}", delta.text);
                *open = Some(delta.text.ends_with('\n'));
            }
            DeltaKind::Text => {
                if let Some(false) = *open {
                    let _ = writeln!(err);
                }
                *open = None;
            }
        }
        let _ = err.flush();
    }
}
