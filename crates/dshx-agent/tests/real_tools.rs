//! The loop with the real `dshx-tools`: their results for dsh's reference
//! calls, background jobs (events and completion wake-ups) and the inline
//! size limit.
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use dshx_agent::testing::{ScriptedLlm, text, tool_calls};
use dshx_core::{ContentBlock, SessionEvent, SessionLog, Tool};
use serde_json::{Value, json};

const M: &str = "deepseek-flash";

fn real_tools() -> Vec<Arc<dyn Tool>> {
    dshx_tools::default_tools(&dshx_core::config::Config::default())
}

#[tokio::test]
async fn real_tools_answer_dsh_reference_calls_identically() {
    let h = harness(vec![], real_tools(), |_, _| {});
    let cwd = h.agent.config().cwd;
    let c = cwd.to_str().unwrap();
    std::fs::write(cwd.join("a.txt"), "hello a\n").unwrap();
    let a = cwd.join("a.txt");
    h.llm.push(tool_calls(
        M,
        &[
            ("c1", "read", &json!({"file_path": a}).to_string()),
            ("c2", "bash", "{\"command\": \"echo hi\", \"description\": \"say hi\"}"),
            ("c3", "todo_write", "{\"todos\": [{\"content\": \"Do it\", \"status\": \"in_progress\"}, {\"content\": \"Then that\", \"status\": \"pending\"}]}"),
        ],
    ));
    h.llm.push(text(M, "done"));
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let results = of_kind(&h.log, "tool/result");
    // Texts exactly as the official dsh returned them for the same calls.
    assert_eq!(
        text_of_result(&results[0]),
        format!(
            "<path>{c}/a.txt</path>\n<type>file</type>\n<content>\n1: hello a\n\n(End of file - total 1 lines)\n</content>"
        )
    );
    assert_eq!(
        results[0].data["meta"],
        json!({"path": format!("{c}/a.txt"), "offset": 1, "lines": [{"number": 1, "text": "hello a"}], "totalLines": 1})
    );
    assert_eq!(text_of_result(&results[1]), "hi\n");
    assert_eq!(
        text_of_result(&results[2]),
        "Updated todo list: 1 pending, 1 in progress, 0 completed."
    );
    assert_eq!(
        of_kind(&h.log, "todo/write")[0].data,
        json!({"todos": [{"content": "Do it", "status": "in_progress"}, {"content": "Then that", "status": "pending"}]})
    );
    validate(&h.log);
}

async fn wait_for(log: &dshx_session::MemoryLog, pred: impl Fn(&[SessionEvent]) -> bool) {
    for _ in 0..400 {
        if pred(&log.events()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_job_events_and_completion_wake_up() {
    let llm = ScriptedLlm::new(vec![
        tool_calls(
            M,
            &[(
                "c1",
                "bash",
                "{\"command\": \"sleep 0.3; echo bg-done\", \"description\": \"bg\", \"run_in_background\": true}",
            )],
        ),
        text(M, "started"),
        text(M, "noted the job"),
    ]);
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let log = Arc::new(dshx_session::MemoryLog::new(cwd.to_str().unwrap()));
    let h = with_log(llm, real_tools(), dir, cwd, log, |_, _| {});
    h.agent
        .run_turn(vec![ContentBlock::text("start a job")])
        .await
        .unwrap();
    let k = kinds(&h.log);
    let call = k.iter().position(|x| x == "tool/call").unwrap();
    assert_eq!(
        &k[call..call + 3],
        ["tool/call", "job/registered", "tool/result"]
    );
    // The job settles while the agent is idle: logged, then a wake-up turn.
    wait_for(&h.log, |e| {
        e.iter().filter(|e| e.kind == "turn/end").count() == 2
    })
    .await;
    let all = h.log.events();
    let settled = all.iter().position(|e| e.kind == "job/settled").unwrap();
    assert_eq!(all[settled].data["status"], "completed");
    let notice = all
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "tool-jobs")
        .expect("notice entered the wake-up turn");
    assert_eq!(notice.data["source"]["form"], "notice");
    assert!(
        notice.data["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("bg-done"),
        "{}",
        notice.data
    );
    assert!(notice.seq > all[settled].seq);
    validate(&h.log);
}

#[tokio::test]
async fn oversized_results_are_bounded_inline() {
    let h = harness(vec![], real_tools(), |_, _| {});
    h.llm.push(tool_calls(
        M,
        &[(
            "c1",
            "bash",
            "{\"command\": \"head -c 120000 /dev/zero | tr '\\\\0' x\", \"description\": \"big\"}",
        )],
    ));
    h.llm.push(text(M, "ok"));
    h.agent
        .run_turn(vec![ContentBlock::text("go")])
        .await
        .unwrap();
    let result = &of_kind(&h.log, "tool/result")[0];
    let t = text_of_result(result);
    assert!(t.len() < 60_000, "{}", t.len());
    assert!(
        t.contains("Full formatted result stored at:"),
        "{}",
        &t[t.len() - 300..]
    );
    let _: &Value = &result.data;
    validate(&h.log);
}
