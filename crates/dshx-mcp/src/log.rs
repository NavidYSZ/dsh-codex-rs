//! The recent history of one server for the MCP page's log: what a stdio
//! server wrote to stderr, failed HTTP requests and the supervisor's
//! connection events. A ring of [`LOG_LINES`] lines that survives restarts
//! of the server (the handle keeps one per name).
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Lines kept per server.
pub const LOG_LINES: usize = 200;

/// Longer lines are cut (a server may print a whole JSON document).
const MAX_LINE_CHARS: usize = 2_000;

/// One log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    /// Milliseconds since the epoch.
    pub time: u64,
    /// `stderr` (the server's own output), `error` or `info` (connection
    /// events of the client).
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct ServerLog {
    lines: Mutex<VecDeque<LogLine>>,
}

impl ServerLog {
    pub fn push(&self, kind: &'static str, text: impl Into<String>) {
        let mut text: String = text.into();
        if let Some((cut, _)) = text.char_indices().nth(MAX_LINE_CHARS) {
            text.truncate(cut);
            text.push('…');
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut lines = self.lines.lock().unwrap();
        if lines.len() == LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(LogLine { time, kind, text });
    }

    pub fn info(&self, text: impl Into<String>) {
        self.push("info", text);
    }

    pub fn error(&self, text: impl Into<String>) {
        self.push("error", text);
    }

    /// Oldest first.
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }

    /// The server's own stderr lines, oldest first.
    pub fn stderr(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.kind == "stderr")
            .map(|l| l.text.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_lines_in_order() {
        let log = ServerLog::default();
        for i in 0..LOG_LINES + 25 {
            log.push(
                if i % 2 == 0 { "stderr" } else { "error" },
                format!("line {i}"),
            );
        }
        let lines = log.lines();
        assert_eq!(lines.len(), LOG_LINES);
        assert_eq!(lines[0].text, "line 25");
        assert_eq!(
            lines[LOG_LINES - 1].text,
            format!("line {}", LOG_LINES + 24)
        );
        assert!(lines.windows(2).all(|w| w[0].time <= w[1].time));
        assert_eq!(log.stderr().len(), LOG_LINES / 2);
        assert!(log.stderr().iter().all(|l| {
            let n: usize = l.trim_start_matches("line ").parse().unwrap();
            n.is_multiple_of(2)
        }));
    }

    #[test]
    fn cuts_long_lines_on_a_char_boundary() {
        let log = ServerLog::default();
        log.info("ä".repeat(MAX_LINE_CHARS + 10));
        let text = &log.lines()[0].text;
        assert_eq!(text.chars().count(), MAX_LINE_CHARS + 1);
        assert!(text.ends_with('…'));
    }
}
