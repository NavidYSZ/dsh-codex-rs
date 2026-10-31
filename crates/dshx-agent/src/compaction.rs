//! Pressure compaction, overflow recovery, tool-result pruning (dsh
//! `compaction-basic`, `compaction-tool-result-pruner`, `token-meter`) and
//! dsh-codex's compaction after a completed turn.
use std::sync::Arc;

use dshx_core::{
    ContentBlock, FinishReason, GenerateRequest, Message, MessageSource, Role, SessionEvent,
    ToolSchema, kinds,
};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::TurnEndCompaction;
use crate::agent::Inner;
use crate::events::{Aborted, LlmError, error_chain, event, message_json, surface_replace};
use crate::journal::{event_message, is_surface_type};
use crate::stream::BlockAssembler;

const CHARS_PER_TOKEN: usize = 4;
const BLOCK_OVERHEAD: u64 = 4;
const ROLE_OVERHEAD: u64 = 4;
const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";
const OPEN_TAG: &str = "<compacted-summary>";
const CLOSE_TAG: &str = "</compacted-summary>";
const CHECKPOINT_PREAMBLE: &str = "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.";
const INSTRUCTION: &str = "You are now acting as a compaction engine for this AI coding assistant. Condense the conversation ABOVE into a structured checkpoint that lets another model resume the work with no loss of essential context.

Output EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write \"(none)\" for an empty section — never drop a section.

## Primary Request and Intent
- [the user's original and evolving goals; quote verbatim where the exact wording matters]

## Key Technical Concepts
- [technologies, frameworks, patterns, and conventions in play]

## Files and Code
- [exact path: why it matters, key changes or snippets]

## Errors and Fixes
- [error: how it was resolved, plus any related user feedback]

## Pending Jobs
- [explicitly requested work not yet completed]

## Current Work
- [precisely what was in progress at this checkpoint]

## Next Step
- [the single next action, directly in line with the most recent request, or \"(none)\"]

## Critical Context
- [decisions and their rationale, constraints, user preferences, open questions, data needed to continue]

Rules:
- Write concise English engineering prose. Preserve exact file paths, commands, error strings, identifiers, numeric values, function signatures, and syntax fragments.
- Capture user feedback and explicit instructions faithfully, especially corrections.
- Do NOT mention this summarization request or that the context was compacted.
- Output only the checkpoint text: do not call any tool or take any other action.
- If the conversation already contains a <compacted-summary> block, it is a PRIOR checkpoint. Do not copy it forward verbatim: preserve still-true facts, drop stale ones, and merge newer information into a single consolidated summary under the same structure.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    Pressure,
    ContextOverflow,
}

/// JS `string.length` (UTF-16 code units).
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn ceil_div(n: usize) -> u64 {
    n.div_ceil(CHARS_PER_TOKEN) as u64
}

fn structural(block: &ContentBlock) -> u64 {
    let v = match block {
        ContentBlock::Image { attachment, .. } => {
            json!({"type": "image", "attachment": attachment})
        }
        other => serde_json::to_value(other).unwrap_or_default(),
    };
    BLOCK_OVERHEAD + ceil_div(js_len(&v.to_string()))
}

fn estimate_content(blocks: &[ContentBlock]) -> u64 {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                ceil_div(js_len(text)) + BLOCK_OVERHEAD
            }
            ContentBlock::ToolCall {
                name, arguments, ..
            } => ceil_div(js_len(name)) + ceil_div(js_len(arguments)) + BLOCK_OVERHEAD,
            other => structural(other),
        })
        .sum()
}

/// dsh `estimateMessage`: fixed four-characters-per-token heuristic.
pub(crate) fn estimate_message(m: &Message) -> u64 {
    if m.role == Role::System {
        if m.content.is_empty() {
            return 0;
        }
        let chars: usize = m
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => js_len(text),
                other => js_len(&serde_json::to_string(other).unwrap_or_default()),
            })
            .sum();
        return ceil_div(chars) + ROLE_OVERHEAD;
    }
    estimate_content(&m.content) + ROLE_OVERHEAD
}

fn tools_tokens(header: Option<&Value>) -> u64 {
    match header
        .and_then(|h| h.get("tools"))
        .and_then(Value::as_array)
    {
        Some(t) if !t.is_empty() => {
            ceil_div(js_len(&Value::Array(t.clone()).to_string())) + BLOCK_OVERHEAD
        }
        _ => 0,
    }
}

/// Pressure measurement over the current surface (dsh `TokenMeter.measure`).
pub(crate) struct Measurement {
    pub total_tokens: u64,
    /// `(seq, tokens)` per current surface node.
    pub nodes: Vec<(u64, u64)>,
}

fn node_tokens(event: &SessionEvent) -> u64 {
    event_message(event)
        .map(|m| estimate_message(&m))
        .unwrap_or(0)
}

struct Anchor {
    header: Option<Value>,
    nodes_tokens: u64,
    assistant_tokens: u64,
    usage: Option<u64>,
}

pub(crate) fn measure(events: &[SessionEvent]) -> Measurement {
    let mut header: Option<Value> = None;
    let mut priced: Vec<(u64, u64)> = Vec::new();
    let mut sum: u64 = 0;
    let mut anchor: Option<Anchor> = None;
    for e in events {
        if e.kind == kinds::REQUEST_HEADER {
            header = e.data.get("header").cloned();
            continue;
        }
        if !is_surface_type(&e.kind) {
            continue;
        }
        let tokens = node_tokens(e);
        if e.kind == kinds::ASSISTANT_MESSAGE {
            let usage = e.data.get("usage").filter(|_| header.is_some()).map(|u| {
                [
                    "inputTokens",
                    "cacheReadTokens",
                    "cacheWriteTokens",
                    "outputTokens",
                ]
                .iter()
                .map(|k| u.get(*k).and_then(Value::as_u64).unwrap_or(0))
                .sum::<u64>()
            });
            anchor = Some(Anchor {
                header: header.clone(),
                nodes_tokens: sum,
                assistant_tokens: tokens,
                usage,
            });
        }
        match e.extra.get("surfaceOp") {
            Some(Value::String(op)) if op == "append" => {
                priced.push((e.seq, tokens));
                sum += tokens;
            }
            Some(Value::Object(op)) => {
                let find = |k: &str| {
                    let seq = op.get(k).and_then(Value::as_u64);
                    priced.iter().position(|(s, _)| Some(*s) == seq)
                };
                if let (Some(si), Some(ei)) = (find("startSeq"), find("endSeq"))
                    && si <= ei
                {
                    let removed: u64 = priced[si..=ei].iter().map(|(_, t)| t).sum();
                    priced.splice(si..=ei, [(e.seq, tokens)]);
                    sum = sum - removed + tokens;
                }
            }
            _ => {}
        }
    }
    let surface_tokens: u64 = priced.iter().map(|(_, t)| t).sum();
    let total = match &anchor {
        Some(a) if a.header == header => {
            let anchor_surface = a.nodes_tokens + a.assistant_tokens;
            let estimated = tools_tokens(header.as_ref()) + anchor_surface;
            let base = match a.usage {
                Some(u) if u >= estimated => u,
                _ => estimated,
            };
            (base as i64 + surface_tokens as i64 - anchor_surface as i64).max(0) as u64
        }
        _ if header.is_none() && surface_tokens == 0 => 0,
        _ => tools_tokens(header.as_ref()) + surface_tokens,
    };
    Measurement {
        total_tokens: total,
        nodes: priced,
    }
}

/// Whether no tool call is open across the cut before node `idx`.
fn balanced_before(events: &[SessionEvent], nodes: &[u64], idx: usize) -> bool {
    let mut open: i64 = 0;
    for seq in &nodes[..idx] {
        let Some(e) = events.get(*seq as usize) else {
            continue;
        };
        match e.kind.as_str() {
            kinds::ASSISTANT_MESSAGE => {
                open += e.data["message"]["content"]
                    .as_array()
                    .map(|c| c.iter().filter(|b| b["type"] == "tool-call").count() as i64)
                    .unwrap_or(0)
            }
            kinds::TOOL_RESULT => open -= 1,
            _ => {}
        }
    }
    open == 0
}

/// dsh `selectCompactableRange`.
fn select_range(events: &[SessionEvent], m: &Measurement, retain: u64) -> Option<(u64, u64)> {
    let nodes: Vec<u64> = m.nodes.iter().map(|(s, _)| *s).collect();
    if nodes.is_empty() {
        return None;
    }
    let first_idx = usize::from(
        events
            .get(nodes[0] as usize)
            .is_some_and(|e| e.kind == kinds::SYSTEM_MESSAGE),
    );
    let mut acc = 0;
    let mut keep_from = nodes.len();
    for i in (0..nodes.len()).rev() {
        acc += m.nodes[i].1;
        keep_from = i;
        if acc >= retain {
            break;
        }
    }
    if keep_from <= first_idx {
        return None;
    }
    while keep_from > first_idx {
        if balanced_before(events, &nodes, keep_from) {
            break;
        }
        keep_from -= 1;
    }
    if keep_from <= first_idx {
        return None;
    }
    Some((nodes[first_idx], nodes[keep_from - 1]))
}

/// Text Unicode code points of tool-result content.
fn code_points(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|b| {
            if let ContentBlock::Text { text } = b {
                text.chars().count()
            } else {
                0
            }
        })
        .sum()
}

fn prune_content(
    blocks: &[ContentBlock],
    threshold: usize,
    head: usize,
    tail: usize,
) -> Option<Vec<ContentBlock>> {
    let total = code_points(blocks);
    if total <= threshold {
        return None;
    }
    let removed_start = head;
    let removed_end = total.saturating_sub(tail);
    let mut out = Vec::new();
    let mut consumed = 0;
    let mut inserted = false;
    for b in blocks {
        let ContentBlock::Text { text } = b else {
            out.push(b.clone());
            continue;
        };
        let points: Vec<char> = text.chars().collect();
        let start = consumed;
        let end = start + points.len();
        let head_end = points.len().min(removed_start.saturating_sub(start));
        let tail_start = points.len().min(removed_end.saturating_sub(start));
        let intersects = start < removed_end && end > removed_start;
        let marker = if intersects && !inserted {
            inserted = true;
            PRUNE_MARKER
        } else {
            ""
        };
        let mut s: String = points[..head_end].iter().collect();
        s.push_str(marker);
        s.extend(points[tail_start..].iter());
        if !s.is_empty() {
            out.push(ContentBlock::Text { text: s });
        }
        consumed = end;
    }
    inserted.then_some(out)
}

/// Prunes every over-budget tool result on the current surface.
async fn prune_session(inner: &Arc<Inner>) -> anyhow::Result<usize> {
    let cfg = inner.config.lock().unwrap().pruner.clone();
    if !cfg.enabled {
        return Ok(0);
    }
    // Offloaded images stay offloaded in the pruned replacement.
    let candidates: Vec<(SessionEvent, Message)> = inner.journal.read(|events, surface| {
        surface
            .nodes
            .iter()
            .filter_map(|s| events.get(*s as usize))
            .filter(|e| e.kind == kinds::TOOL_RESULT)
            .filter_map(|e| Some((e.clone(), surface.message(e)?)))
            .collect()
    });
    let mut pruned = 0;
    for (e, original) in candidates {
        let Some(content) = prune_content(
            &original.content,
            cfg.threshold_chars,
            cfg.head_chars,
            cfg.tail_chars,
        ) else {
            continue;
        };
        inner
            .append(event(
                kinds::COMPACTION_PRUNE,
                json!({
                    "shadowedRange": {"start": e.seq, "end": e.seq},
                    "shadowedSeqs": [e.seq],
                    "shadowedTokenCount": estimate_message(&original),
                }),
            ))
            .await?;
        let mut data = e.data.clone();
        let mut message = original.clone();
        message.content = content;
        data["message"] = message_json(&message);
        inner
            .append(surface_replace(
                kinds::TOOL_RESULT,
                data,
                e.seq,
                e.seq,
                vec![e.seq],
            ))
            .await?;
        pruned += 1;
    }
    Ok(pruned)
}

/// Threshold and retained tail for the routed model (dsh `resolveCompactSpec`).
fn spec(inner: &Inner) -> anyhow::Result<(u64, u64)> {
    let window = inner.context_window().ok_or_else(|| {
        anyhow::anyhow!("compaction-basic: no context capacity for the routed model")
    })?;
    let (cfg, default_max) = {
        let c = inner.config.lock().unwrap();
        (c.compaction.clone(), c.route().default_max_tokens)
    };
    let reserved = inner
        .journal
        .request_header()
        .and_then(|h| h["config"]["maxTokens"].as_u64())
        .unwrap_or(default_max as u64);
    let budget = window
        .checked_sub(reserved)
        .filter(|b| *b > 0)
        .ok_or_else(|| anyhow::anyhow!("compaction-basic: no message budget"))?;
    let pressure = budget
        .checked_sub(cfg.headroom_tokens)
        .filter(|b| *b > 0)
        .ok_or_else(|| anyhow::anyhow!("compaction-basic: no pressure budget"))?;
    let threshold = ((window as f64 * cfg.threshold_ratio).min(pressure as f64)).floor() as u64;
    let retain = cfg
        .retain_tokens
        .unwrap_or_else(|| (budget as f64 * cfg.retain_ratio).floor() as u64);
    anyhow::ensure!(
        retain < threshold,
        "compaction-basic: retainTokens ({retain}) must be less than threshold tokens {threshold}"
    );
    Ok((threshold, retain))
}

/// dsh `compactIfNeeded`. Returns whether a summary replaced history.
pub(crate) async fn compact_if_needed(
    inner: &Arc<Inner>,
    trigger: Trigger,
    turn: u64,
    cancel: &CancellationToken,
) -> anyhow::Result<bool> {
    if inner.journal.request_header().is_none() {
        return Ok(false);
    }
    if compaction_open(inner) {
        anyhow::bail!(
            "compaction already in progress; the session compaction lock is already active"
        );
    }
    let measure_now = || inner.journal.read(|events, _| measure(events));
    if trigger == Trigger::ContextOverflow {
        prune_session(inner).await?;
        let m = measure_now();
        let Some(range) = inner.journal.read(|events, _| select_range(events, &m, 0)) else {
            return Ok(false);
        };
        compact_region(inner, range, Some(turn), cancel).await?;
        return Ok(true);
    }
    let (threshold, retain) = spec(inner)?;
    let mut m = measure_now();
    if m.total_tokens < threshold {
        return Ok(false);
    }
    let retries = inner.config.lock().unwrap().compaction.compaction_retries;
    let mut done = false;
    for _ in 0..=retries {
        // dsh-codex: summarize before pruning. The summarizer replays the
        // prefix the provider still caches; pruning first rewrites it from
        // the first oversized result on, usually near the start. Only the
        // retained tail is pruned afterwards, and it is re-read anyway.
        let boundary_attempt = inner.state.lock().unwrap().last_compaction_attempt;
        let range = inner.journal.read(|events, _| {
            select_range(events, &m, retain).filter(|range| {
                boundary_attempt != Some(*range)
                    && (boundary_attempt.is_none() || new_tokens(events, &m, *range) > 0)
            })
        });
        if let Some(range) = range {
            compact_region(inner, range, Some(turn), cancel).await?;
            done = true;
        }
        prune_session(inner).await?;
        m = measure_now();
        if m.total_tokens < threshold || range.is_none() {
            return Ok(done);
        }
    }
    anyhow::bail!(
        "compaction still above threshold after {} compaction attempts ({} estimated tokens >= threshold {threshold})",
        retries + 1,
        m.total_tokens
    )
}

/// New history an every-turn compaction needs, in estimated tokens beside
/// earlier checkpoints: less is not worth a summary call.
const MIN_EVERY_TURN_TOKENS: u64 = 8_000;

/// Completed PTC answers and Goal rounds can benefit from shorter checkpoints.
const MIN_SHORT_TURN_TOKENS: u64 = 1_024;

/// Condenses history only after the entire turn (`turn: null`). The latest
/// answer stays verbatim; intermediate tool results remain available until
/// that answer is complete. Pressure/overflow compaction is a separate path.
pub(crate) async fn compact_after_turn(
    inner: &Arc<Inner>,
    policy: TurnEndCompaction,
    short_turns: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<bool> {
    let turn_open = inner.state.lock().unwrap().open_turn.is_some();
    if turn_open || inner.journal.request_header().is_none() || compaction_open(inner) {
        return Ok(false);
    }
    let range = inner
        .journal
        .read(|events, _| {
            let m = measure(events);
            let retain = match policy {
                TurnEndCompaction::Off => return None,
                TurnEndCompaction::Threshold(tokens) if m.total_tokens < tokens => return None,
                TurnEndCompaction::Threshold(tokens) => tokens / 5,
                TurnEndCompaction::EveryTurn => 0,
            };
            let range = select_range(events, &m, retain)?;
            let fresh = new_tokens(events, &m, range);
            let minimum = if short_turns {
                MIN_SHORT_TURN_TOKENS
            } else {
                MIN_EVERY_TURN_TOKENS
            };
            let worth = (policy != TurnEndCompaction::EveryTurn || fresh >= minimum)
                && (!short_turns || fresh > 0);
            worth.then_some(range)
        })
        .filter(|range| {
            !short_turns || inner.state.lock().unwrap().last_compaction_attempt != Some(*range)
        });
    let Some(range) = range else {
        return Ok(false);
    };
    if short_turns {
        inner.state.lock().unwrap().last_compaction_attempt = Some(range);
    }
    compact_region(inner, range, None, cancel).await?;
    prune_session(inner).await?;
    Ok(true)
}

/// Estimated tokens of the surface span `(start, end)` without checkpoints.
fn new_tokens(events: &[SessionEvent], m: &Measurement, (start, end): (u64, u64)) -> u64 {
    let at = |seq: u64| m.nodes.iter().position(|(s, _)| *s == seq);
    let (Some(si), Some(ei)) = (at(start), at(end)) else {
        return 0;
    };
    m.nodes[si..=ei]
        .iter()
        .filter(|(seq, _)| {
            !events
                .get(*seq as usize)
                .is_some_and(|e| e.data["source"]["kind"] == "compact-checkpoint")
        })
        .map(|(_, tokens)| tokens)
        .sum()
}

fn compaction_open(inner: &Inner) -> bool {
    inner.journal.read(|events, _| {
        events
            .iter()
            .rev()
            .find(|e| e.kind == kinds::COMPACTION_START || e.kind == kinds::COMPACTION_END)
            .is_some_and(|e| e.kind == kinds::COMPACTION_START)
    })
}

/// What a manual compaction replaced (dsh `/compact`: "Compacted N history
/// items (~T tokens).").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compacted {
    /// Surface items the summary replaced.
    pub items: usize,
    /// Their estimated tokens.
    pub tokens: u64,
}

/// dsh `compactNow`: condenses the oldest balanced span even below the
/// pressure threshold, keeping only the newest item, in a bracket of its
/// own (`turn: null`). `None` when nothing can be compacted yet. The caller
/// keeps turns out meanwhile.
pub(crate) async fn compact_now(
    inner: &Arc<Inner>,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<Compacted>> {
    if inner.journal.request_header().is_none() {
        return Ok(None);
    }
    anyhow::ensure!(
        !compaction_open(inner),
        "Compaction is unavailable because this process has an active compaction, or the agent is not idle."
    );
    let range = inner.journal.read(|events, _| {
        let m = measure(events);
        select_range(events, &m, 0)
    });
    let Some(range) = range else {
        return Ok(None);
    };
    compact_region(inner, range, None, cancel).await?;
    Ok(inner.journal.read(|events, _| {
        let summary = events
            .iter()
            .rev()
            .find(|e| e.kind == kinds::COMPACTION_SUMMARY)?;
        Some(Compacted {
            items: summary.data["shadowedSeqs"].as_array().map_or(0, Vec::len),
            tokens: summary.data["shadowedTokenCount"].as_u64().unwrap_or(0),
        })
    }))
}

/// One bracketed compaction transaction over `range` (dsh
/// `compactSurfaceRegion`); `turn` is `None` for a manual compaction
/// between turns.
async fn compact_region(
    inner: &Arc<Inner>,
    range: (u64, u64),
    turn: Option<u64>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let id = dshx_core::new_id();
    let lifecycle = json!({"compactionId": id, "turn": turn});
    let start_seq = inner
        .append(event(kinds::COMPACTION_START, lifecycle.clone()))
        .await?
        .seq;
    match summarize_and_commit(inner, range, start_seq, &id, cancel).await {
        Ok(()) => {
            inner
                .append(event(kinds::COMPACTION_END, lifecycle))
                .await?;
            Ok(())
        }
        Err(e) => {
            let mut end = lifecycle.as_object().cloned().unwrap_or_default();
            end.insert("error".into(), error_chain(&e).into());
            inner
                .append(event(kinds::COMPACTION_END, Value::Object(end)))
                .await?;
            Err(e)
        }
    }
}

/// The system prompt plus the region to summarize, offloaded images marked.
fn summary_input(inner: &Inner, shadowed: &[u64]) -> Vec<Message> {
    inner.journal.read(|events, surface| {
        let head = surface
            .nodes
            .first()
            .and_then(|s| events.get(*s as usize))
            .filter(|e| e.kind == kinds::SYSTEM_MESSAGE);
        head.into_iter()
            .chain(shadowed.iter().filter_map(|s| events.get(*s as usize)))
            .filter_map(|e| surface.message(e))
            .collect()
    })
}

async fn summarize_and_commit(
    inner: &Arc<Inner>,
    (start, end): (u64, u64),
    start_seq: u64,
    id: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let (shadowed, shadowed_tokens) = inner.journal.read(|events, surface| {
        let si = surface.nodes.iter().position(|s| *s == start).unwrap_or(0);
        let ei = surface.nodes.iter().position(|s| *s == end).unwrap_or(si);
        let shadowed: Vec<u64> = surface.nodes[si..=ei].to_vec();
        let tokens: u64 = shadowed
            .iter()
            .filter_map(|s| events.get(*s as usize))
            .map(node_tokens)
            .sum();
        (shadowed, tokens)
    });
    let header = inner.journal.request_header();
    let tools: Vec<ToolSchema> = header
        .as_ref()
        .and_then(|h| h.get("tools").cloned())
        .and_then(|t| serde_json::from_value(t).ok())
        .unwrap_or_default();
    let (configured_model, max_tokens) = {
        let c = inner.config.lock().unwrap();
        (
            c.compaction.summarization_model.clone(),
            c.compaction
                .max_tokens
                .unwrap_or(c.compaction.headroom_tokens.min(u32::MAX as u64) as u32),
        )
    };
    // The summarizer runs on the route of the model it names: the last
    // request's route, unless a summarization model is configured.
    let current = || {
        let c = inner.config.lock().unwrap();
        (c.model.clone(), c.provider.clone())
    };
    // dsh-codex: on the conversation's own model the summarizer also takes
    // its reasoning effort. DeepSeek's prompt prefix depends on it, so a
    // different effort (dsh sends none, i.e. the default) misses the cache.
    let effort = match configured_model {
        Some(_) => None,
        None => header
            .as_ref()
            .and_then(|h| h["config"]["reasoningEffort"].as_str())
            .map(str::to_owned),
    };
    let (model, route) = match configured_model {
        Some(model) => (model, current().1),
        None => header
            .as_ref()
            .and_then(|h| {
                let model = h["config"]["model"].as_str()?.to_owned();
                Some((model, h["config"]["provider"].as_str().map(str::to_owned)))
            })
            .unwrap_or_else(current),
    };
    let llm = inner.client_for(route.as_deref());
    let provider = llm.provider().to_owned();
    let (raw, usage) = loop {
        let mut messages = summary_input(inner, &shadowed);
        messages.push(Message::user_text(INSTRUCTION));
        let request = GenerateRequest {
            model: model.clone(),
            reasoning_effort: effort.clone(),
            messages,
            tools: tools.clone(),
            max_tokens: Some(max_tokens),
            temperature: None,
            session_id: Some(inner.journal.id()),
        };
        let mut stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Aborted.into()),
            stream = llm.stream(request, cancel.child_token()) => stream,
        };
        let mut asm = BlockAssembler::default();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Aborted.into()),
                chunk = stream.next() => match chunk {
                    Some(c) => asm.push(&c),
                    None => break,
                }
            }
        }
        match asm.finish() {
            FinishReason::Error { failure } | FinishReason::Aborted { failure } => {
                // dsh `compaction/summary-error`: offload within the region, then retry.
                if let Some(count) = crate::offload::required(&failure)
                    && let Some(decision) = crate::offload::select(&inner.journal, &shadowed, count)
                {
                    inner.append(decision).await?;
                    continue;
                }
                return Err(LlmError(failure).into());
            }
            FinishReason::MaxTokens => {
                anyhow::bail!("summarization truncated at the token cap (incomplete checkpoint)")
            }
            _ => break (asm.blocks(), asm.usage().cloned()),
        }
    };
    if raw.iter().any(|b| matches!(b, ContentBlock::Image { .. })) {
        return Err(LlmError(dshx_core::LlmFailure::new(
            "UNSUPPORTED_CONTENT",
            "compaction summary cannot contain image output",
        ))
        .into());
    }
    let summary: Vec<ContentBlock> = raw
        .iter()
        .filter(|b| matches!(b, ContentBlock::Text { .. }))
        .cloned()
        .collect();
    if !summary
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if !text.trim().is_empty()))
    {
        anyhow::bail!("summarization produced no text summary content");
    }
    let mut framed = vec![ContentBlock::text(format!(
        "{CHECKPOINT_PREAMBLE}\n\n{OPEN_TAG}"
    ))];
    framed.extend(summary.iter().cloned());
    framed.push(ContentBlock::text(CLOSE_TAG));
    if let Some(todos) = shadowed_todos(inner, &shadowed) {
        framed.push(ContentBlock::text(todos));
    }
    let checkpoint = Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: framed,
        source: MessageSource::Other(json!({"kind": "compact-checkpoint", "compactionId": id})),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    };
    let framed_tokens = estimate_message(&checkpoint);
    anyhow::ensure!(
        framed_tokens < shadowed_tokens,
        "summary is not smaller than the shadowed content ({framed_tokens} estimated framed tokens >= {shadowed_tokens})"
    );
    if cancel.is_cancelled() {
        return Err(Aborted.into());
    }
    let mut data = Map::new();
    data.insert("compactionId".into(), id.into());
    data.insert("summary".into(), serde_json::to_value(&summary)?);
    data.insert("rawOutput".into(), serde_json::to_value(&raw)?);
    data.insert("llmStreamCall".into(), true.into());
    data.insert("shadowedRange".into(), json!({"start": start, "end": end}));
    data.insert("shadowedSeqs".into(), json!(shadowed));
    data.insert("shadowedTokenCount".into(), shadowed_tokens.into());
    data.insert("provider".into(), provider.into());
    data.insert("model".into(), model.into());
    data.insert("maxTokens".into(), max_tokens.into());
    if let Some(u) = usage {
        data.insert("usage".into(), serde_json::to_value(u)?);
    }
    let summary_seq = inner
        .append(event(kinds::COMPACTION_SUMMARY, Value::Object(data)))
        .await?
        .seq;
    let mut sources = vec![start_seq, summary_seq];
    sources.extend(&shadowed);
    inner
        .append(surface_replace(
            kinds::USER_MESSAGE,
            message_json(&checkpoint),
            start,
            end,
            sources,
        ))
        .await?;
    Ok(())
}

/// dsh-codex: the latest todo list unless the model still sees it after
/// this compaction, so it keeps the exact list the app shows. A `todo/write`
/// event directly precedes its call's result. Lists without open items are
/// left out.
fn shadowed_todos(inner: &Inner, shadowed: &[u64]) -> Option<String> {
    inner.journal.read(|events, surface| {
        let latest = events.iter().rev().find(|e| e.kind == kinds::TODO_WRITE)?;
        let result = events[latest.seq as usize..]
            .iter()
            .find(|e| e.kind == kinds::TOOL_RESULT)?;
        let todos = latest.data["todos"].as_array()?;
        let visible = surface.nodes.contains(&result.seq) && !shadowed.contains(&result.seq);
        if visible || todos.iter().all(|t| t["status"] == "completed") {
            return None;
        }
        let items: Vec<String> = todos
            .iter()
            .map(|t| {
                format!(
                    "- [{}] {}",
                    t["status"].as_str().unwrap_or("pending"),
                    t["content"].as_str().unwrap_or_default()
                )
            })
            .collect();
        Some(format!(
            "\n\nYour current todo list, as last written with todo_write:\n{}",
            items.join("\n")
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_head_and_tail() {
        let text: String = (0..10_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = prune_content(&[ContentBlock::text(text.clone())], 8192, 4096, 1024).unwrap();
        let ContentBlock::Text { text: pruned } = &out[0] else {
            panic!()
        };
        assert!(pruned.starts_with(&text[..4096]));
        assert!(pruned.ends_with(&text[text.len() - 1024..]));
        assert!(pruned.contains("[... tool result middle pruned ...]"));
        assert!(prune_content(&[ContentBlock::text("short")], 8192, 4096, 1024).is_none());
    }

    #[test]
    fn estimates_like_dsh() {
        let m = Message::user_text("abcde");
        assert_eq!(estimate_message(&m), 2 + 4 + 4);
        assert_eq!(estimate_message(&Message::system("abcd")), 1 + 4);
    }
}
