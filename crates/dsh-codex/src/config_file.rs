//! Writes a settings change into `config.toml` in place: only the keys the
//! change touched are rewritten, inserted or removed; comments, order,
//! formatting and every key the user left out stay as they are. New tables
//! (a server, a provider, a skill entry) are written without the values that
//! equal their defaults. When an edit cannot be placed safely (a table that
//! dotted keys define, say), the whole file is written from the config as
//! before, with a warning.
use std::ops::Range;

use dshx_core::config::{Config, McpServerConfig, PluginConfig, ProviderConfig, SkillEntryConfig};
use toml::de::{DeTable, DeValue};
use toml::{Table, Value};

/// One change between two configs, at a key path.
#[derive(Debug, Clone, PartialEq)]
enum Edit {
    /// A value that is not a new table: written inline at its key.
    Set(Vec<String>, Value),
    /// A table that did not exist: written as a `[path]` block.
    Table(Vec<String>, Table),
    Remove(Vec<String>),
}

/// The new text of `config.toml` (now `text`) after the change from
/// `before` to `after`.
pub fn render(text: &str, before: &Config, after: &Config) -> anyhow::Result<String> {
    let b = Value::try_from(before)?;
    let a = Value::try_from(after)?;
    let mut edits = Vec::new();
    diff(&mut Vec::new(), Some(&b), Some(&a), &mut edits);
    if edits.is_empty() && !text.trim().is_empty() {
        return Ok(text.to_owned());
    }
    match patch(text, &edits) {
        Some(patched) => Ok(patched),
        None => {
            tracing::warn!(
                "config.toml: a change could not be written in place; rewriting the file"
            );
            Ok(toml::to_string_pretty(after)?)
        }
    }
}

fn diff(
    path: &mut Vec<String>,
    before: Option<&Value>,
    after: Option<&Value>,
    out: &mut Vec<Edit>,
) {
    match (before, after) {
        (Some(Value::Table(b)), Some(Value::Table(a))) => {
            let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                path.push(key.clone());
                diff(path, b.get(key), a.get(key), out);
                path.pop();
            }
        }
        (_, None) => {
            if before.is_some() {
                out.push(Edit::Remove(path.clone()));
            }
        }
        (None, Some(Value::Table(a))) => {
            out.push(Edit::Table(path.clone(), without_defaults(path, a)))
        }
        (b, Some(a)) => {
            if b != Some(a) {
                out.push(Edit::Set(path.clone(), a.clone()));
            }
        }
    }
}

/// The defaults of a new map entry: what its fields are when left out.
fn defaults_of(path: &[String]) -> Option<Value> {
    let keys: Vec<&str> = path.iter().map(String::as_str).collect();
    match keys.as_slice() {
        ["mcp_servers", _] => Value::try_from(McpServerConfig::default()).ok(),
        ["providers", _] => Value::try_from(ProviderConfig::default()).ok(),
        ["skills", "entries", _] => Value::try_from(SkillEntryConfig::default()).ok(),
        ["plugins", _] => Value::try_from(PluginConfig::default()).ok(),
        _ => None,
    }
}

fn without_defaults(path: &[String], table: &Table) -> Table {
    let Some(Value::Table(defaults)) = defaults_of(path) else {
        return table.clone();
    };
    prune(table, &defaults)
}

fn prune(table: &Table, defaults: &Table) -> Table {
    let mut out = Table::new();
    for (key, value) in table {
        match (value, defaults.get(key)) {
            (v, Some(d)) if v == d => {}
            (Value::Table(t), Some(Value::Table(d))) => {
                let rest = prune(t, d);
                if !rest.is_empty() {
                    out.insert(key.clone(), Value::Table(rest));
                }
            }
            (Value::Table(t), None) if t.is_empty() => {}
            (Value::Array(a), None) if a.is_empty() => {}
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

// ---- applying edits ---------------------------------------------------------------

/// `text` with every edit applied, or `None` when one cannot be placed or
/// the result does not read back as intended.
fn patch(text: &str, edits: &[Edit]) -> Option<String> {
    let mut expected = toml::from_str::<Table>(text).ok()?;
    let mut doc = text.to_owned();
    for edit in edits {
        doc = apply(&doc, edit)?;
        apply_value(&mut expected, edit);
    }
    let got = toml::from_str::<Table>(&doc).ok()?;
    // Both sides must read as the same settings (defaults fill the gaps).
    let as_config = |t: Table| {
        toml::from_str::<Config>(&toml::to_string(&t).ok()?)
            .ok()
            .map(|c| Value::try_from(c).ok())
    };
    (as_config(got)? == as_config(expected)?).then_some(doc)
}

fn apply_value(root: &mut Table, edit: &Edit) {
    let (path, value) = match edit {
        Edit::Set(p, v) => (p, Some(v.clone())),
        Edit::Table(p, t) => (p, Some(Value::Table(t.clone()))),
        Edit::Remove(p) => (p, None),
    };
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut table = root;
    for key in parents {
        let entry = table
            .entry(key.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        let Value::Table(next) = entry else {
            return;
        };
        table = next;
    }
    match value {
        Some(v) => {
            table.insert(last.clone(), v);
        }
        None => {
            table.remove(last);
        }
    }
}

/// A `[header]` or `[[header]]` line of the document.
struct Header {
    path: Vec<String>,
    array: bool,
    /// Byte range of the block: the header line up to the next header.
    block: Range<usize>,
}

/// A `key = value` entry found by its path.
struct Entry {
    key: Range<usize>,
    value: Range<usize>,
    /// The entry sits inside an inline table (`{ … }`).
    inline_parent: bool,
}

fn apply(doc: &str, edit: &Edit) -> Option<String> {
    let parsed = DeTable::parse(doc).ok()?;
    let root = parsed.get_ref();
    let headers = header_lines(doc, root);
    match edit {
        Edit::Set(path, value) => {
            let aot =
                matches!(value, Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_table));
            if aot || headers.iter().any(|h| h.array && h.path == *path) {
                return replace_blocks(doc, &headers, path, &render_blocks(path, value.clone()));
            }
            match find(doc, root, path) {
                Some(entry) if entry.inline_parent => None,
                Some(entry) if on_header_line(doc, entry.key.start) => None,
                Some(entry) => Some(splice(doc, entry.value, &inline(value))),
                None => insert_key(doc, &headers, path, &inline(value)),
            }
        }
        Edit::Table(path, table) => {
            let text = match render_blocks(path, Value::Table(table.clone())) {
                t if t.trim().is_empty() => format!("[{}]\n", header_path(path)),
                t => t,
            };
            insert_block(doc, &headers, path, &text)
        }
        Edit::Remove(path) => {
            // A `key = value` line (also a table written inline)…
            let mut doc = doc.to_owned();
            if let Some(entry) = find(&doc, root, path)
                && !on_header_line(&doc, entry.key.start)
            {
                if entry.inline_parent {
                    return None;
                }
                doc = remove_line(&doc, &entry);
            }
            // …and the blocks at or under the path.
            let parsed = DeTable::parse(&doc).ok()?;
            let blocks = header_lines(&doc, parsed.get_ref());
            replace_blocks(&doc, &blocks, path, "")
        }
    }
}

/// Whether `pos` lies on a `[table]` header line.
fn on_header_line(doc: &str, pos: usize) -> bool {
    let start = doc[..pos].rfind('\n').map_or(0, |i| i + 1);
    doc[start..pos].trim_start().starts_with('[')
}

/// `{path: value}` nested from the root, for the serializer.
fn wrap(path: &[String], value: Value) -> Table {
    let mut value = value;
    for key in path.iter().rev() {
        let mut t = Table::new();
        t.insert(key.clone(), value);
        value = Value::Table(t);
    }
    match value {
        Value::Table(t) => t,
        _ => Table::new(),
    }
}

/// `value` at `path` as header blocks, written by the serializer.
fn render_blocks(path: &[String], value: Value) -> String {
    toml::to_string_pretty(&wrap(path, value)).unwrap_or_default()
}

fn splice(doc: &str, range: Range<usize>, with: &str) -> String {
    format!("{}{}{}", &doc[..range.start], with, &doc[range.end..])
}

/// Removes the lines from the key's line to the end of the value's line.
fn remove_line(doc: &str, entry: &Entry) -> String {
    let start = doc[..entry.key.start].rfind('\n').map_or(0, |i| i + 1);
    let end = doc[entry.value.end..]
        .find('\n')
        .map_or(doc.len(), |i| entry.value.end + i + 1);
    format!("{}{}", &doc[..start], &doc[end..])
}

/// Replaces every block at or under `path` with `text` (at the first one's
/// place, else as a new block).
fn replace_blocks(doc: &str, headers: &[Header], path: &[String], text: &str) -> Option<String> {
    let mut ranges: Vec<Range<usize>> = headers
        .iter()
        .filter(|h| h.path.starts_with(path))
        .map(|h| with_leading_comments(doc, h.block.clone()))
        .collect();
    if ranges.is_empty() {
        return if text.is_empty() {
            Some(doc.to_owned())
        } else {
            insert_block(doc, headers, path, text)
        };
    }
    ranges.sort_by_key(|r| r.start);
    let mut out = String::new();
    let mut at = 0;
    for (i, r) in ranges.iter().enumerate() {
        out.push_str(&doc[at..r.start.max(at)]);
        if i == 0 && !text.is_empty() {
            out.push_str(text);
            if !text.ends_with("\n\n") && r.end < doc.len() {
                out.push('\n');
            }
        }
        at = r.end.max(at);
    }
    out.push_str(&doc[at..]);
    Some(out)
}

/// A block plus the comment lines right above its header (they describe it).
fn with_leading_comments(doc: &str, block: Range<usize>) -> Range<usize> {
    let mut start = block.start;
    while start > 0 {
        let line_start = doc[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        if !doc[line_start..start].trim_start().starts_with('#') {
            break;
        }
        start = line_start;
    }
    start..block.end
}

/// Adds a new block after the blocks of its parent table, else at the end.
fn insert_block(doc: &str, headers: &[Header], path: &[String], text: &str) -> Option<String> {
    let parent = &path[..path.len().saturating_sub(1)];
    let after = headers
        .iter()
        .filter(|h| !parent.is_empty() && h.path.starts_with(parent))
        .map(|h| h.block.end)
        .max();
    let at = after.unwrap_or(doc.len());
    let before = &doc[..at];
    let mut out = String::from(before.trim_end_matches('\n'));
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(text.trim_end_matches('\n'));
    out.push('\n');
    let rest = &doc[at..];
    if !rest.trim().is_empty() {
        out.push('\n');
        out.push_str(rest.trim_start_matches('\n'));
    }
    Some(out)
}

/// Adds `key = value` to its table: after the table's last line, or as a
/// new `[table]` block when the document has none for it.
fn insert_key(doc: &str, headers: &[Header], path: &[String], value: &str) -> Option<String> {
    let (key, table) = path.split_last()?;
    let line = format!("{} = {}\n", key_repr(key), value);
    let block = if table.is_empty() {
        Some(0..headers.first().map_or(doc.len(), |h| h.block.start))
    } else {
        headers
            .iter()
            .find(|h| !h.array && h.path == table)
            .map(|h| h.block.clone())
    };
    let Some(block) = block else {
        // A table the document only implies (or not at all): its own block.
        let parsed = DeTable::parse(doc).ok()?;
        if find(doc, parsed.get_ref(), table)
            .is_some_and(|e| doc[e.value].trim_start().starts_with('{'))
        {
            return None;
        }
        return insert_block(
            doc,
            headers,
            table,
            &format!("[{}]\n{line}", header_path(table)),
        );
    };
    // After the block's last key line: trailing blanks and comments stay
    // with the next block.
    let body = &doc[block.clone()];
    let mut end = block.start;
    let mut offset = block.start;
    for l in body.split_inclusive('\n') {
        let t = l.trim();
        if !t.is_empty() && !t.starts_with('#') {
            end = offset + l.len();
        }
        offset += l.len();
    }
    if end == block.start && !table.is_empty() {
        end = block.start + body.find('\n').map_or(body.len(), |i| i + 1);
    }
    let mut out = String::from(&doc[..end]);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push_str(&doc[end..]);
    Some(out)
}

/// Finds the entry at `path` with its spans.
fn find(doc: &str, root: &DeTable<'_>, path: &[String]) -> Option<Entry> {
    let mut table = root;
    let mut inline_parent = false;
    for (i, key) in path.iter().enumerate() {
        let (k, v) = table
            .iter()
            .find(|(k, _)| k.get_ref().as_ref() == key.as_str())?;
        if i + 1 == path.len() {
            return Some(Entry {
                key: k.span(),
                value: v.span(),
                inline_parent,
            });
        }
        match v.get_ref() {
            DeValue::Table(t) => {
                inline_parent |= doc[v.span()].trim_start().starts_with('{');
                table = t;
            }
            _ => return None,
        }
    }
    None
}

/// The header lines of `doc` (lines starting with `[` outside any value).
fn header_lines(doc: &str, root: &DeTable<'_>) -> Vec<Header> {
    let mut values = Vec::new();
    collect_value_spans(doc, root, &mut values);
    let mut found: Vec<(usize, Vec<String>, bool)> = Vec::new();
    let mut offset = 0;
    for line in doc.split_inclusive('\n') {
        let t = line.trim_start();
        let at = offset + (line.len() - t.len());
        if t.starts_with('[') && !values.iter().any(|r| r.start <= at && at < r.end) {
            let array = t.starts_with("[[");
            let inner = if array {
                t.get(2..).and_then(|r| r.split("]]").next())
            } else {
                t.get(1..).and_then(|r| r.split(']').next())
            };
            if let Some(path) = inner.and_then(parse_key_path) {
                found.push((offset, path, array));
            }
        }
        offset += line.len();
    }
    let ends: Vec<usize> = found
        .iter()
        .skip(1)
        .map(|h| h.0)
        .chain([doc.len()])
        .collect();
    found
        .into_iter()
        .zip(ends)
        .map(|((start, path, array), end)| Header {
            path,
            array,
            block: start..end,
        })
        .collect()
}

fn collect_value_spans(doc: &str, table: &DeTable<'_>, out: &mut Vec<Range<usize>>) {
    for (k, v) in table.iter() {
        let header = on_header_line(doc, k.span().start);
        match v.get_ref() {
            DeValue::Table(t) if header || !doc[v.span()].trim_start().starts_with('{') => {
                collect_value_spans(doc, t, out);
            }
            DeValue::Array(items) if header => {
                for item in items.iter() {
                    if let DeValue::Table(t) = item.get_ref() {
                        collect_value_spans(doc, t, out);
                    }
                }
            }
            _ => out.push(v.span()),
        }
    }
}

/// `a."b c".d` → `["a", "b c", "d"]` (bare, basic and literal keys).
fn parse_key_path(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = s.trim().chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut key = String::new();
        match chars.peek()? {
            '"' => {
                chars.next();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => key.push('\n'),
                            't' => key.push('\t'),
                            c => key.push(c),
                        },
                        c => key.push(c),
                    }
                }
            }
            '\'' => {
                chars.next();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => key.push(c),
                    }
                }
            }
            _ => {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                {
                    key.push(chars.next()?);
                }
                if key.is_empty() {
                    return None;
                }
            }
        }
        out.push(key);
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            None => return Some(out),
            Some('.') => continue,
            Some(_) => return None,
        }
    }
}

fn key_repr(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_owned()
    } else {
        Value::String(key.to_owned()).to_string()
    }
}

fn header_path(path: &[String]) -> String {
    path.iter()
        .map(|k| key_repr(k))
        .collect::<Vec<_>>()
        .join(".")
}

/// A value on one line (`[…]`, `{ … }` for tables).
fn inline(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(inline).collect::<Vec<_>>().join(", ")
        ),
        Value::Table(t) if t.is_empty() => "{}".into(),
        Value::Table(t) => format!(
            "{{ {} }}",
            t.iter()
                .map(|(k, v)| format!("{} = {}", key_repr(k), inline(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMENTED: &str = r#"# My dsh-codex settings.
model = "deepseek-v4-pro" # the strong one

# Skills come through / only.
[skills]
catalog = "manual"

[skills.entries."release-notes"]
enabled = true # keep it

# A local server.
[mcp_servers.files]
command = "npx"
args = [
  "-y",
  "@modelcontextprotocol/server-filesystem",
]

[folders]
roots = ["/srv/work"]
"#;

    fn load(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn change(text: &str, f: impl FnOnce(&mut Config)) -> String {
        let before = load(text);
        let mut after = before.clone();
        f(&mut after);
        let out = render(text, &before, &after).unwrap();
        assert_eq!(
            Value::try_from(load(&out)).unwrap(),
            Value::try_from(&after).unwrap(),
            "{out}"
        );
        out
    }

    /// The lines that differ between two texts.
    fn changed_lines(a: &str, b: &str) -> (Vec<String>, Vec<String>) {
        let a: Vec<&str> = a.lines().collect();
        let b: Vec<&str> = b.lines().collect();
        let gone = a
            .iter()
            .filter(|l| !b.contains(l))
            .map(|l| l.to_string())
            .collect();
        let new = b
            .iter()
            .filter(|l| !a.contains(l))
            .map(|l| l.to_string())
            .collect();
        (gone, new)
    }

    #[test]
    fn a_skill_switch_changes_one_line() {
        let out = change(COMMENTED, |c| {
            c.skills.entries.get_mut("release-notes").unwrap().enabled = false
        });
        assert_eq!(
            changed_lines(COMMENTED, &out),
            (
                vec!["enabled = true # keep it".into()],
                vec!["enabled = false # keep it".into()]
            )
        );
    }

    #[test]
    fn a_new_skill_entry_is_a_block_without_defaults() {
        let out = change(COMMENTED, |c| {
            c.skills.entries.insert(
                "qa".into(),
                SkillEntryConfig {
                    enabled: true,
                    model: true,
                },
            );
        });
        let (gone, new) = changed_lines(COMMENTED, &out);
        assert!(gone.is_empty(), "{out}");
        assert_eq!(new, ["[skills.entries.qa]", "model = true"]);
        assert!(
            out.find("[skills.entries.qa]") < out.find("[mcp_servers.files]"),
            "next to its siblings: {out}"
        );
    }

    #[test]
    fn mcp_switches_add_and_remove_their_keys_only() {
        let on = change(COMMENTED, |c| {
            let s = c.mcp_servers.get_mut("files").unwrap();
            s.always = true;
            s.disabled_tools = vec!["write_file".into()];
        });
        let (gone, new) = changed_lines(COMMENTED, &on);
        assert!(gone.is_empty(), "{on}");
        assert_eq!(new, ["always = true", "disabled_tools = [\"write_file\"]"]);
        let off = change(&on, |c| {
            let s = c.mcp_servers.get_mut("files").unwrap();
            s.always = false;
            s.disabled_tools.clear();
        });
        assert_eq!(off, COMMENTED);
    }

    #[test]
    fn top_level_settings_keep_comments() {
        let out = change(COMMENTED, |c| {
            c.model = "deepseek-flash".into();
            c.reasoning_effort = Some("max".into());
        });
        let (gone, new) = changed_lines(COMMENTED, &out);
        assert_eq!(gone, ["model = \"deepseek-v4-pro\" # the strong one"]);
        assert_eq!(
            new,
            [
                "model = \"deepseek-flash\" # the strong one",
                "reasoning_effort = \"max\""
            ]
        );
        assert!(out.starts_with("# My dsh-codex settings.\n"));
    }

    #[test]
    fn servers_and_providers_come_and_go_as_blocks() {
        let added = change(COMMENTED, |c| {
            c.mcp_servers.insert(
                "linear".into(),
                McpServerConfig {
                    url: Some("https://mcp.linear.app/mcp".into()),
                    ..McpServerConfig::default()
                },
            );
        });
        let (gone, new) = changed_lines(COMMENTED, &added);
        assert!(gone.is_empty(), "{added}");
        assert_eq!(
            new,
            [
                "[mcp_servers.linear]",
                "url = \"https://mcp.linear.app/mcp\""
            ]
        );
        let removed = change(&added, |c| {
            c.mcp_servers.remove("files");
        });
        assert!(
            !removed.contains("files") && !removed.contains("# A local server."),
            "{removed}"
        );
        assert!(removed.contains("[folders]") && removed.contains("# Skills come through / only."));

        let provider = change(COMMENTED, |c| {
            let mut p = ProviderConfig {
                api: Some("openai-completions".into()),
                ..ProviderConfig::default()
            };
            p.models.push(dshx_core::config::ProviderModelConfig {
                id: "m1".into(),
                ..Default::default()
            });
            c.providers.insert("relay".into(), p);
        });
        let again = change(&provider, |c| {
            c.providers.get_mut("relay").unwrap().models.push(
                dshx_core::config::ProviderModelConfig {
                    id: "m2".into(),
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            again.matches("[[providers.relay.models]]").count(),
            2,
            "{again}"
        );
        let gone = change(&again, |c| {
            c.providers.remove("relay");
        });
        assert_eq!(gone.trim_end(), COMMENTED.trim_end());
    }

    #[test]
    fn a_first_change_writes_only_itself() {
        let out = change("", |c| c.model = "deepseek-v4-pro".into());
        assert_eq!(out, "model = \"deepseek-v4-pro\"\n");
        let folder = change("", |c| c.folders.roots = vec!["/srv".into()]);
        assert_eq!(folder.trim(), "[folders]\nroots = [\"/srv\"]");
    }

    #[test]
    fn compaction_settings_keep_the_rest_of_the_file() {
        use dshx_core::config::CompactionMode;
        let out = change(COMMENTED, |c| {
            c.codex.compaction.mode = CompactionMode::EveryTurn
        });
        let (gone, new) = changed_lines(COMMENTED, &out);
        assert!(gone.is_empty(), "{out}");
        assert_eq!(
            new,
            ["[codex.compaction]", "mode = \"every-turn\""],
            "{out}"
        );
        let limit = change(&out, |c| c.codex.compaction.threshold_tokens = 150_000);
        assert_eq!(
            changed_lines(&out, &limit),
            (vec![], vec!["threshold_tokens = 150000".to_owned()])
        );
    }

    #[test]
    fn inline_tables_are_rewritten_whole() {
        let text = "skills = { catalog = \"auto\" }\n";
        let out = change(text, |c| {
            c.skills.entries.insert(
                "qa".into(),
                SkillEntryConfig {
                    enabled: false,
                    model: false,
                },
            );
        });
        assert!(out.contains("catalog"), "{out}");
    }
}
