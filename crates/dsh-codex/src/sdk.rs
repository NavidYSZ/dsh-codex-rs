//! `dsh-codex sdk` (also `--profile sdk`): the dsh SDK JSON-RPC protocol on stdio
//! (`packages/sdk/server/src/server.ts`), so the official Python SDK can drive us.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use dshx_agent::{Agent, AgentObserver, AgentStatus, TurnEndReason};
use dshx_core::{AttachmentStore, ContentBlock, Message, NewImage, SessionLog};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::jsonrpc::{self, Handler, Notifier, RpcError};
use crate::runtime::{NoApprover, Overrides, Runtime};

pub async fn run(runtime: Arc<Runtime>) -> anyhow::Result<i32> {
    let (notifier, outgoing) = jsonrpc::channel();
    let server = Arc::new(SdkServer {
        runtime: runtime.clone(),
        notifier: notifier.clone(),
        state: Mutex::new(State::default()),
        sessions: tokio::sync::Mutex::new(HashMap::new()),
    });
    jsonrpc::serve(
        tokio::io::stdin(),
        tokio::io::stdout(),
        server.clone(),
        notifier,
        outgoing,
    )
    .await?;
    server.dispose_all().await;
    runtime.shutdown().await;
    Ok(0)
}

#[derive(Default)]
struct State {
    initialized: bool,
    shutting_down: bool,
    cwd: PathBuf,
    overrides: Overrides,
    max_tokens: Option<u32>,
}

struct SdkSession {
    agent: Arc<Agent>,
    /// Queued and running turns, oldest first; `session/wait` drains them.
    runs: Mutex<Vec<JoinHandle<anyhow::Result<dshx_agent::TurnOutcome>>>>,
    /// Latest live failure not superseded by a later completed turn.
    failure: Mutex<Option<String>>,
}

struct SdkServer {
    runtime: Arc<Runtime>,
    notifier: Notifier,
    state: Mutex<State>,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<SdkSession>>>,
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::internal(message.into())
}

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a non-empty string")))
}

impl SdkServer {
    fn ensure_initialized(&self) -> Result<(), RpcError> {
        let state = self.state.lock().unwrap();
        if !state.initialized {
            return Err(invalid("SDK server is not initialized"));
        }
        if state.shutting_down {
            return Err(invalid("SDK server is shutting down"));
        }
        Ok(())
    }

    fn initialize(&self, params: Value) -> Result<Value, RpcError> {
        let cwd = PathBuf::from(str_param(&params, "cwd")?);
        let cwd = cwd
            .canonicalize()
            .map_err(|e| invalid(format!("{}: {e}", cwd.display())))?;
        let provider = str_param(&params, "provider")?;
        if provider != "deepseek-official" {
            return Err(invalid(format!(
                "no adapter registered for provider \"{provider}\""
            )));
        }
        let model = str_param(&params, "model")?.to_owned();
        let effort = match params.get("reasoningEffort") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => {
                return Err(invalid(
                    "initialize reasoningEffort must be a non-empty string",
                ));
            }
        };
        let max_tokens = match params.get("maxTokens") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|n| *n > 0 && *n <= u32::MAX as u64)
                    .ok_or_else(|| {
                        invalid("initialize maxTokens must be a positive safe integer")
                    })? as u32,
            ),
        };
        let mut state = self.state.lock().unwrap();
        state.cwd = cwd;
        state.overrides = Overrides {
            model: Some(model),
            effort,
            sandbox: None,
        };
        state.max_tokens = max_tokens;
        state.initialized = true;
        Ok(
            json!({ "serverInfo": { "name": "deepseek-harness-sdk-runtime", "version": env!("CARGO_PKG_VERSION") } }),
        )
    }

    fn start(&self, id: &str, log: Arc<dyn SessionLog>) -> Arc<SdkSession> {
        let (cwd, overrides, max_tokens) = {
            let s = self.state.lock().unwrap();
            (
                PathBuf::from(log.header().cwd)
                    .canonicalize()
                    .unwrap_or(s.cwd.clone()),
                s.overrides.clone(),
                s.max_tokens,
            )
        };
        let mut config = self.runtime.agent_config(&cwd, &overrides);
        if max_tokens.is_some() {
            config.max_tokens = max_tokens;
        }
        let observer = Arc::new(StatusNotifier {
            session_id: id.to_owned(),
            notifier: self.notifier.clone(),
        });
        let agent = Arc::new(self.runtime.new_agent(
            config,
            log.clone(),
            Arc::new(NoApprover),
            Some(observer),
        ));
        let mut feed = log.subscribe();
        let notifier = self.notifier.clone();
        let session_id = id.to_owned();
        tokio::spawn(async move {
            loop {
                match feed.recv().await {
                    Ok(event) => notifier.notify(
                        "session.event",
                        json!({ "sessionId": session_id, "event": event }),
                    ),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Arc::new(SdkSession {
            agent,
            runs: Mutex::new(Vec::new()),
            failure: Mutex::new(None),
        })
    }

    /// An unknown SDK id lazily creates the session under exactly that id.
    async fn get_or_create(&self, id: &str) -> Result<Arc<SdkSession>, RpcError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(id) {
            return Ok(s.clone());
        }
        let cwd = self.state.lock().unwrap().cwd.display().to_string();
        let log: Arc<dyn SessionLog> = if self
            .runtime
            .store
            .locate(id)
            .map_err(RpcError::internal)?
            .is_some()
        {
            Arc::new(
                self.runtime
                    .store
                    .resume(id)
                    .await
                    .map_err(RpcError::internal)?,
            )
        } else {
            let mut header = dshx_session::SessionStore::new_header(&cwd);
            header.id = id.to_owned();
            Arc::new(
                self.runtime
                    .store
                    .create_with_header(header)
                    .map_err(RpcError::internal)?,
            )
        };
        let s = self.start(id, log);
        sessions.insert(id.to_owned(), s.clone());
        Ok(s)
    }

    async fn prompt(&self, params: Value) -> Result<Value, RpcError> {
        self.ensure_initialized()?;
        let id = str_param(&params, "sessionId")?.to_owned();
        let blocks = params["contentBlocks"]
            .as_array()
            .ok_or_else(|| invalid("contentBlocks must be an array"))?;
        let mut content = Vec::with_capacity(blocks.len());
        for block in blocks {
            // Inline images are admitted into the attachment store first.
            if block["type"] == "image" && block.get("data").is_some() {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(block["data"].as_str().unwrap_or(""))
                    .map_err(|e| invalid(format!("image data is not base64: {e}")))?;
                let media_type = block["mimeType"].as_str().unwrap_or("").to_owned();
                let attachment = self
                    .runtime
                    .attachments
                    .save_image(NewImage {
                        data,
                        media_type,
                        name: None,
                        width: 0,
                        height: 0,
                    })
                    .await
                    .map_err(RpcError::internal)?;
                content.push(ContentBlock::Image {
                    attachment,
                    offloaded: false,
                });
            } else {
                content.push(
                    serde_json::from_value(block.clone())
                        .map_err(|e| invalid(format!("bad content block: {e}")))?,
                );
            }
        }
        let session = self.get_or_create(&id).await?;
        let mut message = Message::user_text("");
        message.content = content;
        let message_id = message.id.clone();
        let agent = session.agent.clone();
        let handle = tokio::spawn(async move { agent.run_message(message).await });
        session.runs.lock().unwrap().push(handle);
        Ok(json!({ "messageId": message_id }))
    }

    async fn resume(&self, params: Value) -> Result<Value, RpcError> {
        self.ensure_initialized()?;
        let id = str_param(&params, "sessionId")?.to_owned();
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&id) {
            return Err(invalid(format!(
                "SDK session is already open or opening: {id}"
            )));
        }
        if self
            .runtime
            .store
            .locate(&id)
            .map_err(RpcError::internal)?
            .is_none()
        {
            return Err(invalid(format!("unknown session: {id}")));
        }
        let log: Arc<dyn SessionLog> = Arc::new(
            self.runtime
                .store
                .resume(&id)
                .await
                .map_err(RpcError::internal)?,
        );
        let s = self.start(&id, log);
        sessions.insert(id, s);
        Ok(json!({}))
    }

    async fn wait(&self, params: Value) -> Result<Value, RpcError> {
        self.ensure_initialized()?;
        let id = str_param(&params, "sessionId")?;
        let session = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| invalid(format!("unknown SDK session: {id}")))?;
        loop {
            let pending: Vec<_> = std::mem::take(&mut *session.runs.lock().unwrap());
            if pending.is_empty() {
                break;
            }
            for run in pending {
                let failure = match run.await {
                    Ok(Ok(outcome)) => match outcome.reason {
                        TurnEndReason::Completed => None,
                        TurnEndReason::Error { error } => {
                            Some(format!("{} ({})", error.message, error.code))
                        }
                        _ => None,
                    },
                    Ok(Err(e)) => Some(format!("{e:#}")),
                    Err(e) => Some(e.to_string()),
                };
                *session.failure.lock().unwrap() = failure;
            }
        }
        match session.failure.lock().unwrap().take() {
            Some(error) => Err(invalid(error)),
            None => Ok(json!({})),
        }
    }

    async fn working_directory(&self, params: Value, set: bool) -> Result<Value, RpcError> {
        self.ensure_initialized()?;
        let id = str_param(&params, "sessionId")?.to_owned();
        let session = self.get_or_create(&id).await?;
        let current = session.agent.config().cwd;
        if !set {
            return Ok(json!({ "cwd": current }));
        }
        let path = str_param(&params, "path")?;
        let target = current.join(path);
        let dir = target
            .canonicalize()
            .map_err(|e| invalid(format!("{}: {e}", target.display())))?;
        if !dir.is_dir() {
            return Err(invalid(format!("{} is not a directory", dir.display())));
        }
        session.agent.update_config(|c| c.cwd = dir.clone());
        Ok(json!({ "cwd": dir }))
    }

    async fn shutdown(&self) -> Result<Value, RpcError> {
        self.state.lock().unwrap().shutting_down = true;
        self.dispose_all().await;
        Ok(json!({}))
    }

    async fn dispose_all(&self) {
        let sessions: Vec<_> = self.sessions.lock().await.drain().map(|(_, s)| s).collect();
        for s in sessions {
            s.agent.dispose().await;
        }
    }
}

#[async_trait]
impl Handler for SdkServer {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => self.initialize(params),
            "session/prompt" => self.prompt(params).await,
            "session/resume" => self.resume(params).await,
            "session/wait" => self.wait(params).await,
            "session/working-directory/get" => self.working_directory(params, false).await,
            "session/working-directory/set" => self.working_directory(params, true).await,
            "shutdown" => self.shutdown().await,
            _ => Err(RpcError {
                code: jsonrpc::METHOD_NOT_FOUND,
                message: format!("unknown method {method}"),
            }),
        }
    }
}

/// `session.status` notifications (`idle` / `running`, like dsh).
struct StatusNotifier {
    session_id: String,
    notifier: Notifier,
}

impl AgentObserver for StatusNotifier {
    fn on_status(&self, status: &AgentStatus) {
        let status = match status {
            AgentStatus::Idle | AgentStatus::Error { .. } => "idle",
            AgentStatus::Running | AgentStatus::WaitingApproval => "running",
        };
        self.notifier.notify(
            "session.status",
            json!({ "sessionId": self.session_id, "status": status }),
        );
    }
}
