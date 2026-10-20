//! The scripted conversations used for the golden captures in
//! `crates/dshx-llm/tests/fixtures` (recorded from the official dsh against
//! this mock). Replaying them lets tests compare our assembled messages with
//! what dsh persisted for the very same SSE.
use serde_json::json;

use crate::{RecordedRequest, Reply};

/// Whether `request` is dsh's session-title request.
pub fn is_title_request(request: &RecordedRequest) -> bool {
    request
        .system()
        .is_some_and(|s| s.contains("concise title"))
}

/// The reply of scenario `name` (`thinking`, `image`, `text`) for `request`.
/// The step is the number of tool results already in the history; tool call
/// ids are `toolu_<name>_<NN>` with NN = assistant turns so far + 1.
pub fn reply(name: &str, request: &RecordedRequest) -> Reply {
    if is_title_request(request) {
        return Reply::text("Testtitel");
    }
    let step = request.tool_results();
    let assistants = request
        .messages()
        .iter()
        .filter(|m| m["role"] == "assistant")
        .count();
    step_reply(name, step, &format!("toolu_{name}_{:02}", assistants + 1))
}

/// The reply of scenario `name` at `step`, with tool call id `id`.
pub fn step_reply(name: &str, step: usize, id: &str) -> Reply {
    match (name, step) {
        ("thinking", 0) => Reply::stream()
            .thinking(
                "The user wants two commands. Start with echo.",
                "sig-step1-AAAA",
            )
            .text("Running the first command.")
            .tool_call(
                id,
                "bash",
                json!({"command": "echo one", "description": "Print one"}),
            )
            .usage(1200, 40)
            .cache(800, 0)
            .into(),
        ("thinking", 1) => Reply::stream()
            .thinking("Now the second one.", "sig-step2-BBBB")
            .tool_call(
                id,
                "bash",
                json!({"command": "pwd", "description": "Print the directory"}),
            )
            .usage(300, 30)
            .cache(1900, 100)
            .into(),
        ("thinking", _) | ("text", _) => Reply::stream()
            .thinking("Both are done; answer briefly.", "sig-final-CCCC")
            .text("FERTIG")
            .usage(50, 5)
            .cache(2300, 0)
            .into(),
        ("image", 0) => Reply::tool_call(id, "read_image", json!({"file_path": "pixel.png"})),
        _ => Reply::text("FERTIG"),
    }
}
