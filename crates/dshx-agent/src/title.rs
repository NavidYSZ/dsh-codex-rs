//! Session titles: a deterministic first-prompt fallback plus one model title
//! request that runs concurrently with the first step (dsh `session-title`,
//! `session-title-llm`, `session-title-first-prompt-llm` 0.2.0-rc.2).
use std::sync::Arc;

use dshx_core::{ContentBlock, FinishReason, GenerateRequest, Message, MessageSource, Role, kinds};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::agent::Inner;
use crate::config::TitleConfig;
use crate::events::{event, message_json};
use crate::stream::BlockAssembler;

const PROVIDER_ID: &str = "session-title-first-prompt-llm";
const TITLE_LLM_REQUEST: &str = "session/title-llm-request";

/// Removes terminal escapes and invisible controls, collapses whitespace (dsh `cleanTitleText`).
fn clean(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    // OSC sequences, then CSI, then two-byte ESC sequences (one left-to-right pass each, like the regexes).
    while i < chars.len() {
        let c = chars[i];
        let osc = (c == '\u{1b}' && chars.get(i + 1) == Some(&']')) || c == '\u{9d}';
        if osc {
            i += if c == '\u{9d}' { 1 } else { 2 };
            while i < chars.len() {
                if chars[i] == '\u{7}' {
                    i += 1;
                    break;
                }
                if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    let chars = out;
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = if c == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            Some(i + 2)
        } else if c == '\u{9b}' {
            Some(i + 1)
        } else {
            None
        };
        if let Some(mut j) = start {
            while j < chars.len() && ('0'..='?').contains(&chars[j]) {
                j += 1;
            }
            while j < chars.len() && (' '..='/').contains(&chars[j]) {
                j += 1;
            }
            if j < chars.len() && ('@'..='~').contains(&chars[j]) {
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    let chars = out;
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1).is_some_and(|n| ('@'..='_').contains(n)) {
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    let filtered: String = out
        .into_iter()
        .filter(|c| {
            let u = *c as u32;
            let control = matches!(u, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f..=0x9f);
            let directional = matches!(u, 0x200b | 0x200e | 0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff);
            !control && !directional
        })
        .collect();
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_utf8(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_owned();
    }
    let mut out = String::new();
    for c in input.chars() {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// dsh `normalizeSessionTitle`.
pub(crate) fn normalize_title(input: &str, max_bytes: usize) -> String {
    truncate_utf8(&clean(input), max_bytes)
        .trim_end()
        .to_owned()
}

/// dsh `fallbackSessionTitle`: the leading words of the first prompt.
pub(crate) fn fallback_title(input: &str, max_words: usize, max_bytes: usize) -> String {
    let words: Vec<String> = clean(input)
        .split(' ')
        .filter(|w| !w.is_empty())
        .take(max_words)
        .map(str::to_owned)
        .collect();
    truncate_utf8(&words.join(" "), max_bytes)
        .trim_end()
        .to_owned()
}

fn system_prompt(cfg: &TitleConfig) -> String {
    [
        "Create a concise title for an AI coding-assistant session from the supplied human messages.".to_owned(),
        "Return only the title on one line, **in plain text of natural language**, with no quotes, prefix, explanation, Markdown, XML, or terminal control codes. No code is allowed.".to_owned(),
        "Use the language of the messages.".to_owned(),
        format!(
            "Aim for about {} words in non-CJK languages or {} CJK characters.",
            cfg.target_words, cfg.target_cjk_characters
        ),
    ]
    .join("\n")
}

/// Eligible human messages: `user` source with non-empty normalized text.
fn eligible(events: &[dshx_core::SessionEvent]) -> Vec<(u64, String)> {
    events
        .iter()
        .filter(|e| e.kind == kinds::USER_MESSAGE && e.data["source"]["kind"] == "user")
        .filter_map(|e| {
            let text: Vec<&str> = e.data["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            let text = text.join("\n");
            (!normalize_title(&text, usize::MAX).is_empty()).then_some((e.seq, text))
        })
        .collect()
}

/// Called after each request envelope is logged: writes the fallback title and
/// starts the model title request for a session's first prompt.
pub(crate) async fn after_request(inner: &Arc<Inner>, model: &str) {
    let cfg = inner.config.lock().unwrap().title.clone();
    if !cfg.enabled || inner.state.lock().unwrap().titled {
        return;
    }
    let (has_title, eligible) = inner.journal.read(|events, _| {
        (
            events.iter().any(|e| e.kind == kinds::SESSION_TITLE),
            eligible(events),
        )
    });
    let Some((seq, text)) = eligible.first().cloned() else {
        return;
    };
    if has_title {
        inner.state.lock().unwrap().titled = true;
        return;
    }
    inner.state.lock().unwrap().titled = true;
    let fallback = fallback_title(&text, cfg.fallback_max_words, cfg.fallback_max_bytes);
    if !fallback.is_empty() {
        let data = json!({"title": fallback, "messageSeqs": [seq], "source": {"kind": "fallback"}});
        if let Err(e) = inner.append(event(kinds::SESSION_TITLE, data)).await {
            tracing::warn!("fallback title update failed: {e:#}");
            return;
        }
    }
    let header = inner.journal.header();
    let child = header.delegation_depth > 0 || header.extra.contains_key("parentSession");
    if !cfg.llm || child || eligible.len() != 1 {
        return;
    }
    let framed = format!(
        "Generate the session title from this JSON array of human messages:\n{}",
        serde_json::to_string(&json!([{"seq": seq, "text": text}])).unwrap_or_default()
    );
    if framed.len() > cfg.max_input_bytes {
        tracing::warn!(
            "session title input is {} bytes, exceeding maxInputBytes {}",
            framed.len(),
            cfg.max_input_bytes
        );
        return;
    }
    let provider = inner.llm.provider().to_owned();
    let system = system_prompt(&cfg);
    let user = Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(framed)],
        source: MessageSource::Other(json!({"kind": "dsh-session-title-llm"})),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    };
    let data = json!({
        "titleProvider": PROVIDER_ID,
        "messageSeqs": [seq],
        "route": {"provider": provider, "model": model},
        "system": system,
        "messages": [message_json(&user)],
        "maxTokens": cfg.max_output_tokens,
    });
    if let Err(e) = inner.append(event(TITLE_LLM_REQUEST, data)).await {
        tracing::warn!("title request record failed: {e:#}");
        return;
    }
    let request = GenerateRequest {
        model: model.to_owned(),
        // dsh's adapter disables thinking for the title purpose.
        reasoning_effort: Some("off".into()),
        messages: vec![Message::system(system), user],
        tools: Vec::new(),
        max_tokens: Some(cfg.max_output_tokens),
        temperature: None,
        session_id: Some(inner.journal.id()),
    };
    let inner = inner.clone();
    let model = model.to_owned();
    tokio::spawn(async move {
        let token = inner.lifetime.child_token();
        let call = generate(&inner, request, token.clone());
        let title = tokio::select! {
            r = tokio::time::timeout(cfg.timeout, call) => match r {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!("session title request timed out")),
            },
            _ = token.cancelled() => return,
        };
        let title = match title {
            Ok(t) => normalize_title(&t, cfg.max_title_bytes),
            Err(e) => {
                tracing::warn!("automatic title generation failed: {e:#}");
                return;
            }
        };
        if title.is_empty() {
            return;
        }
        let user_titled = inner.journal.read(|events, _| {
            events
                .iter()
                .rev()
                .find(|e| e.kind == kinds::SESSION_TITLE)
                .is_some_and(|e| e.data["source"]["kind"] == "user")
        });
        if user_titled {
            return;
        }
        let data = json!({
            "title": title,
            "messageSeqs": [seq],
            "source": {"kind": "provider", "provider": PROVIDER_ID, "model": {"provider": provider, "model": model}},
        });
        if let Err(e) = inner.append(event(kinds::SESSION_TITLE, data)).await {
            tracing::warn!("title update failed: {e:#}");
        }
    });
}

async fn generate(
    inner: &Inner,
    request: GenerateRequest,
    token: tokio_util::sync::CancellationToken,
) -> anyhow::Result<String> {
    let mut stream = inner.llm.stream(request, token).await;
    let mut asm = BlockAssembler::default();
    while let Some(chunk) = stream.next().await {
        asm.push(&chunk);
    }
    match asm.finish() {
        FinishReason::Stop => {}
        FinishReason::Error { failure } | FinishReason::Aborted { failure } => {
            anyhow::bail!("{}", failure.message)
        }
        FinishReason::MaxTokens => {
            anyhow::bail!("session-title-llm: title output reached maxOutputTokens")
        }
        FinishReason::ToolCalls => {
            anyhow::bail!("session-title-llm: title model unexpectedly requested a tool")
        }
    }
    let blocks = asm.blocks();
    if blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
    {
        anyhow::bail!("session-title-llm: title output must contain text only");
    }
    let text: Vec<&str> = blocks
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    let title = normalize_title(&text.join(" "), usize::MAX);
    anyhow::ensure!(
        !title.is_empty(),
        "session-title-llm: title model produced no text"
    );
    Ok(title)
}

/// A user rename pins the title (`source: user`).
pub(crate) async fn rename(inner: &Inner, title: &str) -> anyhow::Result<()> {
    let max = inner.config.lock().unwrap().title.max_title_bytes;
    let normalized = normalize_title(title, max);
    anyhow::ensure!(!normalized.is_empty(), "session title must not be empty");
    let data: Value = json!({"title": normalized, "messageSeqs": [], "source": {"kind": "user"}});
    inner.append(event(kinds::SESSION_TITLE, data)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_and_normalize() {
        assert_eq!(
            fallback_title("Run two commands please", 5, 40),
            "Run two commands please"
        );
        assert_eq!(
            fallback_title("  one\ttwo\n three four five six seven", 5, 40),
            "one two three four five"
        );
        assert_eq!(fallback_title("ab cd", 5, 3), "ab");
        assert_eq!(
            normalize_title("\u{1b}[31mRed\u{1b}[0m  title\u{200b}", 80),
            "Red title"
        );
    }
}
