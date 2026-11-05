//! Row data (`tool/result.meta`) for Pi's tools, in the shapes Aether's tool
//! rows already read: `read` → `{path}`, `edit` → `{path, diffs}`, `write` →
//! `{path}`, `bash` → `{exitCode, truncated, fullOutputPath}`; bridge tools
//! bring Aether's own meta in `details.aether.meta`.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// `path` as Pi resolves it: absolute, `~/` from HOME, else below `cwd`.
pub fn absolute(path: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    cwd.join(p)
}

fn path_of(args: &Value, cwd: &Path) -> Option<String> {
    args.get("path")
        .or_else(|| args.get("file_path"))
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .map(|p| absolute(p, cwd).display().to_string())
}

/// `meta` for one finished Pi tool call; `result` is the
/// `tool_execution_end.result` (`content`, `details`, `structuredContent`).
pub fn tool_meta(name: &str, args: &Value, result: &Value, cwd: &Path) -> Option<Value> {
    if let Some(meta) = result.pointer("/details/aether/meta")
        && !meta.is_null()
    {
        return Some(meta.clone());
    }
    match name {
        "read" | "write" => path_of(args, cwd).map(|p| json!({ "path": p })),
        "edit" => {
            let path = path_of(args, cwd)?;
            let edits: Vec<Value> = match args.get("edits").and_then(Value::as_array) {
                Some(list) => list.clone(),
                None => vec![args.clone()],
            };
            let diffs: Vec<Value> = edits
                .iter()
                .filter_map(|e| {
                    let old = e.get("oldText").or_else(|| e.get("old_string"))?;
                    let new = e.get("newText").or_else(|| e.get("new_string"))?;
                    Some(json!({ "path": path, "oldText": old, "newText": new }))
                })
                .collect();
            let mut meta = Map::new();
            meta.insert("path".into(), path.into());
            if !diffs.is_empty() {
                meta.insert("diffs".into(), diffs.into());
            }
            Some(Value::Object(meta))
        }
        "bash" => {
            let s = result.get("structuredContent")?;
            let mut meta = Map::new();
            if let Some(code) = s.get("exit_code").and_then(Value::as_i64) {
                meta.insert("exitCode".into(), code.into());
            }
            if let Some(t) = s.get("truncated").and_then(Value::as_bool) {
                meta.insert("truncated".into(), t.into());
            }
            if let Some(p) = s
                .get("full_output_path")
                .or_else(|| s.get("fullOutputPath"))
                .and_then(Value::as_str)
            {
                meta.insert("fullOutputPath".into(), p.into());
            }
            (!meta.is_empty()).then_some(Value::Object(meta))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_get_absolute_paths_and_diffs() {
        let cwd = Path::new("/work");
        let edit = tool_meta(
            "edit",
            &json!({"path": "a.txt", "edits": [{"oldText": "x", "newText": "y"}]}),
            &json!({}),
            cwd,
        )
        .unwrap();
        assert_eq!(edit["path"], "/work/a.txt");
        assert_eq!(edit["diffs"][0]["newText"], "y");
        let bash = tool_meta(
            "bash",
            &json!({"command": "false"}),
            &json!({"structuredContent": {"exit_code": 1, "truncated": false}}),
            cwd,
        )
        .unwrap();
        assert_eq!(bash, json!({"exitCode": 1, "truncated": false}));
        let bridged = tool_meta(
            "todo_write",
            &json!({}),
            &json!({"details": {"aether": {"meta": {"k": 1}}}}),
            cwd,
        );
        assert_eq!(bridged, Some(json!({"k": 1})));
    }
}
