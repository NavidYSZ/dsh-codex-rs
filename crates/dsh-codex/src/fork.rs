//! Branching a chat (dsh `sessions.fork` with `atSeq`, the UI's "Branch
//! into a new conversation"): a new top-level session that inherits the
//! source's log through the end of one turn. Like dsh, the child's header
//! names the source as `parentSession` (without `origin: "subagent"`), is
//! `isSeeded`, and its seed ends with the `session/end-seed {inherited: true}`
//! cut; the copied events keep their seq and time.
use dshx_core::{SessionEvent, SessionHeader};
use dshx_session::SessionStore;
use serde_json::{Value, json};

/// The seed of a branch after `turn`: every event through that turn's
/// `turn/end`, then the inherited cut (dsh `buildForkSeed`). Turn 0 branches
/// before the first turn and inherits nothing but the cut.
pub fn fork_seed(events: &[SessionEvent], turn: u64) -> anyhow::Result<Vec<SessionEvent>> {
    let (mut seed, time) = if turn == 0 {
        (Vec::new(), dshx_core::now_ms())
    } else {
        let end = events
            .iter()
            .position(|e| {
                e.kind == "turn/end" && e.data.get("turn").and_then(Value::as_u64) == Some(turn)
            })
            .ok_or_else(|| {
                anyhow::anyhow!("turn {turn} has not ended, so it cannot be branched")
            })?;
        (events[..=end].to_vec(), events[end].time)
    };
    seed.push(SessionEvent {
        kind: "session/end-seed".into(),
        seq: seed.len() as u64,
        time,
        data: json!({ "inherited": true }),
        extra: Default::default(),
    });
    Ok(seed)
}

/// A fresh header in the source's folder, linked to it and keeping its
/// agent preset (the mode is fixed for a chat's life).
pub fn fork_header(source: &SessionHeader) -> SessionHeader {
    let mut header = SessionStore::new_header(&source.cwd);
    header.is_seeded = true;
    header
        .extra
        .insert("parentSession".into(), json!(source.id));
    if let Some(preset) = source.extra.get("agentPreset") {
        header.extra.insert("agentPreset".into(), preset.clone());
    }
    header
}

/// dsh `increasedForkTitle`: `Title (1)`, then `Title (2)` for a branch of
/// that branch (full-width parentheses stay full-width).
pub fn fork_title(title: &str) -> String {
    for (open, close) in [('(', ')'), ('（', '）')] {
        if let Some(head) = title.strip_suffix(close)
            && let Some((base, n)) = head.rsplit_once(open)
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
            && let Ok(n) = n.parse::<u64>()
        {
            return format!("{base}{open}{}{close}", n + 1);
        }
    }
    format!("{title} (1)")
}

/// The latest `session/title` of a log.
pub fn log_title(events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .filter(|e| e.kind == "session/title")
        .find_map(|e| e.data.get("title").and_then(Value::as_str))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 1_000 + seq,
            data,
            extra: Default::default(),
        }
    }

    fn two_turns() -> Vec<SessionEvent> {
        let mut events = Vec::new();
        for turn in 1..=2 {
            let n = events.len() as u64;
            events.push(ev(n, "turn/start", json!({ "turn": turn })));
            events.push(ev(
                n + 1,
                "session/title",
                json!({ "title": format!("T{turn}") }),
            ));
            events.push(ev(
                n + 2,
                "turn/end",
                json!({ "turn": turn, "reason": { "kind": "completed" } }),
            ));
        }
        events
    }

    #[test]
    fn seed_ends_after_the_turn_with_the_inherited_cut() {
        let events = two_turns();
        let seed = fork_seed(&events, 1).unwrap();
        assert_eq!(&seed[..3], &events[..3]);
        assert_eq!(seed.len(), 4);
        assert_eq!(seed[3].kind, "session/end-seed");
        assert_eq!((seed[3].seq, seed[3].time), (3, events[2].time));
        assert_eq!(seed[3].data, json!({ "inherited": true }));
        assert_eq!(fork_seed(&events, 2).unwrap().len(), 7);
        let empty = fork_seed(&events, 0).unwrap();
        assert_eq!((empty.len(), empty[0].seq), (1, 0));
        assert!(fork_seed(&events, 3).is_err(), "an unknown or open turn");
    }

    #[test]
    fn header_links_the_source_and_keeps_its_preset() {
        let mut source = SessionStore::new_header("/w");
        source.extra.insert("agentPreset".into(), json!("minimal"));
        let child = fork_header(&source);
        assert_ne!(child.id, source.id);
        assert_eq!(child.cwd, "/w");
        assert!(child.is_seeded);
        assert_eq!(child.extra["parentSession"], json!(source.id));
        assert_eq!(child.extra["agentPreset"], json!("minimal"));
        assert!(!child.extra.contains_key("origin"));
    }

    #[test]
    fn titles_count_up_like_dsh() {
        assert_eq!(fork_title("Fix the parser"), "Fix the parser (1)");
        assert_eq!(fork_title("Fix the parser (1)"), "Fix the parser (2)");
        assert_eq!(fork_title("解析（9）"), "解析（10）");
        assert_eq!(fork_title("Call (me)"), "Call (me) (1)");
        assert_eq!(log_title(&two_turns()).as_deref(), Some("T2"));
    }
}
