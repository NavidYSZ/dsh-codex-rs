//! read / write / edit / read_image end to end, including the observation
//! policy and the in-process sandbox fence.
mod common;

use common::{call, err, ok, text};
use dshx_core::{ApprovalDecision, SandboxMode, ToolResultPart};
use dshx_tools::fs::image::ReadImageTool;
use dshx_tools::fs::mutate::{EditTool, WriteTool};
use dshx_tools::fs::read::ReadTool;
use dshx_tools::sandbox::{denial_marker, escalation_hint};
use dshx_tools::testing::{FixedApprover, builder, context};
use serde_json::json;

fn real(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn read_formats_like_dsh() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = real(dir.path());
    std::fs::write(dir.path().join("greeting.txt"), "hello\n").unwrap();
    let ctx = context(dir.path());
    let out = call(&ReadTool, &ctx, json!({"file_path": "greeting.txt"}))
        .await
        .unwrap();
    assert_eq!(
        text(&out),
        format!(
            "<path>{cwd}/greeting.txt</path>\n<type>file</type>\n<content>\n1: hello\n\n(End of file - total 1 lines)\n</content>"
        )
    );
    assert_eq!(
        out.meta.unwrap(),
        json!({"path": format!("{cwd}/greeting.txt"), "offset": 1, "lines": [{"number": 1, "text": "hello"}], "totalLines": 1})
    );
    let big: String = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ]
    .iter()
    .map(|w| format!("line {w}\n"))
    .collect();
    std::fs::write(dir.path().join("big.txt"), big).unwrap();
    let t = ok(
        &ReadTool,
        &ctx,
        json!({"file_path": "big.txt", "offset": 5, "limit": 4}),
    )
    .await;
    assert_eq!(
        t,
        format!(
            "<path>{cwd}/big.txt</path>\n<type>file</type>\n<content>\n5: line five\n6: line six\n7: line seven\n8: line eight\n\n(Showing lines 5-8 of 10. Use offset=9 to continue.)\n</content>"
        )
    );
    std::fs::write(dir.path().join("x.rs"), "fn main() {}\n").unwrap();
    let out = call(&ReadTool, &ctx, json!({"file_path": "x.rs"}))
        .await
        .unwrap();
    assert_eq!(out.meta.unwrap()["lang"], "rs");

    let display = format!("{}/nope.txt", dir.path().display());
    assert_eq!(
        err(&ReadTool, &ctx, json!({"file_path": "nope.txt"})).await,
        format!("cannot read \"{display}\": not found")
    );
    assert_eq!(
        err(
            &ReadTool,
            &ctx,
            json!({"file_path": "big.txt", "offset": 0})
        )
        .await,
        "offset must be a positive integer"
    );
    assert_eq!(
        err(
            &ReadTool,
            &ctx,
            json!({"file_path": "big.txt", "limit": 2001})
        )
        .await,
        "limit must be less than or equal to 2000"
    );
    assert_eq!(
        err(&ReadTool, &ctx, json!({"file_path": "."})).await,
        format!(
            "cannot read \"{}\": not a regular file",
            dir.path().display()
        )
    );
}

#[tokio::test]
async fn write_requires_a_read_of_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = real(dir.path());
    let display = format!("{}/data.txt", dir.path().display());
    let ctx = context(dir.path());

    let out = call(
        &WriteTool,
        &ctx,
        json!({"file_path": "notes.txt", "content": "hello world"}),
    )
    .await
    .unwrap();
    assert_eq!(
        text(&out),
        format!(
            "<path>{cwd}/notes.txt</path>\n<type>file</type>\n<content>\nCreated file\n</content>"
        )
    );
    assert_eq!(
        out.meta.unwrap(),
        json!({"operation": "create", "path": format!("{cwd}/notes.txt"), "diffs": []})
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "hello world"
    );

    std::fs::write(dir.path().join("data.txt"), "original contents").unwrap();
    assert_eq!(
        err(
            &WriteTool,
            &ctx,
            json!({"file_path": "data.txt", "content": "x"})
        )
        .await,
        format!("cannot modify \"{display}\": file has not been read — read the file, then retry")
    );
    ok(&ReadTool, &ctx, json!({"file_path": "data.txt"})).await;
    let out = call(
        &WriteTool,
        &ctx,
        json!({"file_path": "data.txt", "content": "replaced"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("Updated file"));
    assert_eq!(
        out.meta.unwrap()["diffs"],
        json!([{"path": format!("{cwd}/data.txt"), "oldText": "original contents", "newText": "replaced"}])
    );
    // Our own write refreshed the observation: writing again needs no read.
    ok(
        &WriteTool,
        &ctx,
        json!({"file_path": "data.txt", "content": "again"}),
    )
    .await;
    // An outside change makes the observation stale.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(dir.path().join("data.txt"), "changed elsewhere!").unwrap();
    assert_eq!(
        err(
            &WriteTool,
            &ctx,
            json!({"file_path": "data.txt", "content": "x"})
        )
        .await,
        format!(
            "cannot write \"{display}\": file changed since it was read — re-read the file, then retry"
        )
    );
    // Another session has observed nothing.
    let other = builder(dir.path())
        .session("other")
        .state(std::sync::Arc::new(dshx_tools::ToolState::new()))
        .build();
    assert!(
        err(
            &WriteTool,
            &other,
            json!({"file_path": "notes.txt", "content": "x"})
        )
        .await
        .contains("file has not been read")
    );
    // Parent directories are created.
    ok(
        &WriteTool,
        &ctx,
        json!({"file_path": "deep/new/file.txt", "content": "x"}),
    )
    .await;
    assert!(dir.path().join("deep/new/file.txt").exists());
}

#[tokio::test]
async fn delete_then_recreate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("deleted.txt"), "original\n").unwrap();
    let ctx = context(dir.path());
    ok(&ReadTool, &ctx, json!({"file_path": "deleted.txt"})).await;
    std::fs::remove_file(dir.path().join("deleted.txt")).unwrap();
    err(&ReadTool, &ctx, json!({"file_path": "deleted.txt"})).await;
    let t = ok(
        &WriteTool,
        &ctx,
        json!({"file_path": "deleted.txt", "content": "fresh\n"}),
    )
    .await;
    assert!(t.contains("Created file"));
}

#[tokio::test]
async fn edit_rules_and_messages() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = real(dir.path());
    let display = format!("{}/config.txt", dir.path().display());
    std::fs::write(dir.path().join("config.txt"), "mode=DEBUG\nlevel=info\n").unwrap();
    let ctx = context(dir.path());
    let args = json!({"file_path": "config.txt", "old_string": "DEBUG", "new_string": "RELEASE"});
    assert_eq!(
        err(&EditTool, &ctx, args.clone()).await,
        format!("cannot modify \"{display}\": file has not been read — read the file, then retry")
    );
    ok(&ReadTool, &ctx, json!({"file_path": "config.txt"})).await;
    let out = call(&EditTool, &ctx, args).await.unwrap();
    assert_eq!(
        text(&out),
        format!("The file {cwd}/config.txt has been updated successfully.")
    );
    assert_eq!(
        out.meta.unwrap(),
        json!({"path": format!("{cwd}/config.txt"), "diffs": [{"path": format!("{cwd}/config.txt"), "oldText": "mode=DEBUG\nlevel=info", "newText": "mode=RELEASE\nlevel=info"}]})
    );
    // Editing again needs no read: "unless you just created or edited it".
    let t = ok(&EditTool, &ctx, json!({"file_path": "config.txt", "old_string": "i", "new_string": "I", "replace_all": true})).await;
    assert_eq!(
        t,
        format!(
            "The file {cwd}/config.txt has been updated. All occurrences were successfully replaced."
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.txt")).unwrap(),
        "mode=RELEASE\nlevel=Info\n"
    );
    assert_eq!(
        err(
            &EditTool,
            &ctx,
            json!({"file_path": "config.txt", "old_string": "e", "new_string": "E"})
        )
        .await,
        format!(
            "old_string matched 3 times in \"{display}\"; provide a more specific old_string or set replace_all to true"
        )
    );
    assert_eq!(
        err(
            &EditTool,
            &ctx,
            json!({"file_path": "config.txt", "old_string": "zzz", "new_string": "y"})
        )
        .await,
        format!("old_string was not found in \"{display}\"")
    );
    assert_eq!(
        err(
            &EditTool,
            &ctx,
            json!({"file_path": "config.txt", "old_string": "a", "new_string": "a"})
        )
        .await,
        "old_string and new_string must differ"
    );
    // A created file can be edited right away.
    ok(
        &WriteTool,
        &ctx,
        json!({"file_path": "new.txt", "content": "a\r\nb\r\n"}),
    )
    .await;
    ok(
        &EditTool,
        &ctx,
        json!({"file_path": "new.txt", "old_string": "a\nb", "new_string": "c\nd"}),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "c\r\nd\r\n"
    );
    // A missing file observed as absent.
    err(&ReadTool, &ctx, json!({"file_path": "gone.txt"})).await;
    assert_eq!(
        err(
            &EditTool,
            &ctx,
            json!({"file_path": "gone.txt", "old_string": "a", "new_string": "b"})
        )
        .await,
        format!(
            "cannot edit \"{}/gone.txt\": not found",
            dir.path().display()
        )
    );
}

#[tokio::test]
async fn sandbox_fence_for_mutations() {
    let work = tempfile::tempdir().unwrap();
    let outside =
        tempfile::tempdir_in(std::env::var("HOME").unwrap_or_else(|_| "/var/tmp".into())).unwrap();
    let outside_file = outside.path().join("x.txt").to_string_lossy().into_owned();
    let ro = builder(work.path()).sandbox(SandboxMode::ReadOnly).build();
    let denied_ro = format!(
        "{}\n{}",
        denial_marker(SandboxMode::ReadOnly),
        escalation_hint("operation")
    );
    assert_eq!(
        err(
            &WriteTool,
            &ro,
            json!({"file_path": "a.txt", "content": "x"})
        )
        .await,
        denied_ro
    );

    let ww = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .build();
    ok(
        &WriteTool,
        &ww,
        json!({"file_path": "a.txt", "content": "x"}),
    )
    .await;
    let denied_ww = format!(
        "{}\n{}",
        denial_marker(SandboxMode::WorkspaceWrite),
        escalation_hint("operation")
    );
    assert_eq!(
        err(
            &WriteTool,
            &ww,
            json!({"file_path": outside_file, "content": "x"})
        )
        .await,
        denied_ww
    );
    // Escape through a symlink inside the workspace is caught too.
    std::os::unix::fs::symlink(outside.path(), work.path().join("link")).unwrap();
    assert_eq!(
        err(
            &WriteTool,
            &ww,
            json!({"file_path": "link/y.txt", "content": "x"})
        )
        .await,
        denied_ww
    );

    let approver = FixedApprover::new(ApprovalDecision::AllowedOnce);
    let escalated = builder(work.path())
        .sandbox(SandboxMode::WorkspaceWrite)
        .approver(approver.clone())
        .build();
    let args = json!({"file_path": outside_file, "content": "x", "sandbox_permissions": "danger-full-access", "justification": "the user asked"});
    ok(&WriteTool, &escalated, args).await;
    assert_eq!(approver.asked.lock().unwrap()[0].0, "write");
    assert_eq!(
        err(
            &WriteTool,
            &ww,
            json!({"file_path": "b.txt", "content": "x", "justification": "why"})
        )
        .await,
        "invalid escalation: justification is only valid together with sandbox_permissions"
    );
}

#[tokio::test]
async fn read_image_returns_an_image_part() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = real(dir.path());
    // 1x1 PNG.
    let png: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(dir.path().join("red.png"), png).unwrap();
    let ctx = context(dir.path());
    let out = call(&ReadImageTool, &ctx, json!({"file_path": "red.png"}))
        .await
        .unwrap();
    assert_eq!(
        text(&out),
        format!(
            "<path>{cwd}/red.png</path>\n<type>image</type>\n<content>\nimage/png image, 1x1 px, 69 bytes\n</content>"
        )
    );
    let ToolResultPart::Image { attachment } = &out.content[1] else {
        panic!("no image part")
    };
    assert_eq!(attachment.media_type, "image/png");
    assert_eq!(attachment.name.as_deref(), Some("red.png"));
    assert!(attachment.attachment_id.starts_with("sha256:"));
    let (bytes, _) = ctx
        .attachments
        .load(&attachment.attachment_id)
        .await
        .unwrap();
    assert_eq!(bytes.len(), 69);
    assert_eq!(out.meta.unwrap(), json!({"path": format!("{cwd}/red.png")}));

    let text_model = builder(dir.path()).images(false).build();
    dshx_tools::ToolState::get(&text_model)
        .unwrap()
        .set_model("deepseek-v4-pro");
    assert_eq!(
        err(&ReadImageTool, &text_model, json!({"file_path": "red.png"})).await,
        "cannot read \"red.png\" as an image: model \"deepseek-v4-pro\" does not declare image input; switch to an image-capable model to read images"
    );
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    assert_eq!(
        err(&ReadImageTool, &ctx, json!({"file_path": "notes.txt"})).await,
        "cannot read \"notes.txt\": the .txt extension does not declare a supported image format; read_image accepts PNG/JPEG/WebP/GIF files, including extension-less files in those formats"
    );
    std::fs::write(dir.path().join("noext"), png).unwrap();
    let out = call(&ReadImageTool, &ctx, json!({"file_path": "noext"}))
        .await
        .unwrap();
    assert!(text(&out).contains("image/png image"));
}
