//! On-disk vocabulary of dsh session format v4: directory layout, header line,
//! event rows and the `sourceEventSeqs` run compression.
//!
//! Mirrors `session-persistence-jsonl/src/format.ts` and the released v2/v4
//! physical codecs (`session-format-v1-to-v2/src/codec.ts`,
//! `session-format-v3-to-v4/src/codec.ts`).
use std::path::{Path, PathBuf};

use dshx_core::{SESSION_FORMAT_VERSION, SessionEvent, SessionHeader};
use serde_json::{Map, Value};

use crate::error::{Result, SessionError};

/// Current generation file inside a session directory.
pub const LOG_FILENAME: &str = "session.v4.jsonl.zstd";
/// Kernel lock file beside the log (dsh `LEASE_FILENAME`).
pub const LOCK_FILENAME: &str = "session.lock";
/// Project directory for sessions without a cwd.
pub const NO_CWD_DIR: &str = "_no-cwd";

/// Every event type dsh 0.2.1 understands (`known-event-types.ts`). A log
/// with a type outside this set and without `ignorable: true` is refused.
pub const KNOWN_EVENT_TYPES: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "deliverables/presented",
    "developer/message",
    "feedback/message-delete",
    "feedback/message-put",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "image/offload",
    "job/registered",
    "job/settled",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/catalog",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "system/message",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
    "working-directory/change",
    "workspace/changes",
];

/// Types the installed official dsh 0.2.0-rc.2 does not know yet; `FileLog`
/// marks them `ignorable` so that build can still open our sessions.
pub const NOT_IN_DSH_0_2_0: &[&str] =
    &["job/registered", "job/settled", "working-directory/change"];

/// Message-producing event types (the only ones that may carry surface metadata).
pub const SURFACE_EVENT_TYPES: &[&str] = &[
    "system/message",
    "developer/message",
    "user/message",
    "assistant/message",
    "tool/result",
];

pub fn is_known_event_type(kind: &str) -> bool {
    KNOWN_EVENT_TYPES.binary_search(&kind).is_ok()
}

pub fn is_surface_event_type(kind: &str) -> bool {
    SURFACE_EVENT_TYPES.contains(&kind)
}

/// Whether the installed dsh 0.2.0-rc.2 would refuse this type without `ignorable`.
pub fn needs_ignorable_for_dsh_0_2_0(kind: &str) -> bool {
    !is_known_event_type(kind) || NOT_IN_DSH_0_2_0.contains(&kind)
}

/// Optional event envelope keys beside `type`, `seq`, `time`, `data`.
const EVENT_OPTIONAL_KEYS: &[&str] = &["ignorable", "sourceEventSeqs", "surfaceOp"];
const HEADER_OPTIONAL_KEYS: &[&str] = &["parentSession", "origin", "agentPreset"];

// --- paths ---

fn is_safe_unit(unit: u16) -> bool {
    unit < 0x80 && {
        let c = unit as u8;
        c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-'
    }
}

/// Injective single-path-segment encoding of a session id over UTF-16 code
/// units, exactly like dsh `encodeSegment`.
pub fn encode_segment(raw: &str) -> Result<String> {
    if raw.is_empty() {
        return Err(SessionError::InvalidEvent(
            "cannot encode an empty path segment".into(),
        ));
    }
    if raw == "." {
        return Ok("~002E".into());
    }
    if raw == ".." {
        return Ok("~002E~002E".into());
    }
    let mut out = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        if is_safe_unit(unit) {
            out.push(unit as u8 as char);
        } else {
            out.push_str(&format!("~{unit:04X}"));
        }
    }
    Ok(out)
}

/// Readable project directory key for a cwd (`--tmp-x--`), like dsh `projectKey`.
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        if unit == u16::from(b'/') || unit == u16::from(b'\\') || unit == u16::from(b':') {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if is_safe_unit(unit) {
            readable.push(unit as u8 as char);
            separator_run = false;
        } else {
            readable.push_str(&format!("~{unit:04X}"));
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    // ASCII only at this point, so byte slicing equals JS code-unit slicing.
    format!("--{}--", &slug[..slug.len().min(251)])
}

/// `<root>/--<cwd>--` (or `<root>/_no-cwd` for an empty cwd).
pub fn project_dir(root: &Path, cwd: &str) -> PathBuf {
    if cwd.is_empty() {
        root.join(NO_CWD_DIR)
    } else {
        root.join(project_key(cwd))
    }
}

/// `<root>/--<cwd>--/<encoded id>`.
pub fn session_dir(root: &Path, cwd: &str, id: &str) -> Result<PathBuf> {
    Ok(project_dir(root, cwd).join(encode_segment(id)?))
}

/// Generation of a canonical log filename: `session.jsonl.zstd` = 0,
/// `session.vN.jsonl.zstd` = N; anything else (temp files, locks) = `None`.
pub fn parse_generation_filename(name: &str) -> Option<u32> {
    let base = name.strip_suffix(".jsonl.zstd")?;
    if base == "session" {
        return Some(0);
    }
    let digits = base.strip_prefix("session.v")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

// --- header line ---

/// The physical header record, keys in dsh's order, without trailing newline.
pub fn encode_header_line(header: &SessionHeader) -> Result<String> {
    if header.version != SESSION_FORMAT_VERSION {
        return Err(SessionError::InvalidEvent(format!(
            "header version must be {SESSION_FORMAT_VERSION}"
        )));
    }
    if !header.cwd.is_empty() && !is_absolute(&header.cwd) {
        return Err(SessionError::InvalidEvent(format!(
            "header cwd must be absolute, got {:?}",
            header.cwd
        )));
    }
    if let Some(key) = header
        .extra
        .keys()
        .find(|k| !HEADER_OPTIONAL_KEYS.contains(&k.as_str()))
    {
        return Err(SessionError::InvalidEvent(format!(
            "format v4 header has unexpected field {key}"
        )));
    }
    let mut line = Map::new();
    line.insert("type".into(), Value::from("session"));
    line.insert("version".into(), Value::from(header.version));
    line.insert("id".into(), Value::from(header.id.clone()));
    line.insert("createdAt".into(), Value::from(header.created_at));
    if !header.cwd.is_empty() {
        line.insert("cwd".into(), Value::from(header.cwd.clone()));
    }
    if let Some(v) = header.extra.get("parentSession") {
        line.insert("parentSession".into(), v.clone());
    }
    line.insert("isSeeded".into(), Value::from(header.is_seeded));
    if let Some(v) = header.extra.get("origin") {
        line.insert("origin".into(), v.clone());
    }
    line.insert(
        "delegationDepth".into(),
        Value::from(header.delegation_depth),
    );
    if let Some(v) = header.extra.get("agentPreset") {
        line.insert("agentPreset".into(), v.clone());
    }
    Ok(Value::Object(line).to_string())
}

fn is_absolute(path: &str) -> bool {
    // dsh validates with Node's `isAbsolute` of the host platform.
    Path::new(path).is_absolute() || path.starts_with('/')
}

fn safe_count(value: &Value) -> Option<u64> {
    value.as_u64().filter(|v| *v < (1u64 << 53))
}

/// Parses and validates the header record (dsh `isHeaderLine` + version refusal).
pub fn decode_header_line(line: &[u8]) -> Result<SessionHeader> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|_| SessionError::Corrupt("header line is not valid JSON".into()))?;
    let Value::Object(obj) = value else {
        return Err(SessionError::Corrupt(
            "first line is not a JSON object".into(),
        ));
    };
    // A foreign version is "upgrade/migrate", never "corrupt".
    if let Some(version) = obj.get("version").and_then(Value::as_u64)
        && version != u64::from(SESSION_FORMAT_VERSION)
    {
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("?");
        let hint = if version > u64::from(SESSION_FORMAT_VERSION) {
            "it was written by a newer harness"
        } else {
            "open it once with the official dsh to migrate it"
        };
        return Err(SessionError::Unsupported(format!(
            "session \"{id}\" uses format v{version}; this build reads v{SESSION_FORMAT_VERSION} ({hint})"
        )));
    }
    if obj.contains_key("sandboxMode") || obj.contains_key("approvalPolicy") {
        return Err(SessionError::Corrupt(
            "session header uses retired policy baseline fields".into(),
        ));
    }
    let bad = || SessionError::Corrupt("first line is not a session header".into());
    for key in obj.keys() {
        let allowed = matches!(
            key.as_str(),
            "type" | "version" | "id" | "createdAt" | "isSeeded" | "delegationDepth" | "cwd"
        ) || HEADER_OPTIONAL_KEYS.contains(&key.as_str());
        if !allowed {
            return Err(bad());
        }
    }
    if obj.get("type").and_then(Value::as_str) != Some("session") {
        return Err(bad());
    }
    let version = obj.get("version").and_then(Value::as_u64).ok_or_else(bad)?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    let created_at = obj.get("createdAt").and_then(safe_count).ok_or_else(bad)?;
    let delegation_depth = obj
        .get("delegationDepth")
        .and_then(safe_count)
        .ok_or_else(bad)?;
    let is_seeded = obj
        .get("isSeeded")
        .and_then(Value::as_bool)
        .ok_or_else(bad)?;
    let cwd = match obj.get("cwd") {
        None => String::new(),
        Some(Value::String(s)) if is_absolute(s) => s.clone(),
        Some(_) => return Err(bad()),
    };
    let mut extra = Map::new();
    for key in HEADER_OPTIONAL_KEYS {
        if let Some(v) = obj.get(*key) {
            let ok = match *key {
                "origin" => v.as_str() == Some("subagent"),
                _ => v.is_string(),
            };
            if !ok {
                return Err(bad());
            }
            extra.insert((*key).to_owned(), v.clone());
        }
    }
    Ok(SessionHeader {
        kind: "session".into(),
        version: version as u32,
        id,
        created_at,
        cwd,
        is_seeded,
        delegation_depth: u32::try_from(delegation_depth).map_err(|_| bad())?,
        extra,
    })
}

// --- sourceEventSeqs ---

/// Lossless storage form: consecutive runs of at least three seqs become
/// `[start, end]`; a list that is not strictly increasing stays verbatim.
pub fn encode_seq_ranges(values: &[u64]) -> Vec<Value> {
    if values.windows(2).any(|w| w[1] <= w[0]) {
        return values.iter().map(|v| Value::from(*v)).collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < values.len() {
        let mut end = start;
        while end + 1 < values.len() && values[end + 1] == values[end] + 1 {
            end += 1;
        }
        if end - start >= 2 {
            out.push(Value::from(vec![values[start], values[end]]));
        } else {
            out.extend(values[start..=end].iter().map(|v| Value::from(*v)));
        }
        start = end + 1;
    }
    out
}

/// Expands the storage form; every seq must be unique and earlier than `seq`.
pub fn decode_seq_ranges(value: &Value, seq: u64) -> Result<Vec<u64>, String> {
    let entries = value.as_array().ok_or("sourceEventSeqs must be an array")?;
    let mut out: Vec<u64> = Vec::new();
    let mut has_range = false;
    for entry in entries {
        match entry {
            Value::Array(pair) => {
                let [start, end] = pair.as_slice() else {
                    return Err("sourceEventSeqs range must be a [start, end] pair".into());
                };
                let start =
                    safe_count(start).ok_or("sourceEventSeqs range start must be a count")?;
                let end = safe_count(end).ok_or("sourceEventSeqs range end must be a count")?;
                if start > end
                    || end >= seq
                    || end - start + 1 > seq.saturating_sub(out.len() as u64)
                {
                    return Err("sourceEventSeqs range exceeds its event seq".into());
                }
                out.extend(start..=end);
                has_range = true;
            }
            other => out.push(safe_count(other).ok_or("sourceEventSeqs member must be a count")?),
        }
    }
    let mut seen = std::collections::HashSet::new();
    if out.iter().any(|s| *s >= seq || !seen.insert(*s)) {
        return Err("sourceEventSeqs ranges must contain unique earlier seqs".into());
    }
    if has_range && out.windows(2).any(|w| w[1] <= w[0]) {
        return Err("sourceEventSeqs ranges must be strictly increasing".into());
    }
    Ok(out)
}

// --- event rows ---

/// One physical event row without trailing newline: `type, seq, time, data`,
/// then the optional envelope keys in their insertion order.
pub fn encode_event_line(event: &SessionEvent) -> Result<String> {
    let mut row = Map::new();
    row.insert("type".into(), Value::from(event.kind.clone()));
    row.insert("seq".into(), Value::from(event.seq));
    row.insert("time".into(), Value::from(event.time));
    row.insert("data".into(), event.data.clone());
    for (key, value) in &event.extra {
        match key.as_str() {
            "sourceEventSeqs" => {
                let seqs: Vec<u64> = value
                    .as_array()
                    .and_then(|a| a.iter().map(Value::as_u64).collect())
                    .ok_or_else(|| {
                        SessionError::InvalidEvent(
                            "sourceEventSeqs must be an array of seqs".into(),
                        )
                    })?;
                row.insert(key.clone(), Value::Array(encode_seq_ranges(&seqs)));
            }
            "ignorable" | "surfaceOp" => {
                row.insert(key.clone(), value.clone());
            }
            other => {
                return Err(SessionError::InvalidEvent(format!(
                    "{} at seq {} has unexpected envelope field {other}",
                    event.kind, event.seq
                )));
            }
        }
    }
    Ok(Value::Object(row).to_string())
}

/// Decodes one parsed row (released v2 row rules): exact envelope keys,
/// `ignorable` only `true`, `sourceEventSeqs` expanded. Seq density is the
/// caller's check.
pub fn decode_event_value(value: Value) -> Result<SessionEvent, String> {
    let Value::Object(mut obj) = value else {
        return Err("row must be an object".into());
    };
    for key in ["type", "seq", "time", "data"] {
        if !obj.contains_key(key) {
            return Err(format!("row lacks required field {key}"));
        }
    }
    if let Some(key) = obj.keys().find(|k| {
        !matches!(k.as_str(), "type" | "seq" | "time" | "data")
            && !EVENT_OPTIONAL_KEYS.contains(&k.as_str())
    }) {
        return Err(format!("row has unexpected field {key}"));
    }
    let kind = match obj.shift_remove("type") {
        Some(Value::String(s)) => s,
        _ => return Err("row type must be a string".into()),
    };
    let seq = obj
        .shift_remove("seq")
        .as_ref()
        .and_then(safe_count)
        .ok_or("row seq must be a count")?;
    let time = obj
        .shift_remove("time")
        .as_ref()
        .and_then(safe_count)
        .ok_or("row time must be a non-negative integer")?;
    let data = obj.shift_remove("data").unwrap_or(Value::Null);
    if let Some(ignorable) = obj.get("ignorable")
        && ignorable != &Value::Bool(true)
    {
        return Err("row ignorable must be true when present".into());
    }
    if let Some(sources) = obj.get_mut("sourceEventSeqs") {
        let expanded = decode_seq_ranges(sources, seq)?;
        *sources = Value::from(expanded);
    }
    Ok(SessionEvent {
        kind,
        seq,
        time,
        data,
        extra: obj,
    })
}

/// Format-level row admission that refuses outright instead of being treated
/// as a damaged tail (dsh `assertV4RowAdmission`: retired syntax).
pub fn assert_row_admission(row: &Value) -> Result<()> {
    let Some(obj) = row.as_object() else {
        return Ok(());
    };
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let ignorable = obj.get("ignorable") == Some(&Value::Bool(true));
    if (kind == "tool/code-dispatch-start" || kind == "tool/code-dispatch") && !ignorable {
        return Err(SessionError::Unsupported(format!(
            "format v4 rejects retired event type {kind}"
        )));
    }
    if kind == "request/header"
        && let Some(header) = obj
            .get("data")
            .and_then(|d| d.get("header"))
            .and_then(Value::as_object)
        && header.contains_key("system")
    {
        return Err(SessionError::Unsupported(
            "format v4 request/header rejects retired header.system".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_are_sorted_for_binary_search() {
        assert!(KNOWN_EVENT_TYPES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn segment_and_project_keys_match_dsh() {
        assert_eq!(encode_segment("session-1a2b").unwrap(), "session-1a2b");
        assert_eq!(encode_segment("..").unwrap(), "~002E~002E");
        assert_eq!(encode_segment("a/b~c").unwrap(), "a~002Fb~007Ec");
        assert_eq!(encode_segment("ä😀").unwrap(), "~00E4~D83D~DE00");
        assert_eq!(
            project_key("/tmp/claude-0/-root-workspaces/x y"),
            "--tmp-claude-0--root-workspaces-x~0020y--"
        );
        assert_eq!(project_key("/"), "--root--");
        assert_eq!(project_key("C:\\Users\\me"), "--C-Users-me--");
        assert_eq!(project_key(&format!("/{}", "a".repeat(300))).len(), 255);
    }

    #[test]
    fn generation_filenames() {
        assert_eq!(parse_generation_filename("session.v4.jsonl.zstd"), Some(4));
        assert_eq!(parse_generation_filename("session.jsonl.zstd"), Some(0));
        assert_eq!(parse_generation_filename("session.v04.jsonl.zstd"), None);
        assert_eq!(
            parse_generation_filename("session.v4.jsonl.zstd.abc.tmp"),
            None
        );
        assert_eq!(parse_generation_filename("session.lock"), None);
    }

    #[test]
    fn seq_ranges_round_trip_like_dsh() {
        let enc = |v: &[u64]| Value::Array(encode_seq_ranges(v)).to_string();
        assert_eq!(enc(&[16]), "[16]");
        assert_eq!(enc(&[3, 4]), "[3,4]");
        assert_eq!(
            enc(&[3, 4, 5, 9, 10, 12, 13, 14, 15]),
            "[[3,5],9,10,[12,15]]"
        );
        assert_eq!(enc(&[5, 3]), "[5,3]");
        let value: Value = serde_json::from_str("[[3,5],9,10,[12,15]]").unwrap();
        assert_eq!(
            decode_seq_ranges(&value, 20).unwrap(),
            vec![3, 4, 5, 9, 10, 12, 13, 14, 15]
        );
        assert!(decode_seq_ranges(&value, 15).is_err());
        assert!(decode_seq_ranges(&serde_json::json!([1, 1]), 5).is_err());
        assert!(decode_seq_ranges(&serde_json::json!([[4, 2]]), 5).is_err());
    }

    #[test]
    fn header_line_round_trip() {
        let line = r#"{"type":"session","version":4,"id":"session-x","createdAt":1792528628839,"cwd":"/tmp/w","isSeeded":false,"delegationDepth":0}"#;
        let header = decode_header_line(line.as_bytes()).unwrap();
        assert_eq!(header.cwd, "/tmp/w");
        assert_eq!(encode_header_line(&header).unwrap(), line);
        let child = r#"{"type":"session","version":4,"id":"c","createdAt":1,"cwd":"/w","parentSession":"p","isSeeded":true,"origin":"subagent","delegationDepth":1,"agentPreset":"x"}"#;
        assert_eq!(
            encode_header_line(&decode_header_line(child.as_bytes()).unwrap()).unwrap(),
            child
        );
        assert!(matches!(
            decode_header_line(br#"{"type":"session","version":5,"id":"s"}"#),
            Err(SessionError::Unsupported(_))
        ));
        assert!(matches!(
            decode_header_line(br#"{"type":"session","version":4,"id":"s","createdAt":1,"isSeeded":false,"delegationDepth":0,"x":1}"#),
            Err(SessionError::Corrupt(_))
        ));
    }

    #[test]
    fn event_row_round_trip() {
        let line = r#"{"type":"tool/result","seq":19,"time":5,"data":{"x":1},"sourceEventSeqs":[[2,4],16],"surfaceOp":"append"}"#;
        let event = decode_event_value(serde_json::from_str(line).unwrap()).unwrap();
        assert_eq!(
            event.extra["sourceEventSeqs"],
            serde_json::json!([2, 3, 4, 16])
        );
        assert_eq!(encode_event_line(&event).unwrap(), line);
        assert!(
            decode_event_value(
                serde_json::json!({"type":"x","seq":0,"time":0,"data":{},"other":1})
            )
            .is_err()
        );
        assert!(
            decode_event_value(
                serde_json::json!({"type":"x","seq":0,"time":0,"data":{},"ignorable":false})
            )
            .is_err()
        );
        assert!(decode_event_value(serde_json::json!({"type":"x","seq":0,"time":0})).is_err());
    }
}
