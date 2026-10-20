//! Incremental SSE framing (the `eventsource-parser` rules dsh uses) and
//! JSON event decoding (dsh `llm-deepseek/src/sse.ts`).
use dshx_core::LlmFailure;
use serde_json::{Map, Value};

/// One complete SSE frame or comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseItem {
    Event {
        event: Option<String>,
        data: String,
    },
    /// Heartbeat comments count as transport activity.
    Comment(String),
}

/// Byte-level SSE parser. Chunks may split anywhere, including inside a
/// UTF-8 sequence or a CRLF pair. An unterminated tail is never dispatched.
#[derive(Debug, Default)]
pub struct SseParser {
    pending: Vec<u8>,
    started: bool,
    event: String,
    data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes and returns every item they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseItem> {
        self.pending.extend_from_slice(bytes);
        if !self.started {
            if self.pending.len() < 3 && b"\xEF\xBB\xBF".starts_with(&self.pending) {
                return vec![];
            }
            if self.pending.starts_with(b"\xEF\xBB\xBF") {
                self.pending.drain(..3);
            }
            self.started = true;
        }
        let mut items = vec![];
        let mut start = 0;
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i] {
                b'\n' => {
                    let line = String::from_utf8_lossy(&self.pending[start..i]).into_owned();
                    self.line(&line, &mut items);
                    i += 1;
                    start = i;
                }
                b'\r' => {
                    // A CR at the end of the buffer may be the first half of CRLF.
                    if i + 1 == self.pending.len() {
                        break;
                    }
                    let line = String::from_utf8_lossy(&self.pending[start..i]).into_owned();
                    self.line(&line, &mut items);
                    i += if self.pending[i + 1] == b'\n' { 2 } else { 1 };
                    start = i;
                }
                _ => i += 1,
            }
        }
        self.pending.drain(..start);
        items
    }

    fn line(&mut self, line: &str, items: &mut Vec<SseItem>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                let mut data = std::mem::take(&mut self.data);
                if data.ends_with('\n') {
                    data.pop();
                }
                let event = std::mem::take(&mut self.event);
                items.push(SseItem::Event {
                    event: (!event.is_empty()).then_some(event),
                    data,
                });
            }
            self.event.clear();
            self.data.clear();
            return;
        }
        if let Some(comment) = line.strip_prefix(':') {
            items.push(SseItem::Comment(
                comment.strip_prefix(' ').unwrap_or(comment).to_string(),
            ));
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            // `id`, `retry` and unknown fields carry nothing the adapter uses.
            _ => {}
        }
    }
}

fn malformed(message: &str) -> LlmFailure {
    LlmFailure::new("MALFORMED_RESPONSE", message)
}

/// Decodes one SSE frame into its JSON event. In-band `error` events become
/// classified provider failures.
pub fn decode_event(event: Option<&str>, data: &str) -> Result<Map<String, Value>, LlmFailure> {
    let raw: Value = serde_json::from_str(data)
        .map_err(|_| malformed("DeepSeek Messages SSE contains invalid JSON"))?;
    let Value::Object(object) = raw else {
        return Err(malformed("DeepSeek Messages expected a JSON object"));
    };
    let kind = object.get("type").and_then(Value::as_str);
    if kind.is_none() || event.is_some_and(|e| Some(e) != kind) {
        return Err(malformed("DeepSeek Messages SSE event type mismatch"));
    }
    if kind == Some("error") {
        return Err(crate::transport::provider_error(
            Some(&Value::Object(object)),
            None,
            None,
        ));
    }
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &[u8], step: usize) -> Vec<SseItem> {
        let mut parser = SseParser::new();
        text.chunks(step.max(1))
            .flat_map(|chunk| parser.feed(chunk))
            .collect()
    }

    #[test]
    fn frames_survive_any_split_and_line_ending() {
        let text = "\u{FEFF}: heartbeat\r\n\r\nevent: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\ndata: {\"t\":\r\ndata: \"世界\"}\r\r\n";
        let expected = vec![
            SseItem::Comment("heartbeat".into()),
            SseItem::Event {
                event: Some("ping".into()),
                data: "{\"type\":\"ping\"}".into(),
            },
            SseItem::Event {
                event: None,
                data: "{\"t\":\n\"世界\"}".into(),
            },
        ];
        for step in [1, 2, 3, 5, 1000] {
            assert_eq!(events(text.as_bytes(), step), expected, "step {step}");
        }
    }

    #[test]
    fn unterminated_tail_is_not_dispatched() {
        assert!(events(b"data: {\"type\":\"message_stop\"}\n", 1).is_empty());
        assert!(events(b"event: x\n\n", 1).is_empty());
    }

    #[test]
    fn decoding_checks_type_and_in_band_errors() {
        assert!(decode_event(Some("ping"), r#"{"type":"ping"}"#).is_ok());
        for (event, data) in [
            (None, "not-json"),
            (None, "[]"),
            (Some("ping"), r#"{"type":"other"}"#),
            (None, "{}"),
        ] {
            assert_eq!(
                decode_event(event, data).unwrap_err().code,
                "MALFORMED_RESPONSE",
                "{data}"
            );
        }
        let error = decode_event(
            None,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
        )
        .unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            ("SERVER", "busy")
        );
    }
}
