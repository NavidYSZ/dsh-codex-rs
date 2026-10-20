//! Standalone mock for manual runs and golden captures with the official dsh:
//!
//! ```text
//! cargo run -p dshx-testkit --example mock_deepseek -- PORT SCENARIO [DUMP_DIR]
//! DEEPSEEK_API_KEY=sk-mock DEEPSEEK_BASE_URL=http://127.0.0.1:PORT/anthropic dsh --profile headless "..."
//! ```
//!
//! Scenarios (the step is the number of tool results already in the history):
//! - `thinking`: signed thinking + text + bash call, signed thinking + bash call, signed thinking + text.
//! - `image`: a `read_image` call for `pixel.png`, then text.
//! - `text`: one signed thinking block and a text answer.
//!
//! Title requests (system prompt asks for a concise title) get `Testtitel`.
//! With DUMP_DIR every Messages request is saved as `NNN.json`
//! (`{path, headers, body}`) plus the exact bytes as `NNN.raw.json`.
use std::sync::atomic::{AtomicUsize, Ordering};

use dshx_testkit::{MockDeepSeek, scenarios};
use serde_json::json;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = args
        .first()
        .map(String::as_str)
        .unwrap_or("18999")
        .to_string();
    let scenario = args.get(1).cloned().unwrap_or_else(|| "thinking".into());
    let dump = args.get(2).cloned();
    let mock = MockDeepSeek::start_on(&format!("127.0.0.1:{port}"))
        .await
        .expect("bind");
    let counter = AtomicUsize::new(0);
    mock.set_responder(move |request| {
        if let Some(dir) = &dump {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let headers: serde_json::Map<String, serde_json::Value> = request
                .headers
                .iter()
                .filter(|(k, _)| k != "x-api-key" && k != "authorization")
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            let meta = json!({"path": request.path, "headers": headers, "body": request.body});
            std::fs::write(
                format!("{dir}/{n:03}.json"),
                serde_json::to_string_pretty(&meta).unwrap(),
            )
            .unwrap();
            std::fs::write(format!("{dir}/{n:03}.raw.json"), &request.raw_body).unwrap();
        }
        scenarios::reply(&scenario, request)
    });
    eprintln!(
        "mock DeepSeek on {} (scenario {})",
        mock.base_url(),
        args.get(1).map(String::as_str).unwrap_or("thinking")
    );
    tokio::signal::ctrl_c().await.ok();
}
