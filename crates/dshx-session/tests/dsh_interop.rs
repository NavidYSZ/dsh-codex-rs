//! Interop with the installed official dsh (0.2.0-rc.2), fully offline: dsh
//! runs headless with an isolated temporary `DSH_HOME`, the fake key
//! `sk-mock` and `tools/dev/mock_deepseek.py` as the model. Skipped when
//! `dsh` or `python3` is missing (or `DSHX_SKIP_DSH_INTEROP` is set).
mod common;

use common::{Mock, dsh_available, run_dsh, text};
use dshx_core::{ContentBlock, Role, SessionLog};
use dshx_session::format::LOG_FILENAME;
use dshx_session::reader::read_log_file;
use dshx_session::validate::validate_log;
use dshx_session::{SessionError, SessionStore, derive_messages};

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("dsh-home")).unwrap();
        std::fs::create_dir_all(tmp.path().join("work")).unwrap();
        Env { tmp }
    }
    fn home(&self) -> std::path::PathBuf {
        self.tmp.path().join("dsh-home")
    }
    fn work(&self) -> std::path::PathBuf {
        // dsh records the canonical cwd.
        std::fs::canonicalize(self.tmp.path().join("work")).unwrap()
    }
    fn store(&self) -> SessionStore {
        SessionStore::new(self.home().join("sessions"))
    }
}

/// (a) A session written by the official dsh is read, validated and derived by us.
#[test]
fn reads_and_derives_a_session_written_by_dsh() {
    if !dsh_available() {
        eprintln!("skipping: official dsh not available");
        return;
    }
    let env = Env::new();
    let mock = Mock::start(env.tmp.path(), 2);
    let out = run_dsh(&env.home(), &env.work(), mock.port, &["run two commands"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "dsh failed: {stderr}");
    assert_eq!(stdout.trim(), "FERTIG");

    let store = env.store();
    let list = store.list().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    let summary = &list[0];
    assert_eq!(summary.cwd, env.work().to_string_lossy());
    assert_eq!(summary.status, "completed");
    assert_eq!(summary.title.as_deref(), Some("Testtitel"));
    assert_eq!(summary.turns, 1);

    let stored = store.read(&summary.id).unwrap();
    assert_eq!(stored.path.file_name().unwrap(), LOG_FILENAME);
    validate_log(&stored.header, &stored.events).unwrap();
    let messages = derive_messages(&stored.events);
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    use Role::*;
    assert_eq!(
        roles,
        [
            System, User, User, Assistant, Tool, Assistant, Tool, Assistant
        ]
    );
    assert_eq!(messages[1].plain_text(), "run two commands");
    assert!(
        matches!(&messages[3].content[0], ContentBlock::ToolCall { name, .. } if name == "bash")
    );
    assert_eq!(messages.last().unwrap().plain_text(), "FERTIG");
    // Both tool results we derive were on the wire of dsh's final request.
    let last = mock
        .requests()
        .into_iter()
        .rfind(|r| {
            r["path"].as_str().unwrap_or("").ends_with("/messages")
                && !r["body"]["system"].to_string().contains("concise title")
        })
        .unwrap();
    let results = last["body"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "tool_result")
        .count();
    assert_eq!(results, messages.iter().filter(|m| m.role == Tool).count());

    // Our frame encoder reproduces dsh's frames byte for byte.
    let bytes = std::fs::read(&stored.path).unwrap();
    let scan = dshx_session::reader::parse_log(&bytes).unwrap();
    assert_eq!(scan.torn_truncate_to, None);
    let frames = split_frames(&bytes);
    let mut identical = 0;
    for frame in &frames {
        let plain = zstd::stream::decode_all(&frame[..]).unwrap();
        if compress_like_dsh(&plain) == *frame {
            identical += 1;
        }
    }
    assert_eq!(identical, frames.len(), "frames re-encode byte-identically");

    // dsh released its lock: we can take write ownership, and appends stay readable.
    let log = store.open(&summary.id).unwrap();
    assert_eq!(log.events(), stored.events);
}

fn split_frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Public API has no frame splitter; re-scan via zstd's frame size helper.
    let mut frames = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let size = zstd::zstd_safe::find_frame_compressed_size(rest).unwrap();
        frames.push(rest[..size].to_vec());
        rest = &rest[size..];
    }
    frames
}

fn compress_like_dsh(plain: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.write_all(plain).unwrap();
    encoder.finish().unwrap()
}

/// (b) A session written by `FileLog` is resumed by the official dsh, which
/// appends to our file; we read the result back.
#[tokio::test]
async fn official_dsh_resumes_a_session_written_by_file_log() {
    if !dsh_available() {
        eprintln!("skipping: official dsh not available");
        return;
    }
    let env = Env::new();
    let work = env.work();
    let cwd = work.to_string_lossy().into_owned();
    let store = env.store();
    let log = store.create(&cwd).unwrap();
    let id = log.id().to_owned();
    let marker = "scripted-output-4711";
    let events = common::scripted_turn(
        &cwd,
        "run date for me",
        "toolu_scripted01",
        "date",
        &format!("{marker}\n"),
        "Done.",
    );
    log.append(events[..9].to_vec()).await.unwrap();
    log.flush().await.unwrap();
    log.append(events[9..].to_vec()).await.unwrap();
    log.flush().await.unwrap();
    let ours = log.events();
    validate_log(&log.header(), &ours).unwrap();

    // While we hold the write lock, dsh must refuse to adopt the session.
    let mock = Mock::start(env.tmp.path(), 2);
    let refused = run_dsh(
        &env.home(),
        &work,
        mock.port,
        &["--session-id", &id, "continue"],
    );
    assert!(!refused.status.success(), "dsh adopted a session we own");
    assert!(
        text(&refused).1.contains("already owned"),
        "refused for the lock: {}",
        text(&refused).1
    );
    assert_eq!(
        read_log_file(log.path()).unwrap().events.len(),
        ours.len(),
        "dsh wrote nothing while refused"
    );
    log.close().await.unwrap();
    drop(log);

    let out = run_dsh(
        &env.home(),
        &work,
        mock.port,
        &["--session-id", &id, "continue"],
    );
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "dsh resume failed: {stderr}");
    assert_eq!(stdout.trim(), "FERTIG");

    // dsh sent our history (incl. our tool result) to the model.
    let requests = mock.requests();
    let first = requests
        .iter()
        .find(|r| r["path"].as_str().unwrap_or("").ends_with("/messages"))
        .unwrap();
    let wire = first["body"]["messages"].to_string();
    assert!(
        wire.contains(marker),
        "our tool result was replayed: {wire}"
    );
    assert!(wire.contains("toolu_scripted01"));
    assert!(wire.contains("run date for me"));

    // dsh appended to our file; our reader sees the whole log.
    let stored = store.read(&id).unwrap();
    assert_eq!(
        stored.events[..ours.len()],
        ours[..],
        "our events are untouched"
    );
    let appended: Vec<&str> = stored.events[ours.len()..]
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(appended.first(), Some(&"session/end-seed"));
    assert!(appended.contains(&"turn/start") && appended.contains(&"turn/end"));
    let turn2_end = stored
        .events
        .iter()
        .rfind(|e| e.kind == "turn/end")
        .unwrap();
    assert_eq!(turn2_end.data["turn"], 2);
    assert_eq!(turn2_end.data["reason"]["kind"], "completed");
    validate_log(&stored.header, &stored.events).unwrap();

    let messages = derive_messages(&stored.events);
    let texts: Vec<String> = messages.iter().map(|m| m.plain_text()).collect();
    assert!(texts.contains(&format!("{marker}\n")));
    assert!(texts.iter().any(|t| t == "continue"));
    assert_eq!(texts.last().map(String::as_str), Some("FERTIG"));
    // The mock asked dsh for one more bash call (it counts tool results).
    let tools = messages.iter().filter(|m| m.role == Role::Tool).count();
    assert_eq!(tools, 2);

    let summary = store
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!((summary.status.as_str(), summary.turns), ("completed", 2));

    // And we can continue writing after dsh.
    let log = store.open(&id).unwrap();
    assert_eq!(log.events().len(), stored.events.len());
    log.close().await.unwrap();
    assert!(matches!(store.delete(&id), Ok(())));
    assert!(matches!(store.read(&id), Err(SessionError::NotFound(_))));
}

/// (c) Crash repair agrees: the closers dsh appends when it resumes an
/// interrupted `FileLog` session equal our `interrupted_turn_closers`.
#[tokio::test]
async fn dsh_repairs_an_interrupted_file_log_session_exactly_like_us() {
    if !dsh_available() {
        eprintln!("skipping: official dsh not available");
        return;
    }
    let env = Env::new();
    let work = env.work();
    let cwd = work.to_string_lossy().into_owned();
    let store = env.store();
    let log = store.create(&cwd).unwrap();
    let id = log.id().to_owned();
    let scripted = common::scripted_turn(&cwd, "two calls", "call_a", "date", "x", "y");
    // Up to and including the request context, then an assistant message
    // advertising two calls of which only the first was dispatched.
    let mut events = scripted[..11].to_vec();
    let call = |id: &str, cmd: &str| {
        serde_json::json!({"type": "tool-call", "id": id, "name": "bash",
            "arguments": serde_json::json!({"command": cmd, "description": cmd}).to_string()})
    };
    let mut assistant = common::assistant_message(1, 1, "assistant-two", call("call_a", "date"));
    assistant.data["message"]["content"] =
        serde_json::json!([call("call_a", "date"), call("call_b", "pwd")]);
    events.push(assistant);
    events.push(dshx_core::NewEvent::new(
        "tool/call",
        serde_json::json!({"turn": 1, "step": 1, "callId": "call_a", "name": "bash",
            "arguments": serde_json::json!({"command": "date", "description": "date"}).to_string()}),
    ));
    log.append(events).await.unwrap();
    log.close().await.unwrap();
    let ours = log.events();
    drop(log);
    let expected = dshx_session::interrupted_turn_closers(&ours);
    assert_eq!(expected.len(), 4);

    let mock = Mock::start(env.tmp.path(), 2);
    let out = run_dsh(
        &env.home(),
        &work,
        mock.port,
        &["--session-id", &id, "continue"],
    );
    assert!(out.status.success(), "dsh resume failed: {}", text(&out).1);

    let stored = store.read(&id).unwrap();
    let appended = &stored.events[ours.len()..];
    assert_eq!(
        appended[..expected.len()],
        expected[..],
        "dsh's closers == ours"
    );
    assert_eq!(appended[expected.len()].kind, "session/end-seed");
    validate_log(&stored.header, &stored.events).unwrap();
    let tool_errors = derive_messages(&stored.events)
        .iter()
        .filter(|m| m.is_error == Some(true))
        .count();
    assert_eq!(tool_errors, 2);
}

/// (d) An image stored by `FileAttachments` under dsh's home is accepted by
/// the official dsh (it re-verifies digest, type and dimensions) and sent to
/// the model when it resumes a `FileLog` session that references it.
#[tokio::test]
async fn dsh_reads_images_stored_by_file_attachments() {
    use dshx_core::AttachmentStore;
    if !dsh_available() {
        eprintln!("skipping: official dsh not available");
        return;
    }
    let env = Env::new();
    let work = env.work();
    let cwd = work.to_string_lossy().into_owned();
    let mut png = std::io::Cursor::new(Vec::new());
    image::ImageBuffer::from_fn(40, 30, |x, y| {
        image::Rgb([(x * 6) as u8, (y * 8) as u8, 99])
    })
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let png = png.into_inner();
    let attachments = dshx_session::FileAttachments::new(env.home());
    let image_ref = attachments
        .save_image(dshx_core::NewImage {
            data: png.clone(),
            media_type: "image/png".into(),
            name: Some("shot.png".into()),
            width: 0,
            height: 0,
        })
        .await
        .unwrap();
    assert_eq!((image_ref.width, image_ref.height), (40, 30));

    let store = env.store();
    let log = store.create(&cwd).unwrap();
    let id = log.id().to_owned();
    let mut events = common::scripted_turn(
        &cwd,
        "what is in the picture",
        "call_i",
        "date",
        "x",
        "A gradient.",
    );
    let content = serde_json::json!([
        {"type": "text", "text": "what is in the picture"},
        {"type": "image", "attachment": image_ref},
    ]);
    events[3].data["inserted"][0]["content"] = content.clone();
    events[8].data["content"] = content;
    log.append(events).await.unwrap();
    log.close().await.unwrap();
    validate_log(&log.header(), &log.events()).unwrap();
    drop(log);

    let mock = Mock::start(env.tmp.path(), 1);
    let out = run_dsh(
        &env.home(),
        &work,
        mock.port,
        &["--session-id", &id, "continue"],
    );
    assert!(out.status.success(), "dsh resume failed: {}", text(&out).1);
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let first = mock
        .requests()
        .into_iter()
        .find(|r| r["path"].as_str().unwrap_or("").ends_with("/messages"))
        .unwrap();
    assert!(
        first["body"]["messages"].to_string().contains(&encoded),
        "dsh sent our stored image to the model"
    );
}
