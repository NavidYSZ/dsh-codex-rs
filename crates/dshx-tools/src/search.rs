//! `glob` and `grep` (dsh `tool-fs-search`), done in-process with ripgrep's
//! own crates instead of spawning `rg`, with rg's defaults as dsh invokes it:
//!
//! - glob = `rg --files --glob=<pattern> --sort=modified --no-ignore --hidden`
//!   minus VCS directories; up to 100 paths, oldest first.
//! - grep = `rg --json --regexp=<pattern> [--glob=<include>]` (gitignore,
//!   hidden files skipped, binary files skipped); up to 250 matches.
//!
//! Override globs are anchored at the session directory, like rg run there.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::spill::SpillRef;
use crate::state::ToolState;
use crate::util::utf8_prefix;

/// Paths shown inline (dsh `GLOB_MAX_RESULTS`).
pub const GLOB_MAX_RESULTS: usize = 100;
/// Matches shown inline (dsh `GREP_MAX_MATCHES`).
pub const GREP_MAX_MATCHES: usize = 250;
/// Bytes kept per matched line (dsh `GREP_MAX_LINE_BYTES`).
pub const GREP_MAX_LINE_BYTES: usize = 2000;
/// Raw result cap (dsh `RAW_OUTPUT_MAX_BYTES`).
pub const RAW_OUTPUT_MAX_BYTES: usize = 20_000_000;
/// Tool timeout (dsh `SEARCH_TIMEOUT_MS`).
pub const SEARCH_TIMEOUT: Duration = Duration::from_millis(30_000);
/// Presentation-meta cap (dsh `SEARCH_META_MAX_BYTES`).
const SEARCH_META_MAX_BYTES: usize = 65_536;
/// VCS directories glob never lists (dsh `GLOB_VCS_EXCLUDES`).
const GLOB_VCS_EXCLUDES: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// The search failed the way rg would have exited with status 2.
fn rg_failure(tool: &str, stderr: &str) -> ToolError {
    let stderr = stderr.trim();
    if stderr.contains("regex parse error") || stderr.to_lowercase().contains("error parsing glob")
    {
        return fail(format!("{tool} pattern rejected by ripgrep: {stderr}"));
    }
    let suffix = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    fail(format!("{tool} search failed (exit 2){suffix}"))
}

fn aborted(tool: &str) -> ToolError {
    fail(format!(
        "{tool} was aborted before completion (tool timeout or caller cancellation)"
    ))
}

fn overflow(tool: &str, bytes: usize) -> ToolError {
    fail(format!(
        "{tool} produced {bytes} bytes of raw output, over the {RAW_OUTPUT_MAX_BYTES}-byte cap; narrow pattern, path, or include and retry"
    ))
}

/// dsh `toWorkdirRelative`.
fn to_workdir_relative(path: &str, workdir: &Path) -> String {
    let p = Path::new(path);
    if !p.is_absolute() {
        return path.to_owned();
    }
    match p.strip_prefix(workdir) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_owned(),
    }
}

/// Where a search runs and how its hits are spelled.
struct SearchRoot {
    cwd: PathBuf,
    /// The path as the model typed it (rg's search path argument).
    given: Option<String>,
    abs: PathBuf,
}

impl SearchRoot {
    fn new(cwd: &Path, given: Option<&str>) -> Self {
        let abs = match given {
            Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
            Some(p) => cwd.join(p),
            None => cwd.to_path_buf(),
        };
        SearchRoot {
            cwd: cwd.to_path_buf(),
            given: given.map(str::to_owned),
            abs,
        }
    }

    /// rg's printed path for a walked entry, made workdir-relative like dsh.
    fn display(&self, entry: &Path) -> String {
        let rel = entry.strip_prefix(&self.abs).unwrap_or(entry);
        let printed = match &self.given {
            None => rel.to_string_lossy().into_owned(),
            Some(given) if rel.as_os_str().is_empty() => given.clone(),
            Some(given) => Path::new(given).join(rel).to_string_lossy().into_owned(),
        };
        to_workdir_relative(&printed, &self.cwd)
    }

    fn check_exists(&self, tool: &str) -> Result<(), ToolError> {
        if std::fs::symlink_metadata(&self.abs).is_err() {
            let given = self.given.clone().unwrap_or_else(|| ".".into());
            return Err(rg_failure(
                tool,
                &format!(
                    "rg: {given}: IO error for operation on {given}: No such file or directory (os error 2)"
                ),
            ));
        }
        Ok(())
    }
}

fn build_overrides(tool: &str, root: &Path, globs: &[String]) -> Result<Override, ToolError> {
    let mut builder = OverrideBuilder::new(root);
    for glob in globs {
        builder
            .add(glob)
            .map_err(|e| rg_failure(tool, &format!("rg: {e}")))?;
    }
    builder
        .build()
        .map_err(|e| rg_failure(tool, &format!("rg: {e}")))
}

/// Saves the complete result; `None` (with a warning) when it cannot.
fn save_complete(ctx: &ToolContext, name: &str, content: &str) -> Option<SpillRef> {
    let state = ToolState::get(ctx).ok()?;
    match state.spill().save_text(&ctx.session_id, name, content) {
        Ok(saved) => Some(saved),
        Err(error) => {
            tracing::warn!("search: complete {name} not saved: {error}");
            None
        }
    }
}

fn json_bytes(value: &Value) -> usize {
    serde_json::to_string(value).map_or(0, |s| s.len())
}

/// dsh `capMetaBytes`: drop trailing entries until the meta fits.
fn cap_meta(mut meta: Value, list_key: &str) -> Value {
    if json_bytes(&meta) <= SEARCH_META_MAX_BYTES {
        return meta;
    }
    meta["truncated"] = json!(true);
    while meta[list_key].as_array().is_some_and(|a| a.len() > 1)
        && json_bytes(&meta) > SEARCH_META_MAX_BYTES
    {
        if let Some(list) = meta[list_key].as_array_mut() {
            list.pop();
        }
    }
    meta
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    path: Option<String>,
}

/// The `glob` tool.
pub struct GlobTool;

fn glob_walk(
    root: &SearchRoot,
    pattern: &str,
    cancel: &AtomicBool,
) -> Result<Vec<String>, ToolError> {
    root.check_exists("glob")?;
    let mut globs = vec![pattern.to_owned()];
    for name in GLOB_VCS_EXCLUDES {
        globs.push(format!("!**/{name}"));
        globs.push(format!("!**/{name}/**"));
    }
    let overrides = build_overrides("glob", &root.cwd, &globs)?;
    let walker = WalkBuilder::new(&root.abs)
        .standard_filters(false)
        .overrides(overrides)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    let mut errors = Vec::new();
    let mut bytes = 0usize;
    for entry in walker {
        if cancel.load(Ordering::Relaxed) {
            return Err(aborted("glob"));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("rg: {error}"));
                continue;
            }
        };
        let is_file = entry.file_type().is_some_and(|t| t.is_file())
            || (entry.depth() == 0 && entry.path().is_file());
        if !is_file {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let display = root.display(entry.path());
        bytes += display.len() + 1;
        if bytes > RAW_OUTPUT_MAX_BYTES {
            return Err(overflow("glob", bytes));
        }
        found.push((modified, display));
    }
    if !errors.is_empty() {
        return Err(rg_failure("glob", &errors.join("\n")));
    }
    found.sort_by_key(|(modified, _)| *modified);
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// dsh glob rendering with `sampleOverCapGlobResults: false`.
fn render_glob(paths: &[String], spill: Option<&SpillRef>) -> String {
    if paths.is_empty() {
        return "No files found".into();
    }
    if paths.len() <= GLOB_MAX_RESULTS {
        return paths.join("\n");
    }
    let items = &paths[..GLOB_MAX_RESULTS];
    let recovery = match spill {
        Some(spill) => format!(
            "Full sorted result stored at: {}. {}",
            spill.locator, spill.retrieval_hint
        ),
        None => {
            "The complete result could not be saved; narrow pattern or path to see more.".into()
        }
    };
    format!(
        "{}\n\n(Showing {} of {} paths. {recovery})",
        items.join("\n"),
        items.len(),
        paths.len()
    )
}

/// Runs `job` on a blocking thread, cancelled by the tool's token.
async fn run_blocking<T: Send + 'static>(
    ctx: &ToolContext,
    tool: &'static str,
    job: impl FnOnce(&AtomicBool) -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    if ctx.cancel.is_cancelled() {
        return Err(aborted(tool));
    }
    let flag = Arc::new(AtomicBool::new(false));
    let worker_flag = flag.clone();
    let handle = tokio::task::spawn_blocking(move || job(&worker_flag));
    tokio::select! {
        result = handle => result.map_err(|e| fail(e.to_string()))?,
        _ = ctx.cancel.cancelled() => {
            flag.store(true, Ordering::Relaxed);
            Err(aborted(tool))
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("glob")
    }

    fn concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(SEARCH_TIMEOUT)
    }

    fn describe(&self, args: &Value) -> String {
        let pattern = args["pattern"].as_str().unwrap_or("");
        match args["path"].as_str() {
            Some(path) => format!("Glob {pattern} in {path}"),
            None => format!("Glob {pattern}"),
        }
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GlobArgs = args::parse("glob", args)?;
        if args.pattern.trim().is_empty() {
            return Err(fail("pattern must be a non-empty string"));
        }
        if args.path.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(fail("path must be a non-empty string when given"));
        }
        let root = SearchRoot::new(&ctx.cwd, args.path.as_deref());
        let pattern = args.pattern.clone();
        let paths = run_blocking(&ctx, "glob", move |cancel| {
            glob_walk(&root, &pattern, cancel)
        })
        .await?;
        let spill = if paths.len() > GLOB_MAX_RESULTS {
            save_complete(&ctx, "glob-results.txt", &paths.join("\n"))
        } else {
            None
        };
        let text = render_glob(&paths, spill.as_ref());
        let page: Vec<&String> = paths.iter().take(GLOB_MAX_RESULTS).collect();
        let truncated = paths.len() > GLOB_MAX_RESULTS;
        let meta = cap_meta(
            json!({"shape": "paths", "paths": page, "truncated": truncated, "total": paths.len()}),
            "paths",
        );
        let root_label = match &args.path {
            None => ".".to_owned(),
            Some(p) => to_workdir_relative(p, &ctx.cwd),
        };
        Ok(ToolOutput {
            value: Some(json!({"root": root_label, "paths": page, "total": paths.len()})),
            meta: Some(meta),
            ..ToolOutput::text(text)
        })
    }
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}

fn validate_include(include: &str) -> Result<(), ToolError> {
    if include.trim().is_empty() {
        return Err(fail("include must be a non-empty glob when given"));
    }
    if include.starts_with('!') {
        return Err(fail(
            "include must be a positive glob filter; negated patterns (\"!…\") are not supported",
        ));
    }
    let mut depth = 0usize;
    for ch in include.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                return Err(fail(
                    "include must be one glob, not a comma-separated list (use {a,b} alternation instead)",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

struct MatchSink<'a> {
    path: &'a str,
    matches: &'a mut Vec<GrepMatch>,
    bytes: &'a mut usize,
}

impl Sink for MatchSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let raw = mat.bytes();
        let line = match std::str::from_utf8(raw) {
            Ok(text) => {
                let text = text.strip_suffix('\n').unwrap_or(text);
                text.strip_suffix('\r').unwrap_or(text).to_owned()
            }
            Err(_) => "(line is not valid UTF-8)".to_owned(),
        };
        *self.bytes += raw.len() + self.path.len() + 64;
        if *self.bytes > RAW_OUTPUT_MAX_BYTES {
            return Err(std::io::Error::other("raw output overflow"));
        }
        self.matches.push(GrepMatch {
            path: self.path.to_owned(),
            line_number: mat.line_number().unwrap_or(0),
            line,
        });
        Ok(true)
    }
}

fn grep_walk(
    root: &SearchRoot,
    pattern: &str,
    include: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<GrepMatch>, ToolError> {
    let matcher = RegexMatcherBuilder::new()
        .line_terminator(Some(b'\n'))
        .build_many(&[pattern])
        .map_err(|e| rg_failure("grep", &format!("rg: {e}")))?;
    root.check_exists("grep")?;
    let mut walker = WalkBuilder::new(&root.abs);
    walker.sort_by_file_name(|a, b| a.cmp(b));
    if let Some(include) = include {
        walker.overrides(build_overrides("grep", &root.cwd, &[include.to_owned()])?);
    }
    let mut matches = Vec::new();
    let mut errors = Vec::new();
    let mut bytes = 0usize;
    for entry in walker.build() {
        if cancel.load(Ordering::Relaxed) {
            return Err(aborted("grep"));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("rg: {error}"));
                continue;
            }
        };
        let explicit = entry.depth() == 0;
        let is_file =
            entry.file_type().is_some_and(|t| t.is_file()) || (explicit && entry.path().is_file());
        if !is_file {
            continue;
        }
        let detection = if explicit {
            BinaryDetection::convert(b'\x00')
        } else {
            BinaryDetection::quit(b'\x00')
        };
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .binary_detection(detection)
            .build();
        let display = root.display(entry.path());
        let sink = MatchSink {
            path: &display,
            matches: &mut matches,
            bytes: &mut bytes,
        };
        if let Err(error) = searcher.search_path(&matcher, entry.path(), sink) {
            if bytes > RAW_OUTPUT_MAX_BYTES {
                return Err(overflow("grep", bytes));
            }
            errors.push(format!("rg: {display}: {error}"));
        }
    }
    if !errors.is_empty() {
        return Err(rg_failure("grep", &errors.join("\n")));
    }
    Ok(matches)
}

/// dsh `previewLine`.
fn preview_line(line: &str) -> String {
    let kept = utf8_prefix(line, GREP_MAX_LINE_BYTES);
    if kept.len() < line.len() {
        format!("{kept} (line truncated)")
    } else {
        kept.to_owned()
    }
}

/// dsh `formatGrepMatches`: one section per file, in first-seen order.
fn format_matches(matches: &[GrepMatch]) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: std::collections::HashMap<&str, Vec<&GrepMatch>> =
        std::collections::HashMap::new();
    for m in matches {
        if !groups.contains_key(m.path.as_str()) {
            order.push(&m.path);
        }
        groups.entry(&m.path).or_default().push(m);
    }
    order
        .iter()
        .map(|path| {
            let lines: Vec<String> = groups[path]
                .iter()
                .map(|m| format!("Line {}: {}", m.line_number, m.line))
                .collect();
            format!("{path}\n{}", lines.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn match_noun(count: usize) -> &'static str {
    if count == 1 { "match" } else { "matches" }
}

fn render_grep(all: &[GrepMatch], kept: &[GrepMatch], spill: Option<&SpillRef>) -> String {
    if all.is_empty() {
        return "No matches found".into();
    }
    let truncated = all.len() > kept.len();
    let header = if truncated {
        format!("Found {} of {} matches", kept.len(), all.len())
    } else {
        format!("Found {} {}", all.len(), match_noun(all.len()))
    };
    let body = format_matches(kept);
    if !truncated {
        return format!("{header}\n\n{body}");
    }
    let recovery = match spill {
        Some(spill) => format!(
            "Full grep result stored at: {}. {}",
            spill.locator, spill.retrieval_hint
        ),
        None => {
            "The complete result could not be saved; narrow pattern, path, or include to see more."
                .into()
        }
    };
    format!("{header}\n\n{body}\n\n({recovery})")
}

/// The `grep` tool.
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("grep")
    }

    fn concurrency_safe(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(SEARCH_TIMEOUT)
    }

    fn describe(&self, args: &Value) -> String {
        let mut title = format!("Grep {}", args["pattern"].as_str().unwrap_or(""));
        if let Some(path) = args["path"].as_str() {
            title.push_str(&format!(" in {path}"));
        }
        if let Some(include) = args["include"].as_str() {
            title.push_str(&format!(" ({include})"));
        }
        title
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GrepArgs = args::parse("grep", args)?;
        if args.pattern.is_empty() {
            return Err(fail("pattern must be a non-empty string"));
        }
        if args.path.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(fail("path must be a non-empty string when given"));
        }
        if let Some(include) = &args.include {
            validate_include(include)?;
        }
        let root = SearchRoot::new(&ctx.cwd, args.path.as_deref());
        let pattern = args.pattern.clone();
        let include = args.include.clone();
        let all = run_blocking(&ctx, "grep", move |cancel| {
            grep_walk(&root, &pattern, include.as_deref(), cancel)
        })
        .await?;
        let kept: Vec<GrepMatch> = all
            .iter()
            .take(GREP_MAX_MATCHES)
            .map(|m| GrepMatch {
                line: preview_line(&m.line),
                ..m.clone()
            })
            .collect();
        let spill = if all.len() > GREP_MAX_MATCHES {
            let previewed: Vec<GrepMatch> = all
                .iter()
                .map(|m| GrepMatch {
                    line: preview_line(&m.line),
                    ..m.clone()
                })
                .collect();
            let content = format!(
                "Found {} {}\n\n{}",
                all.len(),
                match_noun(all.len()),
                format_matches(&previewed)
            );
            save_complete(&ctx, "grep-results.txt", &content)
        } else {
            None
        };
        let text = render_grep(&all, &kept, spill.as_ref());
        let mut files: Vec<Value> = Vec::new();
        for m in &kept {
            let entry = json!({"lineNumber": m.line_number, "line": m.line});
            match files.last_mut().filter(|f| f["path"] == m.path.as_str()) {
                Some(file) => file["matches"]
                    .as_array_mut()
                    .into_iter()
                    .for_each(|a| a.push(entry.clone())),
                None => files.push(json!({"path": m.path, "matches": [entry]})),
            }
        }
        let truncated = all.len() > kept.len();
        let meta = cap_meta(
            json!({"shape": "matches", "files": files, "truncated": truncated, "total": all.len()}),
            "files",
        );
        let value_matches: Vec<Value> = kept
            .iter()
            .map(|m| json!({"path": m.path, "lineNumber": m.line_number, "line": m.line}))
            .collect();
        Ok(ToolOutput {
            value: Some(json!({"matches": value_matches, "total": all.len()})),
            meta: Some(meta),
            ..ToolOutput::text(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_rendering() {
        let m = |p: &str, n: u64, l: &str| GrepMatch {
            path: p.into(),
            line_number: n,
            line: l.into(),
        };
        let all = vec![m("a.rs", 1, "x"), m("a.rs", 3, "y"), m("b.rs", 2, "z")];
        assert_eq!(
            render_grep(&all, &all, None),
            "Found 3 matches\n\na.rs\nLine 1: x\nLine 3: y\n\nb.rs\nLine 2: z"
        );
        assert_eq!(
            render_grep(&all[..1], &all[..1], None),
            "Found 1 match\n\na.rs\nLine 1: x"
        );
        assert_eq!(render_grep(&[], &[], None), "No matches found");
        assert_eq!(
            render_grep(&all, &all[..1], None),
            "Found 1 of 3 matches\n\na.rs\nLine 1: x\n\n(The complete result could not be saved; narrow pattern, path, or include to see more.)"
        );
        assert_eq!(
            preview_line(&"é".repeat(1001)),
            format!("{} (line truncated)", "é".repeat(1000))
        );
    }

    #[test]
    fn include_validation() {
        assert!(validate_include("*.{js,jsx}").is_ok());
        assert_eq!(
            validate_include("*.js,*.ts").unwrap_err().to_string(),
            "include must be one glob, not a comma-separated list (use {a,b} alternation instead)"
        );
        assert!(validate_include("!x").is_err());
    }

    #[test]
    fn glob_rendering() {
        let paths: Vec<String> = (0..101).map(|i| format!("f{i}")).collect();
        let spill = SpillRef {
            locator: "/tmp/s/x-glob-results.txt".into(),
            bytes: 1,
            retrieval_hint: crate::spill::RETRIEVAL_HINT,
        };
        let text = render_glob(&paths, Some(&spill));
        assert!(text.ends_with("f99\n\n(Showing 100 of 101 paths. Full sorted result stored at: /tmp/s/x-glob-results.txt. Use read with offset/limit, or grep this path to search within it.)"));
        assert_eq!(render_glob(&[], None), "No files found");
    }
}
