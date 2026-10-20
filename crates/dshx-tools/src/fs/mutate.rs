//! The `write` and `edit` tools (dsh `tool-fs/write.ts`, `edit.ts`, guarded by
//! the `fs-observation-policy`: a file must have been read — or created or
//! edited by this session — at its current version before it changes).
use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Info, Target, checked_target, hunk_diffs, lock_target, probe, resolve, resolve_mode,
    write_atomic,
};
use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::state::{Observation, ToolState};

/// Files at or above this size get no diff basis (dsh `diffBasisMaxBytes`).
const DIFF_BASIS_MAX_BYTES: u64 = 10 * 1024 * 1024;

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn not_observed(display: &str) -> ToolError {
    fail(format!(
        "cannot modify \"{display}\": file has not been read — read the file, then retry"
    ))
}

fn stale(message: String) -> ToolError {
    fail(format!("{message} — re-read the file, then retry"))
}

fn record_present(state: &ToolState, target: &Target) {
    if let Ok(Some(info)) = probe(&target.key) {
        state.observe(&target.key, Observation::Present(info.version));
    }
}

/// dsh `readTextForDiff`: LF-normalized text, or `None` for large/binary/invalid files.
fn diff_basis(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 >= DIFF_BASIS_MAX_BYTES || bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let text = text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text);
    Some(normalize_line_endings(&text))
}

#[derive(Deserialize)]
struct WriteArgs {
    file_path: String,
    content: String,
    sandbox_permissions: Option<String>,
    justification: Option<String>,
}

/// The `write` tool.
pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("write")
    }

    fn describe(&self, args: &Value) -> String {
        format!("Write {}", args["file_path"].as_str().unwrap_or(""))
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = args::parse("write", args)?;
        if args.file_path.trim().is_empty() {
            return Err(fail("file_path must be a non-empty string"));
        }
        let mode = resolve_mode(
            &ctx,
            "write",
            args.sandbox_permissions.as_deref(),
            args.justification.as_deref(),
            &args.file_path,
        )
        .await?;
        let state = ToolState::get(&ctx)?;
        let target = resolve(&ctx.cwd, &args.file_path)?;
        // fs/write-intent: replace the observed version, or create only.
        let expected = match state.observation(&target.key) {
            Some(Observation::Present(version)) => Some(version),
            _ => None,
        };
        let fresh = checked_target(&target, mode, &ctx.cwd)?;
        let _guard = lock_target(&fresh.key).await;
        let display = fresh.display.clone();
        let existing: Option<Info> = probe(&fresh.key)?;
        if let Some(info) = &existing
            && !info.is_file
        {
            return Err(fail(format!(
                "cannot write \"{display}\": not a regular file"
            )));
        }
        match (&expected, &existing) {
            (Some(_), None) => {
                return Err(stale(format!(
                    "cannot write \"{display}\": file no longer exists"
                )));
            }
            (Some(version), Some(info)) if *version != info.version => {
                return Err(stale(format!(
                    "cannot write \"{display}\": file changed since it was read"
                )));
            }
            (None, Some(_)) => return Err(not_observed(&display)),
            _ => {}
        }
        let before = match &existing {
            Some(_) if (args.content.len() as u64) < DIFF_BASIS_MAX_BYTES => diff_basis(&fresh.key),
            _ => None,
        };
        let key = fresh.key.clone();
        let content = args.content.clone();
        let mode_bits = existing.as_ref().map(|i| i.mode);
        let create_only = expected.is_none();
        tokio::task::spawn_blocking(move || {
            write_atomic(&key, content.as_bytes(), mode_bits, create_only, &display)
        })
        .await
        .map_err(|e| fail(e.to_string()))??;
        record_present(state, &target);
        let path = target.process_path();
        let operation = if existing.is_some() {
            "update"
        } else {
            "create"
        };
        let after = normalize_line_endings(&args.content);
        let diffs = match &before {
            Some(before) => hunk_diffs(&path, before, &after),
            None => Vec::new(),
        };
        let verb = if operation == "create" {
            "Created"
        } else {
            "Updated"
        };
        let text =
            format!("<path>{path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>");
        Ok(ToolOutput {
            value: Some(json!({"path": path, "operation": operation})),
            meta: Some(json!({"operation": operation, "path": path, "diffs": diffs})),
            ..ToolOutput::text(text)
        })
    }
}

#[derive(Deserialize)]
struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
    sandbox_permissions: Option<String>,
    justification: Option<String>,
}

/// dsh `applyLiteralEdit`.
pub fn apply_literal_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
    display: &str,
) -> Result<String, ToolError> {
    let old = normalize_line_endings(old);
    if old.is_empty() {
        return Err(fail("old_string must be a non-empty string"));
    }
    let new = normalize_line_endings(new);
    let count = content.matches(old.as_str()).count();
    if count == 0 {
        return Err(fail(format!("old_string was not found in \"{display}\"")));
    }
    if !replace_all && count > 1 {
        return Err(fail(format!(
            "old_string matched {count} times in \"{display}\"; provide a more specific old_string or set replace_all to true"
        )));
    }
    Ok(content.replace(old.as_str(), &new))
}

/// The `edit` tool.
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("edit")
    }

    fn describe(&self, args: &Value) -> String {
        format!("Edit {}", args["file_path"].as_str().unwrap_or(""))
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = args::parse("edit", args)?;
        if args.file_path.trim().is_empty() {
            return Err(fail("file_path must be a non-empty string"));
        }
        if args.old_string.is_empty() {
            return Err(fail("old_string must be a non-empty string"));
        }
        if args.old_string == args.new_string {
            return Err(fail("old_string and new_string must differ"));
        }
        let replace_all = args.replace_all.unwrap_or(false);
        let mode = resolve_mode(
            &ctx,
            "edit",
            args.sandbox_permissions.as_deref(),
            args.justification.as_deref(),
            &args.file_path,
        )
        .await?;
        let state = ToolState::get(&ctx)?;
        let target = resolve(&ctx.cwd, &args.file_path)?;
        // fs/edit-intent: an edit needs a present observation.
        let expected = match state.observation(&target.key) {
            None => return Err(not_observed(&target.display)),
            Some(Observation::Absent) => {
                return Err(fail(format!(
                    "cannot edit \"{}\": not found",
                    target.display
                )));
            }
            Some(Observation::Present(version)) => version,
        };
        let fresh = checked_target(&target, mode, &ctx.cwd)?;
        let _guard = lock_target(&fresh.key).await;
        let display = fresh.display.clone();
        let changed = || {
            stale(format!(
                "cannot edit \"{display}\": file changed since it was read"
            ))
        };
        let existing = probe(&fresh.key)?.ok_or_else(changed)?;
        if !existing.is_file {
            return Err(fail(format!(
                "cannot edit \"{display}\": not a regular file"
            )));
        }
        if existing.version != expected {
            return Err(changed());
        }
        let bytes = std::fs::read(&fresh.key)
            .map_err(|e| fail(format!("cannot edit \"{display}\": {e}")))?;
        if bytes.contains(&0) {
            return Err(fail(format!("cannot edit \"{display}\": binary file")));
        }
        let (bom, body) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            Some(rest) => (true, rest),
            None => (false, &bytes[..]),
        };
        let raw = std::str::from_utf8(body)
            .map_err(|_| fail(format!("cannot edit \"{display}\": invalid UTF-8 text")))?;
        let crlf = {
            let sample = crate::util::utf16_prefix(raw, 4096);
            let crlf_count = sample.matches("\r\n").count();
            crlf_count > sample.matches('\n').count() - crlf_count
        };
        let before = normalize_line_endings(raw);
        let after = apply_literal_edit(
            &before,
            &args.old_string,
            &args.new_string,
            replace_all,
            &display,
        )?;
        let mut stored = if crlf {
            after.replace('\n', "\r\n")
        } else {
            after.clone()
        };
        if bom {
            stored.insert(0, '\u{feff}');
        }
        let key = fresh.key.clone();
        let mode_bits = existing.mode;
        let display_for_write = display.clone();
        tokio::task::spawn_blocking(move || {
            write_atomic(
                &key,
                stored.as_bytes(),
                Some(mode_bits),
                false,
                &display_for_write,
            )
        })
        .await
        .map_err(|e| fail(e.to_string()))??;
        record_present(state, &target);
        let path = target.process_path();
        let text = if replace_all {
            format!("The file {path} has been updated. All occurrences were successfully replaced.")
        } else {
            format!("The file {path} has been updated successfully.")
        };
        Ok(ToolOutput {
            value: Some(json!({"path": path})),
            meta: Some(json!({"path": path, "diffs": hunk_diffs(&path, &before, &after)})),
            ..ToolOutput::text(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_edit_errors() {
        assert_eq!(
            apply_literal_edit("a b a", "b", "c", false, "/f").unwrap(),
            "a c a"
        );
        assert_eq!(
            apply_literal_edit("a b a", "a", "c", false, "/f")
                .unwrap_err()
                .to_string(),
            "old_string matched 2 times in \"/f\"; provide a more specific old_string or set replace_all to true"
        );
        assert_eq!(
            apply_literal_edit("a b a", "a", "c", true, "/f").unwrap(),
            "c b c"
        );
        assert_eq!(
            apply_literal_edit("a", "z", "c", false, "/f")
                .unwrap_err()
                .to_string(),
            "old_string was not found in \"/f\""
        );
        assert_eq!(
            apply_literal_edit("x\ny\n", "x\r\ny", "z", false, "/f").unwrap(),
            "z\n"
        );
    }
}
