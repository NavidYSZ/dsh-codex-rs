//! Runs the subagent scenarios with our runtime against a mock DeepSeek, so
//! the logs can be compared with the official dsh's (`examples/subagent_mock.rs`):
//!
//! ```text
//! cargo run -p dshx-subagent --example subagent_run -- STORE_ROOT CWD BASE_URL SESSION_ID PROMPT...
//! ```
//!
//! Every prompt runs as one turn, then the host waits until the parent and
//! all children are idle (like an SDK host). Lifecycle notifications are
//! printed as SDK-style JSON lines.
use std::sync::Arc;

use dshx_agent::testing::{FixedApprover, MemoryAttachments};
use dshx_agent::{Agent, AgentConfig, AgentDeps};
use dshx_core::{ApprovalDecision, ContentBlock, LlmClient, SessionLog, ToolRegistry};
use dshx_llm::{DeepSeekClient, DeepSeekConfig, StaticApiKey};
use dshx_session::SessionStore;
use dshx_subagent::{
    RuntimeDeps, StoreSessions, SubagentFinished, SubagentObserver, SubagentRuntime,
    SubagentStarted, subagent_tools,
};
use serde_json::json;

struct Print;

impl SubagentObserver for Print {
    fn on_started(&self, e: &SubagentStarted) {
        println!("{}", json!({"method": "subagent.started", "params": e}));
    }
    fn on_finished(&self, e: &SubagentFinished) {
        println!("{}", json!({"method": "subagent.finished", "params": e}));
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [root, cwd, base_url, session_id, prompts @ ..] = args.as_slice() else {
        anyhow::bail!("usage: subagent_run STORE_ROOT CWD BASE_URL SESSION_ID PROMPT...");
    };
    let mut config = DeepSeekConfig {
        base_url: base_url.clone(),
        ..DeepSeekConfig::default()
    };
    config.send_session_id = false;
    let llm: Arc<dyn LlmClient> = Arc::new(
        DeepSeekClient::builder(config)
            .api_key(Arc::new(StaticApiKey("sk-mock".into())))
            .build()?,
    );
    let store = Arc::new(SessionStore::new(root));
    let runtime = SubagentRuntime::new(RuntimeDeps {
        llm: llm.clone(),
        sessions: Arc::new(StoreSessions::new(store.clone())),
        attachments: Arc::new(MemoryAttachments::default()),
        approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
    });
    runtime.set_observer(Arc::new(Print));

    let log = match store.locate(session_id)? {
        Some(_) => store.resume(session_id).await?,
        None => {
            let mut header = SessionStore::new_header(cwd);
            header.id = session_id.clone();
            store.create_with_header(header)?
        }
    };
    let mut agent_config = AgentConfig::new("deepseek-flash", cwd);
    let home = std::env::temp_dir().join("dshx-subagent-run-home");
    agent_config.instructions.harness_home = home.join(".dsh-codex");
    agent_config.instructions.agents_home = home.join(".agents");
    let mut tools = ToolRegistry::new();
    for tool in dshx_tools::default_tools(&dshx_core::config::Config::default()) {
        tools.register(tool);
    }
    for tool in subagent_tools(runtime.clone()) {
        tools.register(tool);
    }
    let log: Arc<dyn SessionLog> = Arc::new(log.clone());
    let agent = Agent::new(
        agent_config,
        AgentDeps {
            llm,
            tools,
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::Rejected)),
            observer: None,
            tool_state: None,
        },
    );
    runtime.attach(&agent);
    for prompt in prompts {
        let outcome = agent.run_turn(vec![ContentBlock::text(prompt)]).await?;
        eprintln!("turn: {}", serde_json::to_string(&outcome)?);
        runtime.wait_idle(&agent).await;
    }
    runtime.drain_descendants(&agent.session_id()).await;
    agent.dispose().await;
    runtime.detach(&agent.session_id());
    log.flush().await?;
    Ok(())
}
