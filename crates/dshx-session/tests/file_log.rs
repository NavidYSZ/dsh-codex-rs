//! `FileLog` / `SessionStore` behaviour on disk (no official dsh needed).
mod common;

use dshx_core::{NewEvent, SessionLog};
use dshx_session::format::{LOCK_FILENAME, LOG_FILENAME};
use dshx_session::reader::{parse_log, read_log_file};
use dshx_session::validate::validate_log;
use dshx_session::{SessionError, SessionStore, derive_messages, interrupted_turn_closers};
use serde_json::json;

fn cwd(tmp: &tempfile::TempDir) -> String {
    tmp.path().join("work dir").to_string_lossy().into_owned()
}

fn frame_count(bytes: &[u8]) -> usize {
    // Every frame starts with the zstd magic; count structurally via the reader's scan.
    let mut n = 0;
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4] == [0x28, 0xB5, 0x2F, 0xFD] {
            n += 1;
        }
        i += 1;
    }
    n
}

#[tokio::test]
async fn created_session_materializes_lazily_with_dsh_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path().join("sessions"));
    let cwd = cwd(&tmp);
    let log = store.create(&cwd).unwrap();
    assert!(log.id().starts_with("session-"));
    assert!(
        !tmp.path().join("sessions").exists(),
        "no footprint before the first flush"
    );

    let events = common::scripted_turn(&cwd, "run date", "call_1", "date", "Mon Oct 20\n", "done");
    let stored = log.append(events[..5].to_vec()).await.unwrap();
    assert_eq!(
        stored.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    log.flush().await.unwrap();
    log.append(events[5..].to_vec()).await.unwrap();
    log.flush().await.unwrap();
    log.flush().await.unwrap(); // idempotent: no empty frame

    let project = tmp.path().join("sessions").join(format!(
        "--{}--",
        cwd.trim_start_matches('/')
            .replace('/', "-")
            .replace(' ', "~0020")
    ));
    let dir = project.join(log.id());
    assert_eq!(log.path(), dir.join(LOG_FILENAME));
    assert!(dir.join(LOCK_FILENAME).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let bytes = std::fs::read(log.path()).unwrap();
    assert_eq!(
        frame_count(&bytes),
        3,
        "header frame + one frame per flushed batch"
    );
    let parsed = parse_log(&bytes).unwrap();
    assert_eq!(parsed.header, log.header());
    assert_eq!(parsed.events, log.events());
    validate_log(&parsed.header, &parsed.events).unwrap();
    // sourceEventSeqs survives the run-compressed storage form.
    assert_eq!(parsed.events[14].extra["sourceEventSeqs"], json!([13]));
    let texts: Vec<String> = derive_messages(&parsed.events)
        .iter()
        .map(|m| m.plain_text())
        .collect();
    assert_eq!(texts.len(), 5);
    assert_eq!(texts[1], "run date");
    assert_eq!(texts[3], "Mon Oct 20\n");
    assert_eq!(texts[4], "done");
    assert_eq!(log.messages(), derive_messages(&parsed.events));
}

#[tokio::test]
async fn appends_reach_disk_within_the_batching_window() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let log = store.create(&cwd(&tmp)).unwrap();
    log.append(vec![NewEvent::new("turn/start", json!({"turn": 1}))])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(read_log_file(log.path()).unwrap().events.len(), 1);
}

#[tokio::test]
async fn invalid_events_are_refused_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let log = store.create(&cwd(&tmp)).unwrap();
    let user = json!({"role": "user", "content": [], "source": {"kind": "user"}, "id": "u"});
    // A surface event without surfaceOp would make dsh refuse the log.
    let err = log
        .append(vec![
            NewEvent::new("turn/start", json!({"turn": 1})),
            NewEvent::new("user/message", user.clone()),
        ])
        .await
        .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<SessionError>(),
        Some(SessionError::InvalidEvent(_))
    ));
    assert!(log.events().is_empty(), "whole batch refused");
    // Unknown envelope keys are refused too.
    assert!(
        log.append(vec![
            NewEvent::new("turn/start", json!({})).with("bogus", json!(1))
        ])
        .await
        .is_err()
    );
    // Unknown and dsh-0.2.1-only types are marked ignorable for the installed dsh 0.2.0.
    let stored = log
        .append(vec![
            NewEvent::new("codex/screenshot", json!({"x": 1})),
            NewEvent::new("job/registered", json!({"jobId": "j"})),
            NewEvent::new("user/message", user).with("surfaceOp", json!("append")),
        ])
        .await
        .unwrap();
    assert_eq!(stored[0].extra["ignorable"], json!(true));
    assert_eq!(stored[1].extra["ignorable"], json!(true));
    assert!(!stored[2].extra.contains_key("ignorable"));
}

#[tokio::test]
async fn lock_excludes_second_writer_and_close_releases_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let log = store.create(&cwd(&tmp)).unwrap();
    log.append(vec![NewEvent::new("turn/start", json!({"turn": 1}))])
        .await
        .unwrap();
    log.flush().await.unwrap();
    assert!(matches!(store.open(log.id()), Err(SessionError::Locked(_))));
    assert!(matches!(
        store.delete(log.id()),
        Err(SessionError::Locked(_))
    ));
    // Readers never need the lock.
    assert_eq!(store.read(log.id()).unwrap().events.len(), 1);
    assert_eq!(store.list().unwrap()[0].status, "running");
    log.close().await.unwrap();
    assert!(
        log.append(vec![NewEvent::new("turn/end", json!({}))])
            .await
            .is_err()
    );
    assert_eq!(store.list().unwrap()[0].status, "interrupted");
    let reopened = store.open(log.id()).unwrap();
    assert_eq!(reopened.events().len(), 1);
}

#[tokio::test]
async fn torn_tail_is_recovered_and_rewritten_before_the_next_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let cwd = cwd(&tmp);
    let log = store.create(&cwd).unwrap();
    let events = common::scripted_turn(&cwd, "run date", "call_1", "date", "out", "done");
    log.append(events[..4].to_vec()).await.unwrap();
    log.flush().await.unwrap();
    let good = std::fs::metadata(log.path()).unwrap().len();
    log.append(events[4..12].to_vec()).await.unwrap();
    log.flush().await.unwrap();
    let id = log.id().to_owned();
    let path = log.path().to_owned();
    log.close().await.unwrap();
    drop(log);

    // Simulate a crash in the middle of the last batch's frame.
    let full = std::fs::metadata(&path).unwrap().len();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(full - 3).unwrap();
    drop(file);
    let parsed = read_log_file(&path).unwrap();
    assert_eq!(parsed.torn_truncate_to, Some(good));
    let recovered = parsed.events.len();
    assert!(recovered >= 4, "complete frames survive");

    let log = store.open(&id).unwrap();
    assert_eq!(log.events().len(), recovered);
    log.append(vec![NewEvent::new(
        "step/start",
        json!({"turn": 1, "step": 1}),
    )])
    .await
    .unwrap();
    log.flush().await.unwrap();
    let after = read_log_file(&path).unwrap();
    assert_eq!(after.torn_truncate_to, None);
    assert_eq!(after.events.len(), recovered + 1);
    assert_eq!(after.events[..recovered], parsed.events[..]);
}

#[tokio::test]
async fn resume_closes_an_interrupted_turn_like_dsh() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let cwd = cwd(&tmp);
    let log = store.create(&cwd).unwrap();
    let events = common::scripted_turn(&cwd, "run date", "call_1", "date", "out", "done");
    // Crash after tool/call (seq 13): the result never landed.
    log.append(events[..14].to_vec()).await.unwrap();
    log.close().await.unwrap();
    let id = log.id().to_owned();
    drop(log);

    let summary = &store.list().unwrap()[0];
    assert_eq!(
        (summary.status.as_str(), summary.title.as_deref()),
        ("interrupted", Some("run date"))
    );
    let cold = store.read_repaired(&id).unwrap();
    assert_eq!(cold.events.len(), 14 + 3);

    let log = store.resume(&id).await.unwrap();
    let kinds: Vec<String> = log.events()[14..].iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        ["tool/result", "step/end", "turn/end", "session/end-seed"]
    );
    let stored = store.read(&id).unwrap();
    assert_eq!(stored.events, log.events());
    validate_log(&stored.header, &stored.events).unwrap();
    assert!(interrupted_turn_closers(&stored.events).is_empty());
    assert_eq!(stored.messages().last().unwrap().is_error, Some(true));
    assert_eq!(store.list().unwrap()[0].status, "interrupted");
}

#[tokio::test]
async fn listing_and_delete_across_workspaces() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path().join("root"));
    let a = store.create("/tmp/ws-a").unwrap();
    let b = store.create("/tmp/ws-b").unwrap();
    let never = store.create("/tmp/ws-c").unwrap();
    a.append(common::scripted_turn(
        "/tmp/ws-a",
        "first",
        "c1",
        "date",
        "x",
        "ok",
    ))
    .await
    .unwrap();
    a.append(vec![NewEvent::new(
        "session/title",
        json!({"title": "Better title", "messageSeqs": [8], "source": {"kind": "provider", "provider": "p"}}),
    )])
    .await
    .unwrap();
    a.close().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.append(vec![NewEvent::new(
        "permission/preset",
        json!({"preset": "read-only"}),
    )])
    .await
    .unwrap();
    b.close().await.unwrap();
    drop(never);

    let list = store.list().unwrap();
    assert_eq!(list.len(), 2, "unmaterialized sessions are not listed");
    assert_eq!(list[0].id, b.id());
    assert_eq!(
        (list[0].status.as_str(), list[0].title.as_deref()),
        ("new", None)
    );
    assert_eq!(list[1].title.as_deref(), Some("Better title"));
    assert_eq!(list[1].status, "completed");
    assert_eq!(list[1].turns, 1);
    assert_eq!(store.list_in("/tmp/ws-a").unwrap().len(), 1);
    assert_eq!(
        store
            .workspaces()
            .unwrap()
            .iter()
            .map(|w| w.0.as_str())
            .collect::<Vec<_>>(),
        ["/tmp/ws-b", "/tmp/ws-a"]
    );

    store.delete(a.id()).unwrap();
    assert!(matches!(store.read(a.id()), Err(SessionError::NotFound(_))));
    assert!(
        !tmp.path().join("root/--tmp-ws-a--").exists(),
        "empty project dir removed"
    );
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(matches!(
        store.delete(a.id()),
        Err(SessionError::NotFound(_))
    ));
}

#[tokio::test]
async fn subagent_child_header_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path());
    let mut header = SessionStore::new_header("/tmp/ws");
    header.delegation_depth = 1;
    header
        .extra
        .insert("parentSession".into(), json!("session-parent"));
    header.extra.insert("origin".into(), json!("subagent"));
    let log = store.create_with_header(header.clone()).unwrap();
    log.flush().await.unwrap(); // header-only artifact
    let stored = store.read(&header.id).unwrap();
    assert_eq!(stored.header, header);
    assert!(stored.events.is_empty());
    assert_eq!(
        store.list().unwrap()[0].parent_session.as_deref(),
        Some("session-parent")
    );
    assert!(matches!(
        store.create_with_header(header),
        Err(SessionError::AlreadyExists(_))
    ));
}

#[tokio::test]
async fn closing_an_empty_created_session_leaves_no_footprint() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path().join("root"));
    let log = store.create("/tmp/ws").unwrap();
    log.close().await.unwrap();
    drop(log);
    assert!(!tmp.path().join("root").exists());
    assert!(store.list().unwrap().is_empty());
}

#[tokio::test]
async fn seeded_session_keeps_seq_and_time_of_its_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(tmp.path().join("sessions"));
    let cwd = cwd(&tmp);
    let source = store.create(&cwd).unwrap();
    let events = common::scripted_turn(&cwd, "run date", "call_1", "date", "Mon Oct 20\n", "done");
    source.append(events).await.unwrap();
    source.flush().await.unwrap();
    let mut seed = source.events();
    let last = seed.last().unwrap().clone();
    seed.push(dshx_core::SessionEvent {
        kind: "session/end-seed".into(),
        seq: last.seq + 1,
        time: last.time,
        data: json!({"inherited": true}),
        extra: Default::default(),
    });

    let mut header = SessionStore::new_header(&cwd);
    header
        .extra
        .insert("parentSession".into(), json!(source.id()));
    assert!(
        matches!(
            store.create_seeded(header.clone(), seed.clone()),
            Err(SessionError::InvalidEvent(_))
        ),
        "an unseeded header refuses an inherited cut"
    );
    header.is_seeded = true;
    let child = store.create_seeded(header.clone(), seed.clone()).unwrap();
    let next = child
        .append(vec![NewEvent::new("session/end-seed", json!({}))])
        .await
        .unwrap();
    assert_eq!(next[0].seq, seed.len() as u64);
    child.flush().await.unwrap();

    let stored = store.read(&header.id).unwrap();
    assert_eq!(stored.header, header);
    assert_eq!(&stored.events[..seed.len()], &seed[..], "seq and time kept");
    validate_log(&stored.header, &stored.events).unwrap();
    let row = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == header.id)
        .unwrap();
    assert_eq!(row.parent_session.as_deref(), Some(source.id()));
    assert_eq!(row.origin, None, "a branch is no subagent");
    assert!(matches!(
        store.create_seeded(header, seed),
        Err(SessionError::AlreadyExists(_))
    ));
}
