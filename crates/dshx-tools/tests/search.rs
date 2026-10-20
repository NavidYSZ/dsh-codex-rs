//! glob / grep with ripgrep's defaults as dsh runs them.
mod common;

use std::time::{Duration, SystemTime};

use common::{call, err, ok, text};
use dshx_tools::search::{GlobTool, GrepTool};
use dshx_tools::testing::{builder, context};
use serde_json::json;

fn touch(path: &std::path::Path, content: &str, age_secs: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    let time = SystemTime::now() - Duration::from_secs(age_secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[tokio::test]
async fn glob_lists_hidden_and_ignored_files_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir(d.join(".git")).unwrap();
    touch(&d.join(".git/config.ts"), "", 10);
    touch(&d.join(".gitignore"), "ignored.ts\n", 50);
    touch(&d.join("ignored.ts"), "", 40);
    touch(&d.join(".hidden.ts"), "", 30);
    touch(&d.join("pkg/src/a.ts"), "", 20);
    touch(&d.join("b.js"), "", 5);
    let ctx = context(d);
    assert_eq!(
        ok(&GlobTool, &ctx, json!({"pattern": "*.ts"})).await,
        "ignored.ts\n.hidden.ts\npkg/src/a.ts"
    );
    assert_eq!(
        ok(
            &GlobTool,
            &ctx,
            json!({"pattern": "**/*.ts", "path": "pkg"})
        )
        .await,
        "pkg/src/a.ts"
    );
    // Globs with a separator are anchored at the session directory, like rg there.
    assert_eq!(
        ok(
            &GlobTool,
            &ctx,
            json!({"pattern": "src/**/*.ts", "path": "pkg"})
        )
        .await,
        "No files found"
    );
    assert_eq!(
        ok(
            &GlobTool,
            &ctx,
            json!({"pattern": "pkg/src/*.ts", "path": "pkg"})
        )
        .await,
        "pkg/src/a.ts"
    );
    let abs = d.join("pkg").to_string_lossy().into_owned();
    assert_eq!(
        ok(&GlobTool, &ctx, json!({"pattern": "*.ts", "path": abs})).await,
        "pkg/src/a.ts"
    );
    assert_eq!(
        err(&GlobTool, &ctx, json!({"pattern": "*", "path": "nope"})).await,
        "glob search failed (exit 2): rg: nope: IO error for operation on nope: No such file or directory (os error 2)"
    );
    assert_eq!(
        err(&GlobTool, &ctx, json!({"pattern": "["})).await,
        "glob pattern rejected by ripgrep: rg: error parsing glob '[': unclosed character class; missing ']'"
    );
    let out = call(&GlobTool, &ctx, json!({"pattern": "*.js"}))
        .await
        .unwrap();
    assert_eq!(
        out.meta.unwrap(),
        json!({"shape": "paths", "paths": ["b.js"], "truncated": false, "total": 1})
    );
}

#[tokio::test]
async fn glob_over_the_cap_saves_the_complete_list() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..105 {
        touch(&dir.path().join(format!("f{i:03}.txt")), "", 1000 - i);
    }
    let spill = tempfile::tempdir().unwrap();
    let ctx = builder(dir.path()).spill_root(spill.path()).build();
    let t = ok(&GlobTool, &ctx, json!({"pattern": "*.txt"})).await;
    let (body, footer) = t.split_once("\n\n").unwrap();
    assert_eq!(body.lines().count(), 100);
    assert!(body.starts_with("f000.txt\nf001.txt"));
    let path = footer
        .strip_prefix("(Showing 100 of 105 paths. Full sorted result stored at: ")
        .and_then(|rest| {
            rest.strip_suffix(
                ". Use read with offset/limit, or grep this path to search within it.)",
            )
        })
        .unwrap_or_else(|| panic!("footer: {footer}"));
    assert!(path.ends_with("-glob-results.txt"));
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 105);
}

#[tokio::test]
async fn grep_respects_gitignore_and_hidden_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir(d.join(".git")).unwrap();
    std::fs::write(d.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(d.join("ignored.txt"), "needle\n").unwrap();
    std::fs::write(d.join(".hidden.txt"), "needle\n").unwrap();
    std::fs::create_dir(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/h.txt"), "hello needle\nno\nneedle again\r\n").unwrap();
    std::fs::write(d.join("a.rs"), "let needle = 1;\n").unwrap();
    std::fs::write(d.join("bin.dat"), b"needle\0binary").unwrap();
    let ctx = context(d);
    let out = call(&GrepTool, &ctx, json!({"pattern": "needle"}))
        .await
        .unwrap();
    assert_eq!(
        text(&out),
        "Found 3 matches\n\na.rs\nLine 1: let needle = 1;\n\nsub/h.txt\nLine 1: hello needle\nLine 3: needle again"
    );
    assert_eq!(
        out.meta.unwrap(),
        json!({"shape": "matches", "files": [
            {"path": "a.rs", "matches": [{"lineNumber": 1, "line": "let needle = 1;"}]},
            {"path": "sub/h.txt", "matches": [{"lineNumber": 1, "line": "hello needle"}, {"lineNumber": 3, "line": "needle again"}]}
        ], "truncated": false, "total": 3})
    );
    assert_eq!(
        ok(
            &GrepTool,
            &ctx,
            json!({"pattern": "needle", "include": "*.rs"})
        )
        .await,
        "Found 1 match\n\na.rs\nLine 1: let needle = 1;"
    );
    assert_eq!(
        ok(&GrepTool, &ctx, json!({"pattern": "needle", "path": "sub"})).await,
        "Found 2 matches\n\nsub/h.txt\nLine 1: hello needle\nLine 3: needle again"
    );
    // An explicitly named file is searched even when ignored.
    assert_eq!(
        ok(
            &GrepTool,
            &ctx,
            json!({"pattern": "needle", "path": "ignored.txt"})
        )
        .await,
        "Found 1 match\n\nignored.txt\nLine 1: needle"
    );
    assert_eq!(
        ok(&GrepTool, &ctx, json!({"pattern": "absent"})).await,
        "No matches found"
    );
    assert_eq!(
        err(&GrepTool, &ctx, json!({"pattern": "("})).await,
        "grep pattern rejected by ripgrep: rg: regex parse error:\n    (?:()\n    ^\nerror: unclosed group"
    );
}

#[tokio::test]
async fn grep_over_the_cap_saves_the_complete_result() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (0..260).map(|i| format!("hit {i}\n")).collect();
    std::fs::write(dir.path().join("many.txt"), content).unwrap();
    let spill = tempfile::tempdir().unwrap();
    let ctx = builder(dir.path()).spill_root(spill.path()).build();
    let t = ok(&GrepTool, &ctx, json!({"pattern": "hit"})).await;
    assert!(t.starts_with("Found 250 of 260 matches\n\nmany.txt\nLine 1: hit 0\n"));
    let footer = t.rsplit("\n\n").next().unwrap();
    let path = footer
        .strip_prefix("(Full grep result stored at: ")
        .and_then(|rest| {
            rest.strip_suffix(
                ". Use read with offset/limit, or grep this path to search within it.)",
            )
        })
        .unwrap_or_else(|| panic!("footer: {footer}"));
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(saved.starts_with("Found 260 matches\n\nmany.txt\nLine 1: hit 0\n"));
    assert!(saved.ends_with("Line 260: hit 259"));
}
