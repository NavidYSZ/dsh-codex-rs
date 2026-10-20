//! The subagent scenarios recorded from the official dsh 0.2.0-rc.2 (SDK
//! host against `examples/subagent_mock.rs`), replayed with our runtime: the
//! assertions pin the event shapes and orders dsh produced.
mod support;

use std::time::Duration;

use dshx_subagent::{DELEGATION_CONTEXT, SubagentSettings};
use serde_json::{Value, json};
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_spawn_runs_a_one_shot_child() {
    let h = harness().await;
    h.prompt("SCENARIO spawn-fg").await;
    let parent = h.parent_events();
    assert_eq!(tool_results(&parent), ["CHILD BETA DONE"]);
    let [child] = h.children().try_into().unwrap();
    // The catalog fact lands during the tool call, before its result.
    let catalog = find(&parent, "subagent/catalog")[0];
    assert_eq!(catalog.data["version"], 0);
    assert_eq!(catalog.data["mode"], "one-shot");
    assert_eq!(catalog.data["label"], "Child beta task");
    let result = find(&parent, "tool/result")[0];
    assert!(catalog.seq < result.seq);

    let (header, events) = h.read(&child);
    assert_eq!(header.delegation_depth, 1);
    assert!(!header.is_seeded);
    assert_eq!(header.extra["parentSession"], json!(h.parent_id()));
    assert_eq!(header.extra["origin"], "subagent");
    assert_eq!(header.cwd, h.cwd.to_str().unwrap());
    assert_eq!(
        kinds(&events[..7]),
        [
            "sandbox/mode",
            "approval/policy",
            "agent/inbox/spliced",
            "turn/start",
            "agent/inbox/spliced",
            "subagent/descriptor",
            "step/start",
        ]
    );
    assert_eq!(
        events[0].data,
        json!({"mode": "workspace-write", "source": "delegation"})
    );
    assert_eq!(
        events[1].data,
        json!({"policy": "never", "source": "delegation"})
    );
    assert_eq!(
        events[5].data,
        json!({"version": 3, "mode": "one-shot", "provider": "spawn", "label": "Child beta task"})
    );
    // Delegation statement after the approval sentence of the runtime context.
    let context = events
        .iter()
        .find(|e| e.kind == "user/message" && e.data["source"]["kind"] == "runtime-context")
        .unwrap();
    let sections = context.data["source"]["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[2]["name"], "subagent:delegation");
    assert_eq!(sections[2]["text"], DELEGATION_CONTEXT);
    assert!(
        sections[1]["text"]
            .as_str()
            .unwrap()
            .starts_with("Approval prompts are disabled")
    );
    // The child has its parent's tools, its route explicit, no LLM title.
    let request = find(&events, "request/header")[0];
    assert_eq!(
        request.data["header"]["config"],
        json!({"provider": "deepseek-official", "model": "deepseek-flash", "reasoningEffort": "high", "maxTokens": 256000})
    );
    assert_eq!(
        request.data["header"]["adapterDefaults"],
        json!({"maxTokens": true})
    );
    let tools: Vec<&str> = request.data["header"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"subagent") && tools.contains(&"send_message"));
    assert!(find(&events, "session/title-llm-request").is_empty());
    // Depth 2 is refused; a one-shot child has no catalog of its own.
    assert_eq!(
        tool_results(&events),
        [
            "Error: subagent depth 2 exceeds maxDepth 1",
            "(no subagents)"
        ]
    );
    assert_eq!(turn_ends(&events), [json!({"kind": "completed"})]);

    let started = h.recorder.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].child_session_id, child);
    let finished = h.recorder.finished.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&finished[0]).unwrap(),
        json!({
            "provider": "spawn", "agentId": child, "parentSessionId": h.parent_id(),
            "childSessionId": child, "status": "ok", "stopReason": "completed",
            "lastAssistantMessage": [{"type": "text", "text": "CHILD BETA DONE"}],
        })
    );
    assert!(
        h.runtime.child(&child).is_none(),
        "the run released its child"
    );
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_spawn_reports_back_and_settles() {
    let h = harness().await;
    h.prompt("SCENARIO spawn-bg").await;
    let parent = h.parent_events();
    let [child] = h.children().try_into().unwrap();
    assert_eq!(tool_results(&parent), [format!("started subagent {child}")]);
    let catalog = find(&parent, "subagent/catalog")[0];
    assert_eq!(catalog.data["mode"], "continuable");

    let (_, events) = h.read(&child);
    assert_eq!(
        kinds(&events[..3]),
        ["subagent/descriptor", "sandbox/mode", "approval/policy"]
    );
    assert_eq!(
        events[0].data,
        json!({
            "version": 3, "mode": "continuable", "provider": "spawn", "label": "Child alpha task",
            "agentProvider": "deepseek-official", "agentModel": "deepseek-flash", "agentReasoningEffort": "high",
        })
    );
    // The initial task carries the return guidance as its own block.
    let task = &find(&events, "agent/inbox/spliced")[0].data["inserted"][0];
    assert_eq!(task["source"], json!({"kind": "user"}));
    assert_eq!(
        task["content"][0]["text"],
        "CHILD-TASK alpha: run a command and report"
    );
    let guidance = task["content"][1]["text"].as_str().unwrap();
    assert!(guidance.starts_with(&format!("Your parent agent id is \"{}\".", h.parent_id())));
    assert_eq!(
        tool_results(&events),
        [
            "child-alpha\n".to_owned(),
            format!("message delivered to agent {}", h.parent_id())
        ]
    );

    // Parent: turn 2 from the relayed message (next-step), turn 3 from the notice (next-turn).
    let starts = find(&parent, "turn/start");
    assert_eq!(starts.len(), 3);
    let splices: Vec<&Value> = find(&parent, "agent/inbox/spliced")
        .into_iter()
        .filter(|e| e.data["inserted"].as_array().is_some_and(|a| !a.is_empty()))
        .map(|e| &e.data)
        .collect();
    assert_eq!(splices[1]["target"], "next-step");
    assert_eq!(
        splices[1]["inserted"][0]["source"],
        json!({"kind": "agent-message", "form": "relay", "senderSessionId": child})
    );
    assert_eq!(
        user_texts(&parent, "agent-message"),
        [format!("Agent {child} sent a message: |alpha result: ok")]
    );
    assert_eq!(splices[2]["target"], "next-turn");
    let summary = format!(
        "Background subagent {child} finished and will do no further work unless you send it more."
    );
    assert_eq!(
        splices[2]["inserted"][0]["source"],
        json!({"kind": "subagent-settled", "form": "notice", "summary": summary, "senderSessionId": child})
    );
    assert_eq!(
        user_texts(&parent, "subagent-settled"),
        [format!("{summary}|Its closing message:|CHILD ALPHA DONE")]
    );
    let finished = h.recorder.finished.lock().unwrap().clone();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].stop_reason, "completed");
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fork_children_inherit_completed_turns() {
    let h = harness().await;
    h.prompt("SCENARIO warmup").await;
    h.prompt("SCENARIO fork-fg").await;
    h.prompt("SCENARIO fork-bg").await;
    let parent = h.parent_events();
    let [fg, bg] = h.children().try_into().unwrap();
    let results = tool_results(&parent);
    assert_eq!(results[0], "FORK DONE");
    assert!(results[1].starts_with("started background subagent job subagent-"));
    let job = results[1].rsplit(' ').next().unwrap().to_owned();
    assert_eq!(results[2], "FORK BG DONE\n[status: completed]");
    assert_eq!(
        user_texts(&parent, "tool-jobs"),
        [format!(
            "background job {job} (subagent: Fork background) finished [status: completed]. Read its output with job_output."
        )]
    );

    // The fg fork: the parent's log through turn 1's end, the inherited cut, then its own turn 2.
    let (header, events) = h.read(&fg);
    assert!(header.is_seeded);
    let first_end = parent.iter().position(|e| e.kind == "turn/end").unwrap();
    for (ours, theirs) in events.iter().zip(&parent[..=first_end]) {
        assert_eq!((&ours.kind, &ours.data), (&theirs.kind, &theirs.data));
    }
    let cut = first_end + 1;
    assert_eq!(events[cut].kind, "session/end-seed");
    assert_eq!(events[cut].data, json!({"inherited": true}));
    assert_eq!(
        kinds(&events[cut + 1..cut + 8]),
        [
            "sandbox/mode",
            "approval/policy",
            "agent/inbox/spliced",
            "turn/start",
            "agent/inbox/spliced",
            "subagent/descriptor",
            "step/start",
        ]
    );
    assert_eq!(events[cut + 4].data["turn"], 2);
    assert_eq!(events[cut + 6].data["provider"], "fork");
    let headers = find(&events, "request/header");
    assert_eq!(headers.last().unwrap().data["reason"], "resume");
    // No second system prompt and no title: both are inherited.
    assert_eq!(find(&events, "system/message").len(), 1);
    assert_eq!(
        find(&events, "session/title").len(),
        find(&parent[..cut], "session/title").len()
    );

    // The bg fork sees turns 1 and 2, not the in-flight turn 3.
    let (header, events) = h.read(&bg);
    assert!(header.is_seeded);
    let cut = events
        .iter()
        .position(|e| e.kind == "session/end-seed" && e.data["inherited"] == true)
        .unwrap();
    assert_eq!(events[cut - 1].kind, "turn/end");
    assert_eq!(events[cut - 1].data["turn"], 2);
    assert_eq!(
        turn_ends(&events).last().unwrap(),
        &json!({"kind": "completed"})
    );
    let finished = h.recorder.finished.lock().unwrap().clone();
    assert_eq!(
        finished
            .iter()
            .map(|f| f.provider.as_str())
            .collect::<Vec<_>>(),
        ["fork", "fork"]
    );
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_interrupt_and_cold_resume() {
    let h = harness().await;
    h.prompt("SCENARIO control").await;
    let parent = h.parent_events();
    let [child] = h.children().try_into().unwrap();
    let results = tool_results(&parent);
    assert_eq!(
        results,
        [
            format!("started subagent {child}"),
            format!("{child} [running] — Slow child"),
            format!(
                "{child} [running] parent={} depth=1 — Slow child",
                h.parent_id()
            ),
            format!("interrupt requested for agent {child}"),
            format!("message delivered to agent {child}"),
        ]
    );
    let notices = user_texts(&parent, "subagent-settled");
    assert_eq!(
        notices,
        [
            format!(
                "Background subagent {child} was stopped before it finished.|It left no closing message."
            ),
            format!(
                "Background subagent {child} finished and will do no further work unless you send it more.|Its closing message:|SLOW CHILD RESUMED"
            ),
        ]
    );

    let (_, events) = h.read(&child);
    assert_eq!(
        turn_ends(&events),
        [
            json!({"kind": "aborted", "reason": {"kind": "parent"}}),
            json!({"kind": "completed"})
        ]
    );
    assert_eq!(tool_results(&events), ["Error: tool call aborted"]);
    // Cold resume: restore marker, then the relayed message as a steer into a new turn.
    let marker = events
        .iter()
        .position(|e| e.kind == "session/end-seed")
        .unwrap();
    assert_eq!(events[marker].data, json!({}));
    assert_eq!(events[marker + 1].kind, "agent/inbox/spliced");
    assert_eq!(events[marker + 1].data["target"], "next-step");
    assert_eq!(
        events[marker + 1].data["inserted"][0]["content"],
        json!([
            {"type": "text", "text": format!("Agent {} sent a message: ", h.parent_id())},
            {"type": "text", "text": "Please finish now."},
        ])
    );
    assert_eq!(events[marker + 2].kind, "turn/start");
    assert_eq!(find(&events, "subagent/descriptor").len(), 1);

    let finished = h.recorder.finished.lock().unwrap().clone();
    let reasons: Vec<(&str, &str)> = finished
        .iter()
        .map(|f| (f.stop_reason.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(reasons, [("aborted", "error"), ("completed", "ok")]);
    assert_eq!(
        finished[0].last_assistant_message.as_ref().unwrap()[0]["type"],
        "tool-call"
    );
    assert_eq!(
        h.recorder.started.lock().unwrap().len(),
        2,
        "creation and cold resume"
    );
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_requests_and_failed_jobs() {
    let h = harness().await;
    h.runtime.set_settings(SubagentSettings {
        max_depth: 0,
        ..SubagentSettings::default()
    });
    h.prompt("SCENARIO refused").await;
    let parent = h.parent_events();
    let results = tool_results(&parent);
    assert_eq!(
        results[0],
        "Error: subagent \"no-such-agent\" is unavailable"
    );
    assert_eq!(results[1], "interrupt requested for agent no-such-agent");
    assert!(results[2].starts_with("started background subagent job subagent-"));
    let job = results[2].rsplit(' ').next().unwrap();
    assert_eq!(
        user_texts(&parent, "tool-jobs"),
        [format!(
            "background job {job} (subagent: Fork job) finished [status: failed, SubagentDepthError: subagent depth 1 exceeds maxDepth 0]. Read its output with job_output."
        )]
    );
    assert!(h.children().is_empty());
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_the_parent_cancels_a_foreground_child() {
    let h = harness().await;
    let agent = h.agent.clone();
    let turn = tokio::spawn(async move {
        agent
            .run_turn(vec![dshx_core::ContentBlock::text("SCENARIO cancel-fg")])
            .await
            .unwrap()
    });
    // Wait until the child sleeps in its bash call.
    let child = loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(id) = h.children().first().cloned()
            && h.runtime
                .child(&id)
                .is_some_and(|c| c.events().iter().any(|e| e.kind == "tool/call"))
        {
            break id;
        }
    };
    h.agent.cancel().await;
    let outcome = turn.await.unwrap();
    assert_eq!(
        serde_json::to_value(&outcome.reason).unwrap(),
        json!({"kind": "aborted", "reason": {"kind": "user"}})
    );
    let parent = h.parent_events();
    assert_eq!(tool_results(&parent), ["Error: tool call aborted"]);
    tokio::time::timeout(Duration::from_secs(10), h.runtime.wait_idle(&h.agent))
        .await
        .unwrap();
    let (_, events) = h.read(&child);
    assert_eq!(
        turn_ends(&events),
        [json!({"kind": "aborted", "reason": {"kind": "parent"}})]
    );
    let finished = h.recorder.finished.lock().unwrap().clone();
    assert_eq!(finished[0].stop_reason, "aborted");
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draining_stops_background_children_without_notices() {
    let h = harness().await;
    h.agent
        .run_turn(vec![dshx_core::ContentBlock::text("SCENARIO drain")])
        .await
        .unwrap();
    let [child] = h.children().try_into().unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if h.runtime
            .child(&child)
            .is_some_and(|c| c.events().iter().any(|e| e.kind == "tool/call"))
        {
            break;
        }
    }
    let before = h.agent.log_len();
    tokio::time::timeout(
        Duration::from_secs(10),
        h.runtime.drain_descendants(&h.parent_id()),
    )
    .await
    .unwrap();
    assert!(h.runtime.child(&child).is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.agent.log_len(),
        before,
        "no settlement notice during teardown"
    );
    let (_, events) = h.read(&child);
    assert_eq!(
        turn_ends(&events),
        [json!({"kind": "aborted", "reason": {"kind": "parent"}})]
    );
    h.validate_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deeper_delegation_when_allowed() {
    let h = harness().await;
    h.runtime.set_settings(SubagentSettings {
        max_depth: 2,
        ..SubagentSettings::default()
    });
    h.prompt("SCENARIO spawn-fg").await;
    let [child] = h.children().try_into().unwrap();
    let (_, events) = h.read(&child);
    assert_eq!(tool_results(&events)[0], "CHILD DONE");
    let [grandchild] = catalog_children(&events).try_into().unwrap();
    let (header, _) = h.read(&grandchild);
    assert_eq!(header.delegation_depth, 2);
    assert_eq!(header.extra["parentSession"], json!(child));
    h.validate_all().await;
    assert!(h.log.events().iter().any(|e| e.kind == "subagent/catalog"));
}
