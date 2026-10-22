//! Shared helpers: dsh-shaped scripted events and an offline official-dsh harness.
#![allow(dead_code)]

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use dshx_core::NewEvent;
use serde_json::{Value, json};

const MODEL: &str = "deepseek-flash";
const PROVIDER: &str = "deepseek-official";

fn surface(kind: &str, data: Value) -> NewEvent {
    NewEvent::new(kind, data).with("surfaceOp", json!("append"))
}

fn replay(block: &str) -> Value {
    json!({"response": {"kind": "deepseek-messages", "version": 1, "model": MODEL}, "blocks": [{"type": block}]})
}

fn usage(output: u64) -> Value {
    json!({"inputTokens": 100, "outputTokens": output, "totalTokens": 100 + output})
}

/// `assistant/message` exactly as dsh 0.2.0 logs it, `stream` included.
pub fn assistant_message(turn: u64, step: u64, id: &str, block: Value) -> NewEvent {
    let t = 1_792_000_000_000u64;
    let kind = block["type"].as_str().unwrap().to_owned();
    let middle = if kind == "tool-call" {
        vec![
            json!({"type": "tool-call-chunks", "time0": t, "index": 0, "dt": [], "id": block["id"], "name": block["name"], "args": [""]}),
            json!({"type": "tool-call-chunks", "time0": t, "index": 0, "dt": [], "id": block["id"], "args": [block["arguments"]]}),
        ]
    } else {
        vec![
            json!({"type": "text-chunks", "time0": t, "index": 0, "dt": [], "texts": [block["text"]]}),
        ]
    };
    let finish = if kind == "tool-call" {
        "tool-calls"
    } else {
        "stop"
    };
    let mut stream = vec![
        json!({"type": "chunk", "time": t, "chunk": {"type": "block-start", "index": 0, "blockType": kind}}),
    ];
    stream.extend(middle);
    stream.push(json!({"type": "chunk", "time": t, "chunk": {"type": "block-end", "index": 0, "block": block}}));
    stream
        .push(json!({"type": "chunk", "time": t, "chunk": {"type": "usage", "usage": usage(20)}}));
    stream.push(json!({"type": "chunk", "time": t, "chunk": {"type": "finish", "reason": {"kind": finish}, "replayState": replay(&kind)}}));
    surface(
        "assistant/message",
        json!({
            "turn": turn, "step": step,
            "message": {"role": "assistant", "content": [block], "source": {"kind": "model", "provider": PROVIDER, "model": MODEL, "replayState": replay(&kind)}, "id": id},
            "usage": usage(20),
            "stream": stream,
        }),
    )
}

pub fn bash_tool_schema() -> Value {
    json!({
        "name": "bash",
        "description": "Execute a bash command (`bash -c`) and return its stdout/stderr.",
        "parameters": {"type": "object", "properties": {"command": {"type": "string"}, "description": {"type": "string"}}, "required": ["command", "description"]}
    })
}

/// One complete dsh-shaped turn: prompt, a bash call with its result, a final answer.
/// `first_seq` is the seq the first returned event will get.
pub fn scripted_turn(
    cwd: &str,
    prompt: &str,
    call_id: &str,
    command: &str,
    output: &str,
    answer: &str,
) -> Vec<NewEvent> {
    let user = json!({"content": [{"type": "text", "text": prompt}], "source": {"kind": "user"}, "role": "user", "id": "user-msg-1"});
    let arguments =
        json!({"command": command, "description": format!("run {command}")}).to_string();
    let call = json!({"type": "tool-call", "id": call_id, "name": "bash", "arguments": arguments});
    vec![
        NewEvent::new("permission/preset", json!({"preset": "workspace-write"})),
        NewEvent::new("sandbox/mode", json!({"mode": "workspace-write"})),
        NewEvent::new("approval/policy", json!({"policy": "ask"})),
        NewEvent::new("agent/inbox/spliced", json!({"target": "next-turn", "start": 0, "inserted": [user]})),
        NewEvent::new("turn/start", json!({"turn": 1})),
        NewEvent::new("agent/inbox/spliced", json!({"target": "next-turn", "start": 0, "removedCount": 1, "inserted": []})),
        NewEvent::new("step/start", json!({"turn": 1, "step": 1})),
        surface(
            "system/message",
            json!({"turn": 1, "step": 1, "message": {"role": "system", "content": [{"type": "text", "text": format!("You are a coding agent.\n\nYour working directory is {cwd}.")}], "source": {"kind": "system-prompt"}, "id": "system-msg-1"}}),
        ),
        surface("user/message", user),
        NewEvent::new(
            "request/header",
            json!({"header": {"config": {"provider": PROVIDER, "model": MODEL, "maxTokens": 256000, "reasoningEffort": "high"}, "adapterDefaults": {"reasoningEffort": true, "maxTokens": true}, "tools": [bash_tool_schema()]}, "reason": "initial"}),
        ),
        NewEvent::new("request/context", json!({"provider": PROVIDER, "model": MODEL, "contextWindow": 1_000_000, "systemPromptUpdate": "in-history"})),
        NewEvent::new("session/title", json!({"title": prompt, "messageSeqs": [8], "source": {"kind": "fallback"}})),
        assistant_message(1, 1, "assistant-msg-1", call),
        NewEvent::new("tool/call", json!({"turn": 1, "step": 1, "callId": call_id, "name": "bash", "arguments": arguments})),
        surface(
            "tool/result",
            json!({"turn": 1, "step": 1, "message": {"role": "tool", "source": {"kind": "tool", "callId": call_id}, "toolCallId": call_id, "content": [{"type": "text", "text": output}], "isError": false, "id": "tool-msg-1"}}),
        )
        .with("sourceEventSeqs", json!([13])),
        NewEvent::new("step/end", json!({"turn": 1, "step": 1})),
        NewEvent::new("step/start", json!({"turn": 1, "step": 2})),
        assistant_message(1, 2, "assistant-msg-2", json!({"type": "text", "text": answer})),
        NewEvent::new("step/end", json!({"turn": 1, "step": 2})),
        NewEvent::new("turn/end", json!({"turn": 1, "reason": {"kind": "completed"}})),
    ]
}

/// Whether the official dsh and python3 are available (interop tests skip otherwise).
pub fn dsh_available() -> bool {
    if std::env::var_os("DSHX_SKIP_DSH_INTEROP").is_some() {
        return false;
    }
    let ok = |cmd: &str, arg: &str| {
        Command::new(cmd)
            .arg(arg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    ok("dsh", "--version") && ok("python3", "--version")
}

/// The offline mock DeepSeek (`tools/dev/mock_deepseek.py`) on a free port.
pub struct Mock {
    child: Child,
    pub port: u16,
    pub dump: PathBuf,
}

impl Mock {
    pub fn start(dir: &Path, bash_calls: u32) -> Mock {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/dev/mock_deepseek.py");
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dump = dir.join(format!("dump-{port}"));
        std::fs::create_dir_all(&dump).unwrap();
        let child = Command::new("python3")
            .arg(&script)
            .arg(port.to_string())
            .arg(bash_calls.to_string())
            .arg("/dev/null")
            .arg(&dump)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start mock_deepseek.py");
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "mock did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Mock { child, port, dump }
    }

    /// Every request body the mock received, in order.
    pub fn requests(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dump)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        files
            .iter()
            .map(|f| serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap())
            .collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs the official dsh headless with an isolated `DSH_HOME` (never `~/.dsh`)
/// and the fake key `sk-mock` against the mock. `DSH_AGENTS_HOME` points
/// inside it too, so the user's personal skills (`~/.agents/skills`) stay out
/// of the session; `HOME` is left alone.
pub fn run_dsh(dsh_home: &Path, cwd: &Path, port: u16, args: &[&str]) -> Output {
    let child = Command::new("dsh")
        .arg("--profile")
        .arg("headless")
        .args(args)
        .current_dir(cwd)
        .env("DSH_HOME", dsh_home)
        .env("DSH_AGENTS_HOME", dsh_home.join("agents-home"))
        .env_remove("DSH_BUNDLED_SKILL_DIR")
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env(
            "DEEPSEEK_BASE_URL",
            format!("http://127.0.0.1:{port}/anthropic"),
        )
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dsh");
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(180)) {
        Ok(output) => output.expect("dsh output"),
        Err(_) => {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            panic!("dsh timed out");
        }
    }
}

pub fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
