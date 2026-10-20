//! Golden comparison against sessions written by the official dsh 0.2.0-rc.2.
//! The fixtures were produced offline with a fake key:
//! `python3 tests/fixtures/gen/mock_scripted.py PORT gen/<scenario>.json DUMP &` and
//! `DEEPSEEK_API_KEY=sk-mock DEEPSEEK_BASE_URL=http://127.0.0.1:PORT/anthropic dsh --profile headless "<prompt>"`
//! in an 88-byte scratch cwd (`dsh-bash-two-steps` used `tools/dev/mock_deepseek.py PORT 2`;
//! the overflow fixture continues with `--session-id`), then `zstdcat` with the cwd
//! replaced by `@@CWD@@`. The fixture's model streams become our model script and
//! its tool results our tool outputs; the agent must then log the same events with
//! the same payloads (ids, times and stream timings masked, telemetry dropped,
//! parallel tool interleaving canonicalized).
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::*;
use dshx_agent::testing::{FakeTool, Response, ScriptedLlm};
use dshx_core::{
    ContentBlock, FinishReason, SessionEvent, SessionLog, StreamChunk, TokenUsage, Tool,
    ToolOutput, ToolResultPart,
};
use serde_json::{Map, Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const CONCURRENCY_SAFE: [&str; 3] = ["read", "glob", "grep"];

fn load(name: &str, cwd: &str) -> Vec<SessionEvent> {
    let text = std::fs::read_to_string(format!("{FIXTURES}/{name}"))
        .unwrap()
        .replace("@@CWD@@", cwd);
    text.lines()
        .skip(1)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn block_type(s: &str) -> &'static str {
    match s {
        "text" => "text",
        "reasoning" => "reasoning",
        "tool-call" => "tool-call",
        other => Box::leak(other.to_owned().into_boxed_str()),
    }
}

/// dsh chunk JSON → our chunk.
fn chunk(v: &Value) -> StreamChunk {
    let index = v["index"].as_u64().unwrap_or(0) as usize;
    match v["type"].as_str().unwrap() {
        "block-start" => StreamChunk::BlockStart {
            index,
            block_type: block_type(v["blockType"].as_str().unwrap()),
        },
        "block-end" => StreamChunk::BlockEnd {
            index,
            block: serde_json::from_value(v["block"].clone()).unwrap(),
        },
        "usage" => {
            StreamChunk::Usage(serde_json::from_value::<TokenUsage>(v["usage"].clone()).unwrap())
        }
        "finish" => StreamChunk::Finish {
            reason: serde_json::from_value::<FinishReason>(v["reason"].clone()).unwrap(),
            replay_state: v.get("replayState").cloned(),
        },
        "text-delta" => StreamChunk::TextDelta {
            index,
            text: v["text"].as_str().unwrap().into(),
        },
        "reasoning-delta" => StreamChunk::ReasoningDelta {
            index,
            text: v["text"].as_str().unwrap().into(),
        },
        "tool-call-delta" => StreamChunk::ToolCallDelta {
            index,
            id: v["id"].as_str().unwrap().into(),
            name: v.get("name").and_then(Value::as_str).map(str::to_owned),
            arguments_delta: v["argumentsDelta"].as_str().unwrap().into(),
        },
        other => panic!("unknown chunk {other}"),
    }
}

/// Expands dsh's compact stream records into the original chunk sequence.
fn expand(stream: &Value) -> Vec<StreamChunk> {
    let mut out = Vec::new();
    for r in stream.as_array().unwrap() {
        let index = r["index"].as_u64().unwrap_or(0) as usize;
        match r["type"].as_str().unwrap() {
            "chunk" => out.push(chunk(&r["chunk"])),
            "text-chunks" => {
                for t in r["texts"].as_array().unwrap() {
                    out.push(StreamChunk::TextDelta {
                        index,
                        text: t.as_str().unwrap().into(),
                    });
                }
            }
            "reasoning-chunks" => {
                for t in r["texts"].as_array().unwrap() {
                    out.push(StreamChunk::ReasoningDelta {
                        index,
                        text: t.as_str().unwrap().into(),
                    });
                }
            }
            "tool-call-chunks" => {
                for a in r["args"].as_array().unwrap() {
                    out.push(StreamChunk::ToolCallDelta {
                        index,
                        id: r["id"].as_str().unwrap().into(),
                        name: r.get("name").and_then(Value::as_str).map(str::to_owned),
                        arguments_delta: a.as_str().unwrap().into(),
                    });
                }
            }
            other => panic!("unknown record {other}"),
        }
    }
    out
}

/// One model response per settled attempt, in log order.
fn script(events: &[SessionEvent]) -> Vec<Response> {
    events
        .iter()
        .filter(|e| e.kind == "assistant/message" || e.kind == "assistant/attempt")
        .map(|e| Response::Chunks(expand(&e.data["stream"])))
        .collect()
}

/// Fake tools with dsh's exact schemas that replay dsh's recorded outputs.
fn tools(events: &[SessionEvent]) -> Vec<Arc<dyn Tool>> {
    let header = &events
        .iter()
        .find(|e| e.kind == "request/header")
        .unwrap()
        .data["header"];
    let mut outputs: HashMap<String, ToolOutput> = HashMap::new();
    for e in events
        .iter()
        .filter(|e| e.kind == "tool/result" && e.data.get("error").is_none())
    {
        let m = &e.data["message"];
        let content = m["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(
                |b| match serde_json::from_value::<ContentBlock>(b.clone()).unwrap() {
                    ContentBlock::Text { text } => ToolResultPart::Text { text },
                    ContentBlock::Image { attachment, .. } => ToolResultPart::Image { attachment },
                    other => panic!("unexpected tool content {other:?}"),
                },
            )
            .collect();
        outputs.insert(
            m["toolCallId"].as_str().unwrap().into(),
            ToolOutput {
                content,
                is_error: m["isError"] == true,
                value: None,
                meta: e.data.get("meta").cloned(),
            },
        );
    }
    let outputs = Arc::new(outputs);
    header["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let name = t["name"].as_str().unwrap().to_owned();
            let outputs = outputs.clone();
            let mut tool = FakeTool::new(
                &name,
                t["parameters"].clone(),
                CONCURRENCY_SAFE.contains(&name.as_str()),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(move |args: Value, ctx| {
                    let mut out = outputs.get(&ctx.call_id).cloned().unwrap_or_default();
                    if let Some(todos) = args.get("todos") {
                        out.value = Some(json!({"todos": todos}));
                    }
                    Box::pin(async move { Ok(out) })
                }),
            );
            Arc::get_mut(&mut tool).unwrap().schema.description =
                t["description"].as_str().unwrap().into();
            tool as Arc<dyn Tool>
        })
        .collect()
}

const MASKED_IDS: [&str; 3] = ["id", "retryId", "compactionId"];

fn mask(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for key in MASKED_IDS {
                if m.contains_key(key) && m.get(key).is_some_and(Value::is_string) {
                    m.insert(key.into(), "<id>".into());
                }
            }
            for key in ["time", "time0", "delayMs"] {
                if m.contains_key(key) {
                    m.insert(key.into(), 0.into());
                }
            }
            if m.contains_key("dt") {
                m.insert("dt".into(), json!([]));
            }
            for (_, child) in m.iter_mut() {
                mask(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(mask),
        _ => {}
    }
}

fn is_provider_title(e: &SessionEvent) -> bool {
    e.kind == "session/title" && e.data["source"]["kind"] == "provider"
}

/// Drops telemetry and the asynchronous provider title, canonicalizes runs of
/// tool events to (call, todo, result) per call, renumbers seqs and masks.
fn normalize(events: &[SessionEvent]) -> (Vec<Value>, Option<Value>) {
    let kept: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| !e.kind.starts_with("session-log-deepseek/") && !is_provider_title(e))
        .collect();
    let mut ordered: Vec<&SessionEvent> = Vec::new();
    let mut i = 0;
    while i < kept.len() {
        if !matches!(
            kept[i].kind.as_str(),
            "tool/call" | "tool/result" | "todo/write"
        ) {
            ordered.push(kept[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < kept.len()
            && matches!(
                kept[j].kind.as_str(),
                "tool/call" | "tool/result" | "todo/write"
            )
        {
            j += 1;
        }
        let run = &kept[i..j];
        let mut todos: Vec<&SessionEvent> = Vec::new();
        let mut by_call: HashMap<String, Vec<&SessionEvent>> = HashMap::new();
        let mut calls: Vec<String> = Vec::new();
        for e in run {
            match e.kind.as_str() {
                "tool/call" => {
                    let id = e.data["callId"].as_str().unwrap().to_owned();
                    calls.push(id.clone());
                    by_call.entry(id).or_default().push(e);
                }
                "todo/write" => todos.push(e),
                _ => {
                    let id = e.data["message"]["toolCallId"].as_str().unwrap().to_owned();
                    let group = by_call.entry(id).or_default();
                    group.append(&mut todos);
                    group.push(e);
                }
            }
        }
        for id in calls {
            ordered.extend(by_call.remove(&id).unwrap());
        }
        i = j;
    }
    let index: HashMap<u64, u64> = ordered
        .iter()
        .enumerate()
        .map(|(n, e)| (e.seq, n as u64))
        .collect();
    let remap = |v: &Value| -> Value {
        v.as_u64()
            .and_then(|s| index.get(&s))
            .map(|n| json!(n))
            .unwrap_or(json!("<gone>"))
    };
    let render = |e: &SessionEvent| -> Value {
        let mut v = serde_json::to_value(e).unwrap();
        let m = v.as_object_mut().unwrap();
        m.insert("seq".into(), json!(index.get(&e.seq).copied()));
        if let Some(Value::Array(s)) = m.get("sourceEventSeqs").cloned() {
            m.insert(
                "sourceEventSeqs".into(),
                Value::Array(s.iter().map(remap).collect()),
            );
        }
        if let Some(Value::Object(op)) = m.get("surfaceOp").cloned() {
            let mut op: Map<String, Value> = op;
            for k in ["startSeq", "endSeq"] {
                let r = remap(&op[k]);
                op.insert(k.into(), r);
            }
            m.insert("surfaceOp".into(), Value::Object(op));
        }
        let data = m.get_mut("data").unwrap();
        if let Some(Value::Array(s)) = data.get("messageSeqs").cloned() {
            data["messageSeqs"] = Value::Array(s.iter().map(remap).collect());
        }
        if let Some(h) = data.get("headerSeq").cloned() {
            data["headerSeq"] = remap(&h);
        }
        if let Some(Value::Array(s)) = data.get("shadowedSeqs").cloned() {
            data["shadowedSeqs"] = Value::Array(s.iter().map(remap).collect());
        }
        if let Some(r) = data.get("shadowedRange").cloned() {
            data["shadowedRange"] = json!({"start": remap(&r["start"]), "end": remap(&r["end"])});
        }
        mask(&mut v);
        v
    };
    let title = events.iter().find(|e| is_provider_title(e)).map(render);
    (ordered.into_iter().map(render).collect(), title)
}

fn compare(ours: &[SessionEvent], theirs: &[SessionEvent]) {
    let (a, at) = normalize(ours);
    let (b, bt) = normalize(theirs);
    for (n, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(
            x,
            y,
            "event {n} differs\n ours: {}\n dsh:  {}",
            serde_json::to_string(x).unwrap(),
            serde_json::to_string(y).unwrap()
        );
    }
    assert_eq!(
        a.len(),
        b.len(),
        "event count differs: ours {:?}",
        a.iter().map(|e| e["type"].clone()).collect::<Vec<_>>()
    );
    let strip =
        |t: Option<Value>| t.map(|mut v| v.as_object_mut().unwrap().remove("data").unwrap());
    assert_eq!(strip(at), strip(bt), "provider title differs");
}

/// The reference sessions ran in an 88-byte cwd; ours has the same length so
/// path-bearing token estimates (compaction prices) agree exactly.
const REFERENCE_CWD_LEN: usize = 88;

/// Replays every prompt of a fixture through our agent. A `session/end-seed`
/// in the fixture marks a process restart: the next prompt runs on a fresh
/// agent over the stored log plus that marker (what `SessionStore::resume` does).
async fn replay(name: &str, setup: impl FnOnce(&std::path::Path)) {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let base = dir.path().to_str().unwrap().len();
    let cwd = dir.path().join("w".repeat(REFERENCE_CWD_LEN - base - 1));
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(cwd.to_str().unwrap().len(), REFERENCE_CWD_LEN);
    setup(&cwd);
    let reference = load(name, cwd.to_str().unwrap());
    let llm = ScriptedLlm::new(script(&reference));
    *llm.summary_response.lock().unwrap() = Some(Response::Chunks(summary_chunks()));
    let tools = tools(&reference);
    let mut log = Arc::new(dshx_session::MemoryLog::new(cwd.to_str().unwrap()));
    let mut prompts = Vec::new();
    for e in &reference {
        if e.kind == "user/message" && e.data["source"]["kind"] == "user" {
            prompts.push((
                e.data["content"][0]["text"].as_str().unwrap().to_owned(),
                false,
            ));
        }
        if e.kind == "session/end-seed" {
            prompts.push((String::new(), true));
        }
    }
    let mut first = true;
    for (prompt, restart) in prompts {
        if restart {
            let mut events = log.events();
            let last = events.last().unwrap();
            events.push(SessionEvent {
                kind: "session/end-seed".into(),
                seq: last.seq + 1,
                time: last.time,
                data: json!({}),
                extra: Default::default(),
            });
            log = Arc::new(dshx_session::MemoryLog::with_events(log.header(), events));
            continue;
        }
        let h = with_log(
            llm.clone(),
            tools.clone(),
            dir.clone(),
            cwd.clone(),
            log.clone(),
            |c, _| {
                c.retry.random = Arc::new(|| 0.5);
            },
        );
        h.agent
            .run_turn(vec![ContentBlock::text(prompt)])
            .await
            .unwrap();
        if first {
            wait_title(&log).await;
            first = false;
        }
    }
    validate(&log);
    compare(&log.events(), &reference);
}

/// The mock's summarization answer.
fn summary_chunks() -> Vec<StreamChunk> {
    let mut c = dshx_agent::testing::chunks(
        "deepseek-flash",
        &[dshx_agent::testing::Block::Text(
            "## Primary Request and Intent\n- summary".into(),
        )],
        FinishReason::Stop,
    );
    for chunk in &mut c {
        if let StreamChunk::Usage(u) = chunk {
            *u = TokenUsage {
                input_tokens: 100,
                output_tokens: 20,
                total_tokens: Some(120),
                ..Default::default()
            };
        }
    }
    c
}

#[tokio::test]
async fn matches_dsh_two_bash_steps() {
    replay("dsh-bash-two-steps.jsonl", |_| {}).await;
}

#[tokio::test]
async fn matches_dsh_parallel_tools_errors_todos_and_instructions() {
    replay("dsh-tools-instructions.jsonl", |cwd| {
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "# Project rules\nUse tabs.\n").unwrap();
        std::fs::write(cwd.join("CLAUDE.md"), "# Project rules\nUse tabs.\n").unwrap();
        std::fs::write(cwd.join("AGENTS.local.md"), "local overlay\n").unwrap();
        std::fs::write(cwd.join("sub/AGENTS.md"), "Sub rules here.\n").unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn matches_dsh_retries() {
    replay("dsh-retry.jsonl", |_| {}).await;
}

#[tokio::test]
async fn matches_dsh_resume_and_overflow_compaction() {
    replay("dsh-overflow-compaction.jsonl", |_| {}).await;
}
