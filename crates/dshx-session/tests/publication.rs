//! The live feed must preserve the sequence order assigned by the log.
use dshx_core::{NewEvent, SessionLog};
use dshx_session::{MemoryLog, SessionStore};
use serde_json::json;
use std::sync::{Arc, Barrier};

fn concurrent_batches(log: Arc<dyn SessionLog>) {
    const WRITERS: usize = 16;
    const BATCHES: usize = 8;
    const SIZE: usize = 4;
    let mut feed = log.subscribe();
    let barrier = Arc::new(Barrier::new(WRITERS));
    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let log = log.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                for batch in 0..BATCHES {
                    barrier.wait();
                    let events = (0..SIZE).map(|item| NewEvent::new("test/publication", json!({
                        "writer": writer, "batch": batch, "item": item, "padding": "x".repeat(2048)
                    }))).collect();
                    futures::executor::block_on(log.append(events)).unwrap();
                }
            });
        }
    });
    for expected in 0..(WRITERS * BATCHES * SIZE) {
        assert_eq!(feed.try_recv().unwrap().seq, expected as u64);
    }
    assert!(feed.try_recv().is_err());
}

#[test]
fn memory_log_publishes_concurrent_batches_in_sequence() {
    concurrent_batches(Arc::new(MemoryLog::new("/tmp")));
}

#[test]
fn file_log_publishes_concurrent_batches_in_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionStore::new(dir.path()).create("/tmp").unwrap();
    concurrent_batches(Arc::new(log));
}
