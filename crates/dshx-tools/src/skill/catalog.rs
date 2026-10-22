//! Model-facing skill text (dsh `tool-skill` + `renderSkillContent`): the
//! session skill catalog, its replacement updates, the `<skill_content>`
//! block and user `/name` gestures.
//!
//! dsh contributes the catalog from an `agent/pre-step` listener as a
//! `user/message` with source `{kind: "skill-catalog", form: "catalog",
//! entries}`, appended after the runtime-context snapshot; user gestures add
//! one `skill-invocation` message per invoked skill after the catalog.
use std::path::Path;

use dshx_core::{ContentBlock, Message, MessageSource, Role, SessionEvent, kinds};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::discovery::{ResourceBase, SkillDefinition, SkillRegistry, SkillSnapshot, is_js_space};

/// One published catalog line (dsh `SkillCatalogSource.entries[i]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub name: String,
    /// Normalized and length-capped description, unescaped.
    pub description: String,
}

/// The `<skill_content>` block shared by the `skill` tool result and user
/// invocations (dsh `renderSkillContent`).
pub fn render_skill_content(skill: &SkillDefinition) -> String {
    let resource_hint: [String; 2] = match &skill.summary.resource_base {
        ResourceBase::Directory(path) => [
            format!(
                "Base directory for this skill: {}",
                escape_text(&path.to_string_lossy())
            ),
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.".into(),
        ],
        ResourceBase::Url(url) => [
            format!("Base URL for this skill: {}", escape_text(url)),
            "Resolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed.".into(),
        ],
        ResourceBase::Opaque(description) => [
            format!("Resources for this skill: {}", escape_text(description)),
            "Load referenced resources only as needed.".into(),
        ],
    };
    [
        format!(
            "<skill_content name=\"{}\">",
            escape_attr(&skill.summary.name)
        ),
        "<skill_resources>".into(),
        resource_hint[0].clone(),
        resource_hint[1].clone(),
        "</skill_resources>".into(),
        String::new(),
        "<skill_instructions>".into(),
        skill.content.clone(),
        "</skill_instructions>".into(),
        "</skill_content>".into(),
    ]
    .join("\n")
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// dsh `escapeText`.
pub fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Whitespace runs collapsed, trimmed, capped at `max` UTF-16 units with `...`.
fn catalog_description(value: &str, max: usize) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_space = false;
    for c in value.chars() {
        if is_js_space(c) {
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.push(c);
        }
    }
    if normalized.encode_utf16().count() <= max {
        return normalized;
    }
    let budget = max.saturating_sub(3);
    let mut units = 0;
    let mut out = String::new();
    for c in normalized.chars() {
        units += c.len_utf16();
        if units > budget {
            break;
        }
        out.push(c);
    }
    out.push_str("...");
    out
}

/// Catalog entries of the model-invocable skills, in catalog (name) order.
pub fn catalog_entries(snapshot: &SkillSnapshot, max_description: usize) -> Vec<CatalogEntry> {
    snapshot
        .skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(|s| CatalogEntry {
            name: s.name.clone(),
            description: catalog_description(&s.description, max_description),
        })
        .collect()
}

fn entry_lines(entries: &[CatalogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| format!("- `{}`: {}", e.name, escape_text(&e.description)))
        .collect()
}

fn entries_json(entries: &[CatalogEntry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|e| json!({"name": e.name, "description": e.description}))
            .collect(),
    )
}

fn user_message(text: String, source: Value) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(text)],
        source: MessageSource::Other(source),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// The first catalog of a session (`update == false`) or a replacement.
pub fn catalog_message(entries: &[CatalogEntry], update: bool) -> Message {
    let mut lines: Vec<String> = vec!["<system-reminder>".into()];
    if update {
        lines.push("The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:".into());
    } else {
        lines.push("A skill is a reusable set of task-specific instructions. The following skills are available in this session:".into());
    }
    lines.push(String::new());
    lines.push("<available_skills>".into());
    lines.extend(entry_lines(entries));
    lines.push("</available_skills>".into());
    lines.push(String::new());
    if !update {
        lines.push("If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.".into());
        lines.push("A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.".into());
    } else if entries.is_empty() {
        lines.push("No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.".into());
        lines.push("A user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it.".into());
    } else {
        lines.push("Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting.".into());
        lines.push("A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.".into());
    }
    lines.push("</system-reminder>".into());
    let source = if update {
        json!({"kind": "skill-catalog", "form": "catalog", "update": true, "entries": entries_json(entries)})
    } else {
        json!({"kind": "skill-catalog", "form": "catalog", "entries": entries_json(entries)})
    };
    user_message(lines.join("\n"), source)
}

/// Identity of an entry list (dsh `digestCatalogEntries`).
fn digest(entries: &[CatalogEntry]) -> String {
    let canonical = entries
        .iter()
        .map(|e| serde_json::to_string(&json!([e.name, e.description])).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Entries of a durable catalog source; `None` when unreadable.
fn read_entries(source: &Value) -> Option<Vec<CatalogEntry>> {
    let mut out = Vec::new();
    for entry in source.get("entries")?.as_array()? {
        let name = entry.get("name")?.as_str()?;
        let description = entry.get("description")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        out.push(CatalogEntry {
            name: name.to_owned(),
            description: description.to_owned(),
        });
    }
    Some(out)
}

fn source_json(message: &Message) -> Option<Value> {
    match &message.source {
        MessageSource::Other(v)
            if v.get("kind").and_then(Value::as_str) == Some("skill-catalog") =>
        {
            Some(v.clone())
        }
        _ => None,
    }
}

/// What the session log says about earlier catalogs (dsh `catalogHistory`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogHistory {
    /// Digest of the newest catalog still on the model-visible surface.
    pub visible_digest: Option<String>,
    /// Whether any catalog was ever published in this session.
    pub published: bool,
}

impl CatalogHistory {
    /// Scans `user/message` events newest first; `visible(seq)` tells whether
    /// an event is still on the surface (not compacted away or replaced).
    pub fn from_events(events: &[SessionEvent], visible: impl Fn(u64) -> bool) -> Self {
        let mut published = false;
        for event in events.iter().rev() {
            if event.kind != kinds::USER_MESSAGE || event.data["source"]["kind"] != "skill-catalog"
            {
                continue;
            }
            let Some(entries) = read_entries(&event.data["source"]) else {
                continue;
            };
            published = true;
            if visible(event.seq) {
                return CatalogHistory {
                    visible_digest: Some(digest(&entries)),
                    published,
                };
            }
        }
        CatalogHistory {
            visible_digest: None,
            published,
        }
    }
}

/// The catalog listener of dsh `tool-skill`: given the step's entering batch
/// (`messages`, already holding claimed input, instructions and the runtime
/// context), returns it with the catalog appended, replaced or removed.
///
/// `snapshot` is `None` when the `skill` tool is not offered to the model;
/// the catalog then shrinks to empty like dsh's visibility check.
pub fn apply_catalog(
    mut messages: Vec<Message>,
    snapshot: Option<&SkillSnapshot>,
    max_description: usize,
    history: &CatalogHistory,
) -> Vec<Message> {
    let empty = SkillSnapshot {
        skills: Vec::new(),
        complete: true,
    };
    let snapshot = snapshot.unwrap_or(&empty);
    if !snapshot.complete {
        return messages;
    }
    let entries = catalog_entries(snapshot, max_description);
    let wanted = digest(&entries);
    let existing = messages.iter().enumerate().find_map(|(i, m)| {
        source_json(m)
            .and_then(|s| read_entries(&s))
            .map(|e| (i, digest(&e)))
    });
    let remove_existing = |mut messages: Vec<Message>| {
        if let Some((i, _)) = existing {
            messages.remove(i);
        }
        messages
    };
    if history.visible_digest.as_deref() == Some(wanted.as_str()) {
        return remove_existing(messages);
    }
    if existing.as_ref().is_some_and(|(_, d)| *d == wanted) {
        return messages;
    }
    if !history.published && entries.is_empty() {
        return remove_existing(messages);
    }
    let catalog = catalog_message(&entries, history.published);
    match existing {
        Some((i, _)) => messages[i] = catalog,
        None => messages.push(catalog),
    }
    messages
}

/// `/name` gesture tokens in the text of direct user messages, deduplicated
/// in first-seen order (unvalidated against the registry). `$name` is an
/// alias, as Codex users know it.
pub fn invoked_skill_names(claimed: &[Message]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for message in claimed {
        if !message.source.is_user() {
            continue;
        }
        for block in &message.content {
            let ContentBlock::Text { text } = block else {
                continue;
            };
            let mut prev_is_boundary = true;
            for (i, c) in text.char_indices() {
                if (c == '/' || c == '$') && prev_is_boundary {
                    let rest = &text[i + 1..];
                    let end = rest.find(is_js_space).unwrap_or(rest.len());
                    let token = &rest[..end];
                    if super::is_skill_name(token) && !names.iter().any(|n| n == token) {
                        names.push(token.to_owned());
                    }
                }
                prev_is_boundary = is_js_space(c);
            }
        }
    }
    names
}

/// The invocation listener of dsh `tool-skill`: one `skill-invocation`
/// message per user-invocable skill named by a `/name` gesture in `claimed`.
pub fn invocation_messages(
    registry: &SkillRegistry,
    cwd: &Path,
    claimed: &[Message],
) -> Vec<Message> {
    invoked_skill_names(claimed)
        .into_iter()
        .filter_map(|name| {
            let skill = registry.get(&name, cwd)?;
            skill.summary.user_invocable.then(|| {
                user_message(
                    render_skill_content(&skill),
                    json!({"kind": "skill-invocation", "name": name, "form": "instructions"}),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_are_normalized_and_capped() {
        assert_eq!(catalog_description("  a \n\t b\u{a0}c  ", 500), "a b c");
        assert_eq!(catalog_description("abcdef", 6), "abcdef");
        assert_eq!(catalog_description("abcdefg", 6), "abc...");
        // Surrogate pairs are never split.
        assert_eq!(catalog_description("ab😀cdef", 6), "ab...");
    }

    #[test]
    fn gestures() {
        let m = |t: &str| Message::user_text(t);
        assert_eq!(
            invoked_skill_names(&[m("/a run /b-c and /a, /usr/bin 5/8 /B /d\n/e")]),
            ["a", "b-c", "d", "e"]
        );
        assert_eq!(
            invoked_skill_names(&[m("$a and /b $HOME $c-d x$e")]),
            ["a", "b", "c-d"]
        );
        let mut injected = m("/x");
        injected.source = MessageSource::Other(json!({"kind": "runtime-context"}));
        assert!(invoked_skill_names(&[injected]).is_empty());
    }
}
