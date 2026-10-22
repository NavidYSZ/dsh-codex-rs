//! Provider routes: an agent configured for another provider talks to that
//! route's client (main loop and titles), records its provider in the log,
//! and goes back to the base client when switched.
use std::sync::Arc;

use async_trait::async_trait;
use dshx_agent::testing::{FixedApprover, MemoryAttachments, ScriptedLlm};
use dshx_agent::{Agent, AgentConfig, AgentDeps, RouteCapabilities};
use dshx_core::{
    ApprovalDecision, ContentBlock, GenerateRequest, LlmClient, ModelInfo, SessionLog, StreamChunk,
    ToolRegistry,
};
use dshx_session::MemoryLog;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

/// A scripted client that answers as another provider.
struct Named {
    provider: &'static str,
    inner: Arc<ScriptedLlm>,
}

#[async_trait]
impl LlmClient for Named {
    fn provider(&self) -> &str {
        self.provider
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "claude-x".into(),
            name: "Claude X".into(),
            context_window: 200_000,
            input_modalities: vec!["text".into()],
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
        }]
    }
    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        self.inner.stream(request, cancel).await
    }
}

/// The base client plus one more route.
struct Routes {
    base: Arc<ScriptedLlm>,
    other: Arc<Named>,
}

#[async_trait]
impl LlmClient for Routes {
    fn provider(&self) -> &str {
        self.base.provider()
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.base.models()
    }
    async fn stream(
        &self,
        request: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, StreamChunk> {
        self.base.stream(request, cancel).await
    }
    fn for_provider(&self, provider: &str) -> Option<Arc<dyn LlmClient>> {
        (provider == self.other.provider).then(|| self.other.clone() as Arc<dyn LlmClient>)
    }
}

#[tokio::test]
async fn an_agent_follows_its_provider_route() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let base = ScriptedLlm::new(vec![]);
    let other = ScriptedLlm::new(vec![]);
    let routes = Arc::new(Routes {
        base: base.clone(),
        other: Arc::new(Named {
            provider: "anthropic",
            inner: other.clone(),
        }),
    });
    let log = Arc::new(MemoryLog::new(cwd.to_str().unwrap()));
    let mut config = AgentConfig::new("claude-x", &cwd);
    config.provider = Some("anthropic".into());
    config.title.model = Some("claude-cheap".into());
    config.instructions.harness_home = dir.path().join("home/.dsh-codex");
    config.instructions.agents_home = dir.path().join("home/.agents");
    config.route = Some(RouteCapabilities {
        context_window: Some(200_000),
        system_prompt_in_history: false,
        tool_update: None,
        default_max_tokens: 64_000,
        default_effort: "high".into(),
    });
    let agent = Agent::new(
        config,
        AgentDeps {
            llm: routes,
            tools: ToolRegistry::new(),
            log: log.clone(),
            attachments: Arc::new(MemoryAttachments::default()),
            approver: Arc::new(FixedApprover(ApprovalDecision::AllowedOnce)),
            observer: None,
            tool_state: None,
        },
    );
    assert_eq!(agent.provider(), "anthropic");
    agent
        .run_turn(vec![ContentBlock::text("hello")])
        .await
        .unwrap();
    assert_eq!((other.request_count(), base.request_count()), (1, 0));
    assert_eq!(other.requests.lock().unwrap()[0].model, "claude-x");

    // The title went to the route's title model.
    for _ in 0..200 {
        if !other.side_requests.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(other.side_requests.lock().unwrap()[0].model, "claude-cheap");
    assert!(base.side_requests.lock().unwrap().is_empty());

    let events = log.events();
    let of = |kind: &str| events.iter().filter(|e| e.kind == kind).collect::<Vec<_>>();
    assert_eq!(
        of("request/header")[0].data["header"]["config"]["provider"],
        "anthropic"
    );
    assert_eq!(of("request/context")[0].data["provider"], "anthropic");
    assert_eq!(of("request/context")[0].data["contextWindow"], 200_000);
    let source = &of("assistant/message")[0].data["message"]["source"];
    assert_eq!(
        (source["provider"].as_str(), source["model"].as_str()),
        (Some("anthropic"), Some("claude-x"))
    );

    // Switching back routes the next turn to the base client.
    agent.update_config(|c| {
        c.provider = Some("deepseek-official".into());
        c.model = "deepseek-flash".into();
        c.route = None;
    });
    assert_eq!(agent.provider(), "deepseek-official");
    agent
        .run_turn(vec![ContentBlock::text("again")])
        .await
        .unwrap();
    assert_eq!((other.request_count(), base.request_count()), (1, 1));
    let events = log.events();
    let contexts: Vec<&str> = events
        .iter()
        .filter(|e| e.kind == "request/context")
        .map(|e| e.data["provider"].as_str().unwrap())
        .collect();
    assert_eq!(contexts, ["anthropic", "deepseek-official"]);
}
