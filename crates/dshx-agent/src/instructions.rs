//! Workspace instructions from AGENTS.md-compatible files (dsh
//! `context/agent-instructions`): a durable `<system-reminder>` baseline at the
//! first request and additions/updates/removals after file tool touches.
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use dshx_core::{ContentBlock, Message, MessageSource, Role, kinds};
use serde_json::{Map, Value, json};
use sha1::{Digest, Sha1};
use tokio_util::sync::CancellationToken;

use crate::agent::{Inner, Target};
use crate::config::InstructionsConfig;

const OPEN: &str = "<system-reminder>";
const CLOSE: &str = "</system-reminder>";
const INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";
const REPLACEMENT_PREFIX: &str = "This complete workspace instruction baseline replaces all earlier workspace instruction baselines. ";
const EMPTY_REPLACEMENT_INTRO: &str = "This complete workspace instruction baseline replaces all earlier workspace instruction baselines. No workspace instructions are currently active.";
const COMPACT_INTRO: &str =
    "Workspace instructions were omitted or truncated to fit the configured byte budget.";
const GUIDANCE: &str = "Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.";
const USER_GLOBAL: &str = "user-global";
const AGENTS_GLOBAL: &str = "agents-global";
const USER_GLOBAL_FILE: &str = "AGENTS.md";
const SEP: char = '\u{0}';

fn sha1_hex(s: &str) -> String {
    let digest = Sha1::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// JS `String.prototype.trim` (whitespace and line terminators).
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

#[derive(Debug, Clone)]
struct File {
    abs: PathBuf,
    display: String,
    content: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Change {
    action: String,
    scope: String,
    path: String,
    digest: Option<String>,
}

impl Change {
    fn json(&self) -> Value {
        let mut m = Map::new();
        m.insert("action".into(), self.action.clone().into());
        m.insert("scope".into(), self.scope.clone().into());
        m.insert("path".into(), self.path.clone().into());
        if let Some(d) = &self.digest {
            m.insert("digest".into(), d.clone().into());
        }
        Value::Object(m)
    }

    fn parse(v: &Value) -> Option<Change> {
        let action = v.get("action")?.as_str()?;
        if !matches!(action, "set" | "replace" | "remove") {
            return None;
        }
        Some(Change {
            action: action.to_owned(),
            scope: v.get("scope")?.as_str()?.to_owned(),
            path: v.get("path")?.as_str()?.to_owned(),
            digest: v.get("digest").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

/// Lexical `path.resolve`-style normalization.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Node `path.relative(from, to)` for absolute normalized paths.
fn relative(from: &Path, to: &Path) -> String {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); f.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

fn dirname(display: &str) -> String {
    match display.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => display[..i].to_owned(),
        None => ".".into(),
    }
}

fn basename(display: &str) -> String {
    display.rsplit('/').next().unwrap_or(display).to_owned()
}

struct Ctx<'a> {
    cfg: &'a InstructionsConfig,
    cwd: PathBuf,
    root: PathBuf,
}

impl Ctx<'_> {
    fn global_display(&self, dir: &str) -> Option<(PathBuf, String)> {
        match dir {
            USER_GLOBAL => Some((
                self.cfg.harness_home.clone(),
                self.cfg.harness_home_display.clone(),
            )),
            AGENTS_GLOBAL => Some((
                self.cfg.agents_home.clone(),
                self.cfg.agents_home_display.clone(),
            )),
            _ => None,
        }
    }

    fn scope_for_display(&self, display: &str) -> String {
        if display == format!("{}/{USER_GLOBAL_FILE}", self.cfg.harness_home_display) {
            return USER_GLOBAL.into();
        }
        if display == format!("{}/{USER_GLOBAL_FILE}", self.cfg.agents_home_display) {
            return AGENTS_GLOBAL.into();
        }
        dirname(display)
    }

    fn scope_key(&self, display: &str) -> String {
        format!(
            "{}{SEP}{}",
            self.scope_for_display(display),
            basename(display)
        )
    }

    fn relative_display(&self, path: &Path) -> String {
        let rel = relative(&self.root, path);
        let first = rel.split('/').next().unwrap_or("");
        if first == USER_GLOBAL || first == AGENTS_GLOBAL {
            format!("./{rel}")
        } else {
            rel
        }
    }

    fn relative_scope(&self, dir: &Path) -> String {
        let s = self.relative_display(dir);
        if s.is_empty() { ".".into() } else { s }
    }

    /// Absolute and display path of one candidate scope key.
    fn scope_file(&self, scope: &str) -> (PathBuf, String) {
        let (dir, name) = scope.split_once(SEP).unwrap_or((scope, ""));
        if let Some((home, display)) = self.global_display(dir) {
            return (home.join(name), format!("{display}/{name}"));
        }
        let base = if dir == "." {
            self.root.clone()
        } else {
            self.root.join(dir)
        };
        let abs = base.join(name);
        let display = self.relative_display(&abs);
        (abs, display)
    }

    fn dir_scopes(&self, dir: &str, out: &mut Vec<String>) {
        for c in self.cfg.candidates.iter().chain(&self.cfg.local_candidates) {
            push_unique(out, format!("{dir}{SEP}{c}"));
        }
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

fn group_of(dir: &str) -> String {
    if dir == AGENTS_GLOBAL {
        USER_GLOBAL.into()
    } else {
        dir.to_owned()
    }
}

fn is_global(dir: &str) -> bool {
    dir == USER_GLOBAL || dir == AGENTS_GLOBAL
}

fn is_file(p: &Path) -> Option<bool> {
    match std::fs::metadata(p) {
        Ok(m) => Some(m.is_file()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

fn read_bounded(p: &Path, max: usize) -> Option<String> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() as usize > max {
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    if bytes.len() > max {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Walks up to the first directory holding a root marker; `cwd` when none.
fn find_project_root(cwd: &Path, markers: &[String]) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        for m in markers {
            if std::fs::metadata(current.join(m)).is_ok() {
                return current;
            }
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return cwd.to_path_buf(),
        }
    }
}

/// Inclusive root-to-cwd chain, broadest first.
fn ancestor_chain(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut current = cwd.to_path_buf();
    while current != root {
        chain.push(current.clone());
        match current.parent() {
            Some(p) if p != current => current = p.to_path_buf(),
            _ => break,
        }
    }
    chain.push(root.to_path_buf());
    chain.reverse();
    chain
}

fn descendant_dirs_between(cwd: &Path, touched: &str) -> Vec<PathBuf> {
    let target = normalize(&if Path::new(touched).is_absolute() {
        PathBuf::from(touched)
    } else {
        cwd.join(touched)
    });
    let Some(dir) = target.parent() else {
        return Vec::new();
    };
    let rel = relative(cwd, dir);
    if rel.is_empty() || rel.starts_with("..") {
        return Vec::new();
    }
    ancestor_chain(cwd, dir).into_iter().skip(1).collect()
}

/// Model-facing rendering style of one batch.
struct Style<'a> {
    intro: &'a str,
    section: &'a dyn Fn(&File) -> String,
}

fn escape(body: &str) -> String {
    body.replace(CLOSE, "<\\/system-reminder>")
}

fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn marker_text(max: usize, omitted: &[File], truncated: &[(String, usize, usize)]) -> String {
    if omitted.is_empty() && truncated.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    if !omitted.is_empty() {
        parts.push(format!(
            "omitted {}",
            omitted
                .iter()
                .map(|f| f.display.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !truncated.is_empty() {
        parts.push(format!(
            "truncated {}",
            truncated
                .iter()
                .map(|(d, o, i)| format!("{d} from {o} to {i} bytes"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    format!(
        "Workspace instruction budget {max} bytes: {}",
        parts.join("; ")
    )
}

fn build(
    files: &[File],
    max: usize,
    omitted: &[File],
    truncated: &[(String, usize, usize)],
    style: &Style<'_>,
) -> String {
    let marker = marker_text(max, omitted, truncated);
    let mut body: Vec<String> = vec![marker, style.intro.to_owned()];
    body.extend(files.iter().map(|f| (style.section)(f)));
    let body: Vec<String> = body.into_iter().filter(|b| !b.is_empty()).collect();
    format!("{OPEN}\n{}\n{CLOSE}", escape(&body.join("\n\n")))
}

fn truncate_to_fit(file: &File, max: usize, omitted: &[File], style: &Style<'_>) -> File {
    let original = file.content.len();
    let (mut low, mut high) = (0i64, original as i64);
    let mut best = File {
        content: String::new(),
        ..file.clone()
    };
    while low <= high {
        let mid = (low + high) / 2;
        let candidate = File {
            content: truncate_utf8(&file.content, mid as usize),
            ..file.clone()
        };
        let t = [(file.display.clone(), original, candidate.content.len())];
        if build(std::slice::from_ref(&candidate), max, omitted, &t, style).len() <= max {
            best = candidate;
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    best
}

/// dsh `renderInstructionContext`: text plus the files it represents.
fn render_context(files: &[File], max: usize, style: &Style<'_>) -> (String, Vec<File>) {
    if max == 0 {
        return (String::new(), Vec::new());
    }
    let full = build(files, max, &[], &[], style);
    if full.len() <= max {
        return (full, files.to_vec());
    }
    for start in 1..files.len() {
        let text = build(&files[start..], max, &files[..start], &[], style);
        if text.len() <= max {
            return (text, files[start..].to_vec());
        }
    }
    let Some(most) = files.last() else {
        return (String::new(), Vec::new());
    };
    let omitted = &files[..files.len() - 1];
    let original = most.content.len();
    let compact = Style {
        intro: COMPACT_INTRO,
        section: style.section,
    };
    for candidate_style in [style, &compact] {
        let t = truncate_to_fit(most, max, omitted, candidate_style);
        let included = t.content.len();
        let truncated = [(most.display.clone(), original, included)];
        let text = build(
            std::slice::from_ref(&t),
            max,
            omitted,
            &truncated,
            candidate_style,
        );
        if text.len() <= max {
            let represented = if included > 0 || original == 0 {
                vec![most.clone()]
            } else {
                Vec::new()
            };
            return (text, represented);
        }
    }
    let truncated = [(most.display.clone(), original, 0)];
    let notice = escape(&marker_text(max, omitted, &truncated));
    let heading = escape(&format!(
        "{notice}\n\n{}",
        (style.section)(&File {
            content: String::new(),
            ..most.clone()
        })
    ));
    if heading.len() <= max {
        let represented = if original == 0 {
            vec![most.clone()]
        } else {
            Vec::new()
        };
        return (heading, represented);
    }
    let text = if notice.len() <= max {
        notice
    } else {
        truncate_utf8(&notice, max)
    };
    (text, Vec::new())
}

struct BaselineSet {
    text: String,
    observed: Vec<File>,
    deduped: Vec<File>,
    included: Vec<File>,
}

fn discover(ctx: &Ctx<'_>) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mut add = |abs: PathBuf, display: String| {
        if !found.iter().any(|(a, _)| *a == abs) {
            found.push((abs, display));
        }
    };
    for dir in [USER_GLOBAL, AGENTS_GLOBAL] {
        let (home, display) = ctx.global_display(dir).unwrap();
        let abs = home.join(USER_GLOBAL_FILE);
        if is_file(&abs) == Some(true) {
            add(abs, format!("{display}/{USER_GLOBAL_FILE}"));
        }
    }
    for dir in ancestor_chain(&ctx.root, &ctx.cwd) {
        for list in [&ctx.cfg.candidates, &ctx.cfg.local_candidates] {
            for c in list.iter() {
                let abs = dir.join(c);
                if is_file(&abs) == Some(true) {
                    let display = ctx.relative_display(&abs);
                    add(abs, display);
                }
            }
        }
    }
    found
}

fn dedup(ctx: &Ctx<'_>, files: &[File]) -> Vec<File> {
    let mut kept_by_group: HashMap<String, HashSet<String>> = HashMap::new();
    let mut kept = Vec::new();
    for f in files {
        let group = group_of(&ctx.scope_for_display(&f.display));
        if kept_by_group
            .entry(group)
            .or_default()
            .insert(sha1_hex(js_trim(&f.content)))
        {
            kept.push(f.clone());
        }
    }
    kept
}

fn baseline_section(f: &File) -> String {
    format!("Instructions from: {}\n\n{}", f.display, f.content)
}

fn load_baseline(ctx: &Ctx<'_>, replace_previous: bool) -> Option<BaselineSet> {
    let cfg = ctx.cfg;
    if cfg.max_bytes == 0 || cfg.max_source_bytes == 0 {
        return None;
    }
    let loaded: Vec<File> = discover(ctx)
        .into_iter()
        .filter_map(|(abs, display)| {
            read_bounded(&abs, cfg.max_source_bytes).map(|content| File {
                abs,
                display,
                content,
            })
        })
        .collect();
    let kept = dedup(ctx, &loaded);
    let intro = if !replace_previous {
        INTRO.to_owned()
    } else if kept.is_empty() {
        EMPTY_REPLACEMENT_INTRO.to_owned()
    } else {
        format!("{REPLACEMENT_PREFIX}{INTRO}")
    };
    if kept.is_empty() && !replace_previous {
        return None;
    }
    let style = Style {
        intro: &intro,
        section: &baseline_section,
    };
    let (text, included) = render_context(&kept, cfg.max_bytes, &style);
    let kept_paths: HashSet<&PathBuf> = kept.iter().map(|f| &f.abs).collect();
    let deduped = loaded
        .iter()
        .filter(|f| !kept_paths.contains(&f.abs))
        .cloned()
        .collect();
    Some(BaselineSet {
        text,
        observed: loaded.clone(),
        deduped,
        included,
    })
}

fn baseline_changes(ctx: &Ctx<'_>, files: &[File]) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    for f in files {
        let c = Change {
            action: "set".into(),
            scope: ctx.scope_key(&f.display),
            path: f.display.clone(),
            digest: Some(sha1_hex(&f.content)),
        };
        out.retain(|o| o.scope != c.scope);
        out.push(c);
    }
    out
}

fn instructions_source(m: &Value) -> Option<&Value> {
    let s = m.get("source")?;
    (s.get("kind")? == "agent-instructions" && s.get("changes").is_some_and(Value::is_array))
        .then_some(s)
}

fn changed_section(ctx: &Ctx<'_>, change: &Change, file: &File) -> String {
    match change.action.as_str() {
        "set" => {
            let scope = ctx.scope_for_display(&file.display);
            let applicability = if is_global(&scope) {
                "These user-global instructions apply to all work.".to_owned()
            } else {
                format!("These instructions apply to work under `{scope}`.")
            };
            format!(
                "Additional instructions from: {}\n\n{applicability} {GUIDANCE}\n\n{}",
                file.display, file.content
            )
        }
        "remove" => format!(
            "Instructions removed: {}\n\nThe previously loaded instructions from this file no longer apply.",
            change.path
        ),
        _ => format!(
            "Updated instructions from: {}\n\nThis file changed after it was loaded. Use the following content instead of the previously loaded instructions from this file.\n\n{}",
            change.path, file.content
        ),
    }
}

struct Composed {
    content: Vec<String>,
    changes: Vec<Change>,
}

/// dsh `reconcileInstructionContext` (without the provider metadata fast path).
fn reconcile(
    ctx: &Ctx<'_>,
    effective: &HashMap<String, Change>,
    effective_order: &[String],
    include_baseline: bool,
    excluded: Option<&HashSet<String>>,
    touched: &[String],
) -> Option<Composed> {
    let mut baseline_scopes: Vec<String> = [USER_GLOBAL, AGENTS_GLOBAL]
        .iter()
        .map(|d| format!("{d}{SEP}{USER_GLOBAL_FILE}"))
        .collect();
    for dir in ancestor_chain(&ctx.root, &ctx.cwd) {
        ctx.dir_scopes(&ctx.relative_scope(&dir), &mut baseline_scopes);
    }
    let mut scopes: Vec<String> = Vec::new();
    if include_baseline {
        for s in &baseline_scopes {
            push_unique(&mut scopes, s.clone());
        }
    }
    for scope in effective_order {
        if !include_baseline && baseline_scopes.contains(scope) {
            continue;
        }
        let dir = scope.split_once(SEP).map(|(d, _)| d).unwrap_or(scope);
        if is_global(dir) {
            push_unique(&mut scopes, format!("{dir}{SEP}{USER_GLOBAL_FILE}"));
        } else {
            ctx.dir_scopes(dir, &mut scopes);
        }
    }
    for t in touched {
        for dir in descendant_dirs_between(&ctx.cwd, t) {
            ctx.dir_scopes(&ctx.relative_scope(&dir), &mut scopes);
        }
    }
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for s in scopes {
        let dir = s
            .split_once(SEP)
            .map(|(d, _)| d.to_owned())
            .unwrap_or_default();
        let g = group_of(&dir);
        match groups.iter_mut().find(|(k, _)| *k == g) {
            Some((_, v)) => v.push(s),
            None => groups.push((g, vec![s])),
        }
    }
    let mut items: Vec<(Change, File)> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut kept: HashMap<String, HashSet<String>> = HashMap::new();
    let removal = |scope: &str, path: &str| {
        (
            Change {
                action: "remove".into(),
                scope: scope.into(),
                path: path.into(),
                digest: None,
            },
            File {
                abs: PathBuf::from(format!("removed:{scope}")),
                display: path.into(),
                content: String::new(),
            },
        )
    };
    for (group, group_scopes) in groups {
        let mut probed = Vec::new();
        for scope in group_scopes {
            if let Some(ex) = excluded
                && baseline_scopes.contains(&scope)
                && ex.contains(&scope)
            {
                if let Some(prev) = effective.get(&scope).filter(|p| p.action != "remove") {
                    items.push(removal(&scope, &prev.path));
                }
            } else {
                probed.push(scope);
            }
        }
        let item_start = items.len();
        for scope in &probed {
            let previous = effective.get(scope).filter(|p| p.action != "remove");
            let (abs, display) = ctx.scope_file(scope);
            match is_file(&abs) {
                None => {
                    items.truncate(item_start);
                    for s in &probed {
                        seen.insert(ctx.scope_file(s).0);
                    }
                    kept.remove(&group);
                    break;
                }
                Some(false) => {
                    if let Some(prev) = previous {
                        items.push(removal(scope, &prev.path));
                    }
                    continue;
                }
                Some(true) => {}
            }
            if !seen.insert(abs.clone()) {
                continue;
            }
            let Some(content) = read_bounded(&abs, ctx.cfg.max_source_bytes) else {
                items.truncate(item_start);
                kept.remove(&group);
                break;
            };
            let digest = sha1_hex(&content);
            if !kept
                .entry(group.clone())
                .or_default()
                .insert(sha1_hex(js_trim(&content)))
            {
                if let Some(prev) = previous {
                    items.push(removal(scope, &prev.path));
                }
                continue;
            }
            if let Some(prev) = previous
                && prev.path == display
                && prev.digest.as_deref() == Some(digest.as_str())
            {
                continue;
            }
            let action = if previous.is_none() { "set" } else { "replace" };
            items.push((
                Change {
                    action: action.into(),
                    scope: scope.clone(),
                    path: display.clone(),
                    digest: Some(digest),
                },
                File {
                    abs,
                    display,
                    content,
                },
            ));
        }
    }
    if items.is_empty() {
        return None;
    }
    let by_abs: HashMap<PathBuf, Change> = items
        .iter()
        .map(|(c, f)| (f.abs.clone(), c.clone()))
        .collect();
    let section = |f: &File| {
        by_abs
            .get(&f.abs)
            .map(|c| changed_section(ctx, c, f))
            .unwrap_or_default()
    };
    let style = Style {
        intro: "",
        section: &section,
    };
    let files: Vec<File> = items.iter().map(|(_, f)| f.clone()).collect();
    let (text, represented) = render_context(&files, ctx.cfg.max_bytes, &style);
    let represented: HashSet<PathBuf> = represented.into_iter().map(|f| f.abs).collect();
    let changes: Vec<Change> = items
        .into_iter()
        .filter(|(_, f)| represented.contains(&f.abs))
        .map(|(c, _)| c)
        .collect();
    if text.is_empty() || changes.is_empty() {
        return None;
    }
    Some(Composed {
        content: vec![text],
        changes,
    })
}

fn message(text_blocks: Vec<String>, source: Value) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: text_blocks.into_iter().map(ContentBlock::text).collect(),
        source: MessageSource::Other(source),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// dsh `compose`: the instruction context the next request should receive.
fn compose(
    inner: &Inner,
    cfg: &InstructionsConfig,
    cwd: &Path,
    claimed: &[Message],
    touched: &[String],
) -> Option<Message> {
    if !cfg.enabled || cfg.max_bytes == 0 {
        return None;
    }
    let cwd = normalize(cwd);
    let root = find_project_root(&cwd, &cfg.project_root_markers);
    let ctx = Ctx {
        cfg,
        cwd: cwd.clone(),
        root: root.clone(),
    };
    let identity = serde_json::to_string(&json!({
        "projectRoot": relative(&cwd, &root),
        "projectRootMarkers": cfg.project_root_markers,
        "maxBytes": cfg.max_bytes,
        "maxSourceBytes": cfg.max_source_bytes,
        "instructionFileCandidates": cfg.candidates,
        "localInstructionFileCandidates": cfg.local_candidates,
    }))
    .unwrap_or_default();

    // Durable instruction sources visible on the surface, in surface order.
    let surface_sources: Vec<Value> = inner.journal.read(|events, surface| {
        surface
            .nodes
            .iter()
            .filter_map(|seq| events.get(*seq as usize))
            .filter(|e| e.kind == kinds::USER_MESSAGE)
            .filter_map(|e| instructions_source(&e.data).cloned())
            .collect()
    });
    let mut authority: Vec<Value> = claimed
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .filter_map(|m| instructions_source(&m).cloned())
        .collect();
    let visible_baseline = authority
        .iter()
        .rev()
        .chain(surface_sources.iter().rev())
        .find(|s| s.get("baseline") == Some(&Value::Bool(true)))
        .cloned();
    let present = visible_baseline.is_some();
    let keep_visible = visible_baseline
        .as_ref()
        .and_then(|b| b.get("baselineIdentity"))
        .and_then(Value::as_str)
        == Some(&identity);
    let prepared = inner.state.lock().unwrap().instructions_prepared.clone();
    let mut excluded = prepared
        .filter(|(id, _)| keep_visible && *id == identity)
        .map(|(_, ex)| ex);
    let mut content: Vec<String> = Vec::new();
    let mut changes: Vec<Change> = Vec::new();
    let mut desired_baseline = false;
    if !present || !keep_visible || excluded.is_none() {
        let replace_previous = present && !keep_visible;
        let set = load_baseline(&ctx, replace_previous);
        let included = set.as_ref().map(|s| s.included.clone()).unwrap_or_default();
        let baseline = baseline_changes(&ctx, &included);
        let observed = baseline_changes(
            &ctx,
            set.as_ref()
                .map(|s| s.observed.as_slice())
                .unwrap_or_default(),
        );
        let mut included_digests: HashMap<String, HashSet<String>> = HashMap::new();
        for f in &included {
            included_digests
                .entry(group_of(&ctx.scope_for_display(&f.display)))
                .or_default()
                .insert(sha1_hex(js_trim(&f.content)));
        }
        let mut ex: HashSet<String> = observed.iter().map(|c| c.scope.clone()).collect();
        for c in &baseline {
            ex.remove(&c.scope);
        }
        for f in set
            .as_ref()
            .map(|s| s.deduped.as_slice())
            .unwrap_or_default()
        {
            let group = group_of(&ctx.scope_for_display(&f.display));
            if included_digests
                .get(&group)
                .is_some_and(|d| d.contains(&sha1_hex(js_trim(&f.content))))
            {
                ex.remove(&ctx.scope_key(&f.display));
            }
        }
        excluded = Some(ex.clone());
        inner.state.lock().unwrap().instructions_prepared = Some((identity.clone(), ex));
        if !keep_visible && let Some(set) = set.filter(|s| !s.text.is_empty()) {
            let new_scopes: HashSet<&str> = baseline.iter().map(|c| c.scope.as_str()).collect();
            let mut batch: Vec<Change> = Vec::new();
            if replace_previous && let Some(prev) = &visible_baseline {
                for c in prev["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Change::parse)
                {
                    if c.action != "remove" && !new_scopes.contains(c.scope.as_str()) {
                        batch.push(Change {
                            action: "remove".into(),
                            scope: c.scope,
                            path: c.path,
                            digest: None,
                        });
                    }
                }
            }
            batch.extend(baseline.iter().cloned());
            content.push(set.text);
            authority.push(json!({
                "kind": "agent-instructions", "form": "instructions", "baseline": true,
                "baselineIdentity": identity, "changes": batch.iter().map(Change::json).collect::<Vec<_>>(),
            }));
            changes.extend(batch);
            desired_baseline = true;
        }
    }
    let mut effective: HashMap<String, Change> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for s in surface_sources.iter().chain(authority.iter()) {
        for c in s["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Change::parse)
        {
            push_unique(&mut order, c.scope.clone());
            effective.insert(c.scope.clone(), c);
        }
    }
    let update = reconcile(
        &ctx,
        &effective,
        &order,
        keep_visible,
        if keep_visible {
            excluded.as_ref()
        } else {
            None
        },
        touched,
    );
    if let Some(u) = update {
        content.extend(u.content);
        changes.extend(u.changes);
    }
    if content.is_empty() {
        return None;
    }
    let mut source = Map::new();
    source.insert("kind".into(), "agent-instructions".into());
    source.insert("form".into(), "instructions".into());
    if desired_baseline {
        source.insert("baseline".into(), true.into());
        source.insert("baselineIdentity".into(), identity.into());
    }
    source.insert(
        "changes".into(),
        Value::Array(changes.iter().map(Change::json).collect()),
    );
    Some(message(content, Value::Object(source)))
}

fn same_payload(a: &Message, b: &Message) -> bool {
    a.content == b.content && a.source == b.source
}

/// The pre-step listener: touch-driven refresh (staged through the next-step
/// inbox like dsh's projection) or the per-step reconciliation.
pub(crate) async fn pre_step(
    inner: &Arc<Inner>,
    claimed: &[Message],
    cancel: &CancellationToken,
) -> anyhow::Result<Option<Message>> {
    let (cfg, cwd) = {
        let c = inner.config.lock().unwrap();
        (c.instructions.clone(), c.cwd.clone())
    };
    if !cfg.enabled {
        return Ok(None);
    }
    let touched = std::mem::take(&mut inner.state.lock().unwrap().touches);
    if !touched.is_empty()
        && let Some(m) = compose(inner, &cfg, &cwd, &[], &touched)
    {
        inner
            .splice(Target::NextStep, 0, 0, vec![m.clone()], false)
            .await?;
        let index = inner
            .state
            .lock()
            .unwrap()
            .next_step
            .iter()
            .position(|p| p.id == m.id);
        if let Some(i) = index {
            inner
                .splice(Target::NextStep, i, 1, Vec::new(), true)
                .await?;
        }
        if cancel.is_cancelled() || claimed.iter().any(|c| same_payload(c, &m)) {
            return Ok(None);
        }
        return Ok(Some(m));
    }
    let desired = compose(inner, &cfg, &cwd, claimed, &[]);
    Ok(desired.filter(|d| !claimed.iter().any(|c| same_payload(c, d))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_truncates_most_specific_last() {
        let files = vec![
            File {
                abs: "/a".into(),
                display: "AGENTS.md".into(),
                content: "x".repeat(200),
            },
            File {
                abs: "/b".into(),
                display: "sub/AGENTS.md".into(),
                content: "y".repeat(200),
            },
        ];
        let style = Style {
            intro: INTRO,
            section: &baseline_section,
        };
        let (text, represented) = render_context(&files, 400, &style);
        assert!(text.len() <= 400, "{}", text.len());
        assert!(text.contains("Workspace instruction budget 400 bytes: omitted AGENTS.md"));
        assert_eq!(represented.len(), 1);
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a")), "..");
        assert_eq!(relative(Path::new("/a"), Path::new("/a/b/c")), "b/c");
    }
}
