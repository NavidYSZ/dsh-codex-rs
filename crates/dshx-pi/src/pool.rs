//! The process cap: at most `max` Pi processes; a new one replaces the one
//! idle the longest, and when all are busy the start waits (its prompt
//! stays visible in the queue meanwhile).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

pub type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// Stops a chat's process if that chat is still idle; `true` when it did.
pub type Stopper = Arc<dyn Fn() -> BoxFuture<bool> + Send + Sync>;

struct Slot {
    busy: bool,
    since: Instant,
    stop: Stopper,
}

pub struct Pool {
    max: usize,
    slots: Mutex<HashMap<String, Slot>>,
    freed: Notify,
}

impl Pool {
    pub fn new(max: usize) -> Arc<Pool> {
        Arc::new(Pool {
            max: max.max(1),
            slots: Mutex::new(HashMap::new()),
            freed: Notify::new(),
        })
    }

    pub fn max(&self) -> usize {
        self.max
    }

    pub fn running(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    /// Takes a slot for `id`, stopping the longest-idle other process when
    /// full and waiting while every process is busy.
    pub async fn acquire(&self, id: &str, stop: Stopper) {
        loop {
            let notified = self.freed.notified();
            let victim = {
                let mut slots = self.slots.lock().unwrap();
                if slots.contains_key(id) || slots.len() < self.max {
                    slots.insert(
                        id.to_owned(),
                        Slot {
                            busy: true,
                            since: Instant::now(),
                            stop: stop.clone(),
                        },
                    );
                    return;
                }
                slots
                    .iter()
                    .filter(|(_, s)| !s.busy)
                    .min_by_key(|(_, s)| s.since)
                    .map(|(k, s)| (k.clone(), s.stop.clone()))
            };
            match victim {
                Some((other, stopper)) => {
                    if !stopper().await {
                        // It became busy meanwhile; look again.
                        let mut slots = self.slots.lock().unwrap();
                        if let Some(s) = slots.get_mut(&other) {
                            s.since = Instant::now();
                        }
                    }
                }
                None => notified.await,
            }
        }
    }

    /// The chat's process works (`true`) or waits idle (`false`).
    pub fn set_busy(&self, id: &str, busy: bool) {
        if let Some(s) = self.slots.lock().unwrap().get_mut(id) {
            s.busy = busy;
            s.since = Instant::now();
        }
        if !busy {
            self.freed.notify_waiters();
        }
    }

    /// The chat's process ended.
    pub fn release(&self, id: &str) {
        self.slots.lock().unwrap().remove(id);
        self.freed.notify_waiters();
    }

    /// Stops every process idle for longer than `idle`.
    pub async fn reap(&self, idle: Duration) {
        let due: Vec<Stopper> = {
            let slots = self.slots.lock().unwrap();
            slots
                .values()
                .filter(|s| !s.busy && s.since.elapsed() >= idle)
                .map(|s| s.stop.clone())
                .collect()
        };
        for stop in due {
            stop().await;
        }
    }
}
