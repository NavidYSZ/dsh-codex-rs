//! The `read` tool (dsh `tool-fs/read.ts`, `read-render.ts`).
use std::io::Read;

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Target, lang::lang_for_path, probe, resolve};
use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::state::{Observation, ToolState};
use crate::util::{utf16_len, utf16_prefix};

/// Default and maximum lines per call (dsh `READ_LIMIT`).
pub const READ_LIMIT: u64 = 2000;
/// Characters kept per line (dsh `READ_MAX_LINE_LENGTH`).
pub const READ_MAX_LINE_LENGTH: usize = 2000;
/// Bytes of selected lines per call (dsh `READ_MAX_BYTES`).
pub const READ_MAX_BYTES: usize = 50 * 1024;
/// Leading bytes checked for NUL (dsh `BINARY_SAMPLE_BYTES`).
const BINARY_SAMPLE_BYTES: usize = 8192;

#[derive(Deserialize)]
struct ReadArgs {
    file_path: String,
    offset: Option<f64>,
    limit: Option<f64>,
}

fn positive_integer(value: f64, name: &str) -> Result<u64, ToolError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 {
        return Err(fail(format!("{name} must be a positive integer")));
    }
    Ok(value as u64)
}

/// One numbered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub number: u64,
    pub text: String,
}

/// A bounded window over a file's lines (dsh `buildWindow`).
#[derive(Debug, Default)]
pub struct Window {
    pub lines: Vec<Line>,
    pub total_lines: u64,
    pub truncated_by_bytes: bool,
}

struct Scanner {
    offset: u64,
    limit: u64,
    max_line_length: usize,
    max_bytes: usize,
    window: Window,
    output_bytes: usize,
    line: String,
    line_units: usize,
    /// The cap fell inside a surrogate pair: JavaScript keeps half of it, so
    /// the line counts as over-long.
    split_pair: bool,
}

impl Scanner {
    fn new(offset: u64, limit: u64) -> Self {
        Scanner {
            offset,
            limit,
            max_line_length: READ_MAX_LINE_LENGTH,
            max_bytes: READ_MAX_BYTES,
            window: Window::default(),
            output_bytes: 0,
            line: String::new(),
            line_units: 0,
            split_pair: false,
        }
    }

    fn append(&mut self, segment: &str) {
        let cap = self.max_line_length + 1;
        if self.line_units >= cap {
            return;
        }
        let take = utf16_prefix(segment, cap - self.line_units);
        self.line_units += utf16_len(take);
        self.line.push_str(take);
        if take.len() < segment.len() && self.line_units < cap {
            self.split_pair = true;
            self.line_units = cap;
        }
    }

    fn flush(&mut self) {
        let raw = std::mem::take(&mut self.line);
        self.line_units = 0;
        let split_pair = std::mem::take(&mut self.split_pair);
        let raw = if split_pair {
            raw.as_str()
        } else {
            raw.strip_suffix('\r').unwrap_or(&raw)
        };
        self.window.total_lines += 1;
        let w = &mut self.window;
        if w.truncated_by_bytes || w.total_lines < self.offset || w.lines.len() as u64 >= self.limit
        {
            return;
        }
        let text = if split_pair || utf16_len(raw) > self.max_line_length {
            format!(
                "{}... (line truncated to {} chars)",
                utf16_prefix(raw, self.max_line_length),
                self.max_line_length
            )
        } else {
            raw.to_owned()
        };
        let bytes = text.len() + usize::from(!w.lines.is_empty());
        if self.output_bytes + bytes > self.max_bytes {
            w.truncated_by_bytes = true;
            return;
        }
        self.output_bytes += bytes;
        w.lines.push(Line {
            number: w.total_lines,
            text,
        });
    }

    fn feed(&mut self, text: &str) {
        let mut rest = text;
        while let Some(newline) = rest.find('\n') {
            self.append(&rest[..newline]);
            self.flush();
            rest = &rest[newline + 1..];
        }
        self.append(rest);
    }

    fn finish(mut self) -> Window {
        if !self.line.is_empty() {
            self.flush();
        }
        self.window
    }
}

/// Reads `path` as UTF-8 text and builds the window. Errors use dsh wording.
pub fn read_window(
    path: &std::path::Path,
    display: &str,
    offset: u64,
    limit: u64,
) -> Result<Window, ToolError> {
    let mut file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => fail(format!("cannot read \"{display}\": not found")),
        _ => fail(format!("cannot read \"{display}\": {e}")),
    })?;
    let mut scanner = Scanner::new(offset, limit);
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut sampled = 0usize;
    let mut first = true;
    let not_text = || fail(format!("cannot read \"{display}\": invalid UTF-8 text"));
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| fail(format!("cannot read \"{display}\": {e}")))?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if sampled < BINARY_SAMPLE_BYTES {
            let sample = &chunk[..chunk.len().min(BINARY_SAMPLE_BYTES - sampled)];
            if sample.contains(&0) {
                return Err(fail(format!("cannot read \"{display}\": binary file")));
            }
            sampled += sample.len();
        }
        pending.extend_from_slice(chunk);
        if first && pending.len() >= 3 {
            if pending.starts_with(&[0xEF, 0xBB, 0xBF]) {
                pending.drain(..3);
            }
            first = false;
        }
        if first {
            continue;
        }
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return Err(not_text()),
        };
        let rest = pending.split_off(valid);
        scanner.feed(std::str::from_utf8(&pending).map_err(|_| not_text())?);
        pending = rest;
    }
    if first && pending.starts_with(&[0xEF, 0xBB, 0xBF]) {
        pending.drain(..3);
    }
    let tail = std::str::from_utf8(&pending).map_err(|_| not_text())?;
    scanner.feed(tail);
    let window = scanner.finish();
    if !window.truncated_by_bytes
        && offset > window.total_lines
        && !(window.total_lines == 0 && offset == 1)
    {
        return Err(fail(format!(
            "offset {offset} is out of range for \"{display}\" ({} lines)",
            window.total_lines
        )));
    }
    Ok(window)
}

/// dsh `formatReadOutput`.
pub fn format_read_output(path: &str, offset: u64, window: &Window) -> String {
    let end_line = window
        .lines
        .last()
        .map_or(offset.saturating_sub(1), |l| l.number);
    let footer = if window.truncated_by_bytes {
        format!(
            "(Output capped. Showing lines {offset}-{end_line}. Use offset={} to continue.)",
            end_line + 1
        )
    } else if end_line < window.total_lines {
        format!(
            "(Showing lines {offset}-{end_line} of {}. Use offset={} to continue.)",
            window.total_lines,
            end_line + 1
        )
    } else {
        format!("(End of file - total {} lines)", window.total_lines)
    };
    let body = if window.lines.is_empty() {
        footer
    } else {
        let lines: Vec<String> = window
            .lines
            .iter()
            .map(|l| format!("{}: {}", l.number, l.text))
            .collect();
        format!("{}\n\n{footer}", lines.join("\n"))
    };
    format!("<path>{path}</path>\n<type>file</type>\n<content>\n{body}\n</content>")
}

/// Stats a read target: absent targets are observed as absent and refused.
pub(crate) fn regular_target(state: &ToolState, target: &Target) -> Result<super::Info, ToolError> {
    match probe(&target.key)? {
        None => {
            state.observe(&target.key, Observation::Absent);
            Err(fail(format!(
                "cannot read \"{}\": not found",
                target.display
            )))
        }
        Some(info) if !info.is_file => Err(fail(format!(
            "cannot read \"{}\": not a regular file",
            target.display
        ))),
        Some(info) => Ok(info),
    }
}

/// The `read` tool.
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("read")
    }

    fn concurrency_safe(&self) -> bool {
        true
    }

    fn describe(&self, args: &Value) -> String {
        let path = args["file_path"].as_str().unwrap_or("");
        let offset = args["offset"].as_f64();
        let window = match (offset, args["limit"].as_f64()) {
            (_, Some(limit)) if limit > 0.0 => {
                let start = offset.unwrap_or(1.0);
                format!(
                    " ({} - {})",
                    crate::util::js_number(start),
                    crate::util::js_number(start + limit - 1.0)
                )
            }
            (Some(offset), _) => format!(" (from line {})", crate::util::js_number(offset)),
            _ => String::new(),
        };
        format!("Read {path}{window}")
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = args::parse("read", args)?;
        if args.file_path.trim().is_empty() {
            return Err(fail("file_path must be a non-empty string"));
        }
        let offset = args
            .offset
            .map(|v| positive_integer(v, "offset"))
            .transpose()?
            .unwrap_or(1);
        let limit = args
            .limit
            .map(|v| positive_integer(v, "limit"))
            .transpose()?
            .unwrap_or(READ_LIMIT);
        if limit > READ_LIMIT {
            return Err(fail(format!(
                "limit must be less than or equal to {READ_LIMIT}"
            )));
        }
        let state = ToolState::get(&ctx)?;
        let cwd = ctx.cwd.clone();
        let file_path = args.file_path.clone();
        let target = resolve(&cwd, &file_path)?;
        let info = regular_target(state, &target)?;
        let key = target.key.clone();
        let display = target.display.clone();
        let window =
            tokio::task::spawn_blocking(move || read_window(&key, &display, offset, limit))
                .await
                .map_err(|e| fail(e.to_string()))??;
        let path = target.process_path();
        let text = format_read_output(&path, offset, &window);
        state.observe(&target.key, Observation::Present(info.version));
        let lines: Vec<Value> = window
            .lines
            .iter()
            .map(|l| json!({"number": l.number, "text": l.text}))
            .collect();
        let mut meta = Map::new();
        meta.insert("path".into(), json!(path));
        meta.insert("offset".into(), json!(offset));
        meta.insert("lines".into(), Value::Array(lines.clone()));
        meta.insert("totalLines".into(), json!(window.total_lines));
        if let Some(lang) = lang_for_path(&path) {
            meta.insert("lang".into(), json!(lang));
        }
        let value = json!({"path": path, "offset": offset, "lines": lines, "totalLines": window.total_lines});
        Ok(ToolOutput {
            value: Some(value),
            meta: Some(Value::Object(meta)),
            ..ToolOutput::text(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(text: &str, offset: u64, limit: u64) -> Result<Window, ToolError> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), text).unwrap();
        read_window(tmp.path(), "/d", offset, limit)
    }

    #[test]
    fn counts_lines_like_dsh() {
        assert_eq!(window("", 1, 10).unwrap().total_lines, 0);
        assert_eq!(window("a\nb\n", 1, 10).unwrap().total_lines, 2);
        assert_eq!(window("a\nb", 1, 10).unwrap().total_lines, 2);
        assert_eq!(window("\n", 1, 10).unwrap().total_lines, 1);
        let w = window("\u{feff}x\r\ny", 1, 10).unwrap();
        assert_eq!(w.lines[0].text, "x");
        assert_eq!(
            window("a\n", 3, 10).unwrap_err().to_string(),
            "offset 3 is out of range for \"/d\" (1 lines)"
        );
        assert_eq!(
            window("a\0b", 1, 10).unwrap_err().to_string(),
            "cannot read \"/d\": binary file"
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"ok\n\xff\xfe").unwrap();
        assert_eq!(
            read_window(tmp.path(), "/d", 1, 5).unwrap_err().to_string(),
            "cannot read \"/d\": invalid UTF-8 text"
        );
    }

    #[test]
    fn footers() {
        let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let w = window(&text, 5, 4).unwrap();
        assert_eq!(
            format_read_output("/p", 5, &w),
            "<path>/p</path>\n<type>file</type>\n<content>\n5: line 5\n6: line 6\n7: line 7\n8: line 8\n\n(Showing lines 5-8 of 10. Use offset=9 to continue.)\n</content>"
        );
        let w = window("", 1, 10).unwrap();
        assert_eq!(
            format_read_output("/p", 1, &w),
            "<path>/p</path>\n<type>file</type>\n<content>\n(End of file - total 0 lines)\n</content>"
        );
    }

    #[test]
    fn long_lines_and_byte_cap() {
        let long = "x".repeat(2500);
        let emoji = "\u{1F600}".repeat(1500);
        let w = window(&emoji, 1, 10).unwrap();
        assert_eq!(
            w.lines[0].text,
            format!(
                "{}... (line truncated to 2000 chars)",
                "\u{1F600}".repeat(1000)
            )
        );
        let w = window(&long, 1, 10).unwrap();
        assert_eq!(
            w.lines[0].text,
            format!("{}... (line truncated to 2000 chars)", "x".repeat(2000))
        );
        let many: String = (0..100)
            .map(|_| format!("{}\n", "y".repeat(1000)))
            .collect();
        let w = window(&many, 1, 2000).unwrap();
        assert!(w.truncated_by_bytes);
        assert_eq!(w.lines.len(), 51);
        assert_eq!(w.total_lines, 100);
        assert!(
            format_read_output("/p", 1, &w)
                .contains("(Output capped. Showing lines 1-51. Use offset=52 to continue.)")
        );
    }
}
