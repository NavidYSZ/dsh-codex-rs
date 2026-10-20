//! Mock DeepSeek for the subagent golden runs with the official dsh:
//!
//! ```text
//! cargo run -p dshx-subagent --example subagent_mock -- PORT [DUMP_DIR]
//! DEEPSEEK_API_KEY=sk-mock DEEPSEEK_BASE_URL=http://127.0.0.1:PORT/anthropic \
//!   dsh --profile headless "SCENARIO spawn-bg"
//! ```
//!
//! Scenarios: see `tests/support/scenarios.rs`.
use std::sync::atomic::{AtomicUsize, Ordering};

use dshx_testkit::MockDeepSeek;
use serde_json::json;

#[path = "../tests/support/scenarios.rs"]
mod scenarios;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = args.first().cloned().unwrap_or_else(|| "18990".into());
    let dump = args.get(1).cloned();
    let mock = MockDeepSeek::start_on(&format!("127.0.0.1:{port}"))
        .await
        .expect("bind");
    let counter = AtomicUsize::new(0);
    mock.set_responder(move |request| {
        if let Some(dir) = &dump {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let meta = json!({"path": request.path, "body": request.body});
            let _ = std::fs::write(
                format!("{dir}/{n:03}.json"),
                serde_json::to_string_pretty(&meta).unwrap(),
            );
        }
        scenarios::reply(request)
    });
    eprintln!("subagent mock on {}", mock.base_url());
    tokio::signal::ctrl_c().await.ok();
}
