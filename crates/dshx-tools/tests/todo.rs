mod common;

use common::{call, err, text};
use dshx_tools::testing::context;
use dshx_tools::todo::TodoWriteTool;
use serde_json::json;

#[tokio::test]
async fn todo_write_counts_and_returns_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let todos = json!([
        {"content": "read the code", "status": "in_progress"},
        {"content": "watch the background build", "status": "in_progress"},
        {"content": " write the fix ", "status": "pending"}
    ]);
    let out = call(&TodoWriteTool, &ctx, json!({"todos": todos}))
        .await
        .unwrap();
    assert_eq!(
        text(&out),
        "Updated todo list: 1 pending, 2 in progress, 0 completed."
    );
    let value = out.value.unwrap();
    assert_eq!(
        value["todos"][2],
        json!({"content": "write the fix", "status": "pending"})
    );
    assert_eq!(
        value["counts"],
        json!({"pending": 1, "inProgress": 2, "completed": 0})
    );
    assert_eq!(
        err(&TodoWriteTool, &ctx, json!({"todos": [{"content": "a", "status": "pending"}, {"content": "a ", "status": "completed"}]})).await,
        "invalid todos: duplicate content \"a\""
    );
    assert_eq!(
        err(
            &TodoWriteTool,
            &ctx,
            json!({"todos": [{"content": " ", "status": "pending"}]})
        )
        .await,
        "invalid todo: `content` must be a non-empty string"
    );
    assert_eq!(
        err(
            &TodoWriteTool,
            &ctx,
            json!({"todos": [{"content": "a", "status": "done"}]})
        )
        .await,
        "invalid arguments: \"todos[0].status\" must be one of [\"pending\",\"in_progress\",\"completed\"]"
    );
}
