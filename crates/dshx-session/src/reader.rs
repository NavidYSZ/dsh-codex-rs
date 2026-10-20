//! Reader for v4 logs, with dsh's recovery rules (`index.ts` `readZstdPrefix`,
//! `format.ts` `SessionLogScanner`, `validation.ts` vocabulary checks).
use std::path::Path;

use dshx_core::{SessionEvent, SessionHeader};
use serde_json::Value;

use crate::error::{IoContext, Result, SessionError};
use crate::format::{
    assert_row_admission, decode_event_value, decode_header_line, is_known_event_type,
};
use crate::frames::{decompress_frame, decompress_prefix, scan_frames};

/// A decoded log: the committed event prefix plus torn-tail facts a writer
/// must repair before its next append.
#[derive(Debug, Clone)]
pub struct ParsedLog {
    pub header: SessionHeader,
    pub events: Vec<SessionEvent>,
    /// Byte offset of an incomplete final frame; a writer truncates it away.
    pub torn_truncate_to: Option<u64>,
    /// Number of trailing `events` recovered from that torn frame (a writer
    /// rewrites them durably as one batch).
    pub recovered_tail: usize,
    /// Physical size of the file read.
    pub size_bytes: u64,
}

/// Incremental row scanner with dsh's recoverable-tail semantics: after the
/// first damaged row nothing more is accepted, and a later `turn/end`
/// proves the damage is not a crash tail.
struct Scanner {
    events: Vec<SessionEvent>,
    fragment: Vec<u8>,
    input_bytes: usize,
    committed_bytes: usize,
    line: usize,
    issue: Option<String>,
}

impl Scanner {
    fn new() -> Self {
        Scanner {
            events: Vec::new(),
            fragment: Vec::new(),
            input_bytes: 0,
            committed_bytes: 0,
            line: 0,
            issue: None,
        }
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let chunk_start = self.input_bytes;
        self.input_bytes += chunk.len();
        let mut line_start = 0;
        while let Some(rel) = chunk[line_start..].iter().position(|b| *b == b'\n') {
            let newline = line_start + rel;
            let line = if self.fragment.is_empty() {
                std::borrow::Cow::Borrowed(&chunk[line_start..newline])
            } else {
                let mut joined = std::mem::take(&mut self.fragment);
                joined.extend_from_slice(&chunk[line_start..newline]);
                std::borrow::Cow::Owned(joined)
            };
            self.consume(&line, chunk_start + newline + 1)?;
            line_start = newline + 1;
        }
        self.fragment.extend_from_slice(&chunk[line_start..]);
        Ok(())
    }

    fn consume(&mut self, line: &[u8], end_byte: usize) -> Result<()> {
        self.line += 1;
        let row: Value = match serde_json::from_slice(line) {
            Ok(row) => row,
            Err(_) => {
                self.issue.get_or_insert_with(|| {
                    format!("unparsable committed event at line {}", self.line)
                });
                return Ok(());
            }
        };
        assert_row_admission(&row)?;
        let is_turn_end = row.get("type").and_then(Value::as_str) == Some("turn/end");
        if let Some(issue) = &self.issue {
            return if is_turn_end {
                Err(SessionError::Corrupt(issue.clone()))
            } else {
                Ok(())
            };
        }
        let decoded = decode_event_value(row).and_then(|event| {
            if event.seq == self.events.len() as u64 {
                Ok(event)
            } else {
                Err(format!(
                    "seq gap (expected {}, got {})",
                    self.events.len(),
                    event.seq
                ))
            }
        });
        match decoded {
            Ok(event) => {
                self.events.push(event);
                self.committed_bytes = end_byte;
                Ok(())
            }
            Err(detail) => {
                let issue = format!("invalid committed event at line {}: {detail}", self.line);
                if is_turn_end {
                    return Err(SessionError::Corrupt(issue));
                }
                self.issue = Some(issue);
                Ok(())
            }
        }
    }
}

/// Decodes a complete or torn `session.v4.jsonl.zstd` image.
pub fn parse_log(buf: &[u8]) -> Result<ParsedLog> {
    let scan = scan_frames(buf, usize::MAX)?;
    let Some(first) = scan.frames.first() else {
        return Err(SessionError::Corrupt(
            "empty or header-less Zstandard session log".into(),
        ));
    };
    let header_plain = decompress_frame(&buf[first.clone()])?;
    let header = decode_header_record(&header_plain)?;

    let mut scanner = Scanner::new();
    for frame in &scan.frames[1..] {
        scanner.write(&decompress_frame(&buf[frame.clone()])?)?;
    }
    if scanner.committed_bytes != scanner.input_bytes {
        return Err(scanner.issue.map(SessionError::Corrupt).unwrap_or_else(|| {
            SessionError::Corrupt("complete frame contains a torn JSONL record".into())
        }));
    }
    let complete = scanner.events.len();
    let mut torn_truncate_to = None;
    if let Some(start) = scan.torn_start {
        // Complete records already flushed into a torn frame are real events.
        scanner.write(&decompress_prefix(&buf[start..]))?;
        torn_truncate_to = Some(start as u64);
    }
    let events = scanner.events;
    check_vocabulary(&header, &events)?;
    Ok(ParsedLog {
        header,
        recovered_tail: events.len() - complete,
        events,
        torn_truncate_to,
        size_bytes: buf.len() as u64,
    })
}

/// The first frame must hold exactly one newline-terminated header line.
fn decode_header_record(plain: &[u8]) -> Result<SessionHeader> {
    if plain.is_empty() || plain.iter().position(|b| *b == b'\n') != Some(plain.len() - 1) {
        return Err(SessionError::Corrupt(
            "first frame is not exactly one header line".into(),
        ));
    }
    decode_header_line(&plain[..plain.len() - 1])
}

/// dsh refuses unknown required events and checks the seed marker cut.
fn check_vocabulary(header: &SessionHeader, events: &[SessionEvent]) -> Result<()> {
    let mut last_inherited = None;
    for event in events {
        let ignorable = event.extra.get("ignorable") == Some(&Value::Bool(true));
        if !is_known_event_type(&event.kind) && !ignorable {
            return Err(SessionError::Unsupported(format!(
                "format v4 contains unknown event type {:?} at seq {} (written by a newer harness?)",
                event.kind, event.seq
            )));
        }
        if event.kind == "session/end-seed"
            && event.data.get("inherited") == Some(&Value::Bool(true))
        {
            last_inherited = Some(event.seq);
        }
    }
    if header.is_seeded && last_inherited.is_none() {
        return Err(SessionError::Corrupt(
            "seeded session lacks an inherited end-seed marker".into(),
        ));
    }
    if !header.is_seeded && last_inherited.is_some() {
        return Err(SessionError::Corrupt(
            "unseeded session contains an inherited end-seed marker".into(),
        ));
    }
    Ok(())
}

/// Reads and decodes a log file.
pub fn read_log_file(path: &Path) -> Result<ParsedLog> {
    let bytes = std::fs::read(path).at(path)?;
    parse_log(&bytes).map_err(|e| with_path(e, path))
}

/// Reads only the header frame (listing without decoding bodies).
pub fn read_header_file(path: &Path) -> Result<SessionHeader> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).at(path)?;
    let mut content = Vec::new();
    let mut chunk = vec![0u8; 8192];
    loop {
        let n = file.read(&mut chunk).at(path)?;
        if n == 0 {
            return Err(with_path(
                SessionError::Corrupt("empty or header-less Zstandard session log".into()),
                path,
            ));
        }
        content.extend_from_slice(&chunk[..n]);
        let scan = scan_frames(&content, 1).map_err(|e| with_path(e, path))?;
        if let Some(first) = scan.frames.first() {
            let plain =
                decompress_frame(&content[first.clone()]).map_err(|e| with_path(e, path))?;
            return decode_header_record(&plain).map_err(|e| with_path(e, path));
        }
    }
}

fn with_path(error: SessionError, path: &Path) -> SessionError {
    match error {
        SessionError::Corrupt(m) => {
            SessionError::Corrupt(format!("{m} (raw log: {})", path.display()))
        }
        SessionError::Unsupported(m) => {
            SessionError::Unsupported(format!("{m} (raw log: {})", path.display()))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::compress_frame;

    const HEADER: &str = r#"{"type":"session","version":4,"id":"s1","createdAt":1,"cwd":"/w","isSeeded":false,"delegationDepth":0}"#;

    fn row(seq: u64, kind: &str) -> String {
        format!(r#"{{"type":"{kind}","seq":{seq},"time":10,"data":{{"turn":1}}}}"#)
    }

    fn log(batches: &[Vec<String>]) -> Vec<u8> {
        let mut out = compress_frame(format!("{HEADER}\n").as_bytes());
        for batch in batches {
            out.extend(compress_frame(format!("{}\n", batch.join("\n")).as_bytes()));
        }
        out
    }

    #[test]
    fn reads_complete_log() {
        let bytes = log(&[
            vec![row(0, "turn/start")],
            vec![row(1, "step/start"), row(2, "step/end")],
        ]);
        let parsed = parse_log(&bytes).unwrap();
        assert_eq!(parsed.header.id, "s1");
        assert_eq!(parsed.events.len(), 3);
        assert_eq!(parsed.torn_truncate_to, None);
    }

    #[test]
    fn torn_final_frame_keeps_complete_records() {
        let mut bytes = log(&[vec![row(0, "turn/start")]]);
        let good = bytes.len();
        let rows: Vec<String> = (1..200).map(|i| row(i, "step/start")).collect();
        let frame = compress_frame(format!("{}\n", rows.join("\n")).as_bytes());
        bytes.extend_from_slice(&frame[..frame.len() - 4]);
        let parsed = parse_log(&bytes).unwrap();
        assert_eq!(parsed.torn_truncate_to, Some(good as u64));
        assert_eq!(parsed.recovered_tail, parsed.events.len() - 1);
        // A frame cut inside its header recovers nothing but still parses.
        let mut short = log(&[vec![row(0, "turn/start")]]);
        short.extend_from_slice(&frame[..5]);
        let parsed = parse_log(&short).unwrap();
        assert_eq!((parsed.events.len(), parsed.recovered_tail), (1, 0));
    }

    #[test]
    fn refuses_unknown_required_events_but_keeps_ignorable_ones() {
        let bytes = log(&[vec![row(0, "future/thing")]]);
        assert!(matches!(
            parse_log(&bytes),
            Err(SessionError::Unsupported(_))
        ));
        let ignorable =
            r#"{"type":"future/thing","seq":0,"time":1,"data":{},"ignorable":true}"#.to_owned();
        assert_eq!(parse_log(&log(&[vec![ignorable]])).unwrap().events.len(), 1);
    }

    #[test]
    fn damage_inside_complete_frames_is_corruption() {
        let bytes = log(&[vec![row(0, "turn/start"), "{not json".into()]]);
        assert!(matches!(parse_log(&bytes), Err(SessionError::Corrupt(_))));
        let gap = log(&[vec![row(0, "turn/start"), row(2, "step/start")]]);
        assert!(matches!(parse_log(&gap), Err(SessionError::Corrupt(_))));
        let retired = log(&[vec![
            r#"{"type":"tool/code-dispatch","seq":0,"time":1,"data":{}}"#.into(),
        ]]);
        assert!(matches!(
            parse_log(&retired),
            Err(SessionError::Unsupported(_))
        ));
    }

    #[test]
    fn header_frame_must_be_a_single_line() {
        let mut bytes = compress_frame(format!("{HEADER}\n{}\n", row(0, "turn/start")).as_bytes());
        assert!(parse_log(&bytes).is_err());
        bytes.clear();
        assert!(parse_log(&bytes).is_err());
    }
}
