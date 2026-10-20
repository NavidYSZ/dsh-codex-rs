//! Shared helpers: dsh session fixtures → the requests dsh sent.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{
    AttachmentStore, GenerateRequest, ImageAttachmentRef, Message, NewImage, ToolSchema,
};
use serde_json::Value;

pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn read_fixture(name: &str, file: &str) -> Vec<u8> {
    std::fs::read(fixture_dir(name).join(file))
        .unwrap_or_else(|e| panic!("fixture {name}/{file}: {e}"))
}

/// Header line (`type: session`) and events of a dsh v4 session log.
pub fn session(name: &str) -> (Value, Vec<Value>) {
    let text = String::from_utf8(read_fixture(name, "session.jsonl")).unwrap();
    let mut lines = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap());
    let header = lines.next().unwrap();
    (header, lines.collect())
}

/// One request dsh made, reconstructed from its session log.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub request: GenerateRequest,
    /// Seq of the `assistant/message` the request produced (agent steps only).
    pub assistant_seq: Option<u64>,
    /// The persisted assistant message (agent steps only).
    pub assistant: Option<Value>,
    pub usage: Option<Value>,
}

fn surface_message(event: &Value) -> Option<Value> {
    match event["type"].as_str()? {
        "user/message" => Some(event["data"].clone()),
        "system/message" | "assistant/message" | "tool/result" | "developer/message" => {
            Some(event["data"]["message"].clone())
        }
        _ => None,
    }
}

/// Minimal surface derivation (append / replace ranges), enough for the
/// captured sessions: every `assistant/message` closes one agent request,
/// `session/title-llm-request` is the title request.
pub fn recorded_requests(name: &str) -> (Vec<Recorded>, Option<Recorded>) {
    let (header, events) = session(name);
    let session_id = header["id"].as_str().map(str::to_owned);
    let mut surface: Vec<(u64, Value)> = vec![];
    let mut config = Value::Null;
    let mut tools: Vec<ToolSchema> = vec![];
    let mut steps = vec![];
    let mut title = None;
    let messages = |surface: &[(u64, Value)]| -> Vec<Message> {
        surface
            .iter()
            .map(|(_, m)| serde_json::from_value(m.clone()).expect("canonical message"))
            .collect()
    };
    for event in &events {
        let seq = event["seq"].as_u64().unwrap();
        match event["type"].as_str().unwrap() {
            "request/header" => {
                config = event["data"]["header"]["config"].clone();
                if let Some(list) = event["data"]["header"]["tools"].as_array() {
                    tools = list
                        .iter()
                        .map(|t| serde_json::from_value(t.clone()).unwrap())
                        .collect();
                }
            }
            "assistant/message" => {
                steps.push(Recorded {
                    request: GenerateRequest {
                        model: config["model"].as_str().unwrap().into(),
                        reasoning_effort: config["reasoningEffort"].as_str().map(Into::into),
                        messages: messages(&surface),
                        tools: tools.clone(),
                        max_tokens: config["maxTokens"].as_u64().map(|v| v as u32),
                        temperature: None,
                        session_id: session_id.clone(),
                    },
                    assistant_seq: Some(seq),
                    assistant: Some(event["data"]["message"].clone()),
                    usage: Some(event["data"]["usage"].clone()),
                });
            }
            "session/title-llm-request" => {
                let data = &event["data"];
                let mut history = vec![Message::system(data["system"].as_str().unwrap())];
                history.extend(
                    data["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|m| serde_json::from_value(m.clone()).unwrap()),
                );
                title = Some(Recorded {
                    request: GenerateRequest {
                        model: data["route"]["model"].as_str().unwrap().into(),
                        reasoning_effort: Some("off".into()),
                        messages: history,
                        tools: vec![],
                        max_tokens: data["maxTokens"].as_u64().map(|v| v as u32),
                        temperature: None,
                        session_id: session_id.clone(),
                    },
                    assistant_seq: None,
                    assistant: None,
                    usage: None,
                });
            }
            _ => {}
        }
        if let Some(message) = surface_message(event) {
            match &event["surfaceOp"] {
                Value::String(op) if op == "append" => surface.push((seq, message)),
                Value::Object(op) if op["op"] == "replace" => {
                    let (start, end) = (
                        op["startSeq"].as_u64().unwrap(),
                        op["endSeq"].as_u64().unwrap(),
                    );
                    let first = surface
                        .iter()
                        .position(|(s, _)| (start..=end).contains(s))
                        .unwrap();
                    surface.retain(|(s, _)| !(start..=end).contains(s));
                    surface.insert(first, (seq, message));
                }
                other => panic!("unsupported surfaceOp {other}"),
            }
        }
    }
    (steps, title)
}

/// Captured request bodies (`NNN.body.json`) in arrival order: agent steps
/// and the title request separately.
pub fn captured_bodies(name: &str) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut files: Vec<_> = std::fs::read_dir(fixture_dir(name))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().into_string().unwrap()))
        .filter(|f| f.ends_with(".body.json"))
        .collect();
    files.sort();
    let mut steps = vec![];
    let mut title = None;
    for file in files {
        let bytes = read_fixture(name, &file);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if body["system"]
            .as_str()
            .is_some_and(|s| s.contains("concise title"))
        {
            title = Some(bytes);
        } else {
            steps.push(bytes);
        }
    }
    (steps, title.expect("title request"))
}

/// In-memory attachment store keyed by attachment id.
#[derive(Default, Clone)]
pub struct MemoryAttachments(Arc<Mutex<HashMap<String, StoredImage>>>);

type StoredImage = (Vec<u8>, String);

impl MemoryAttachments {
    pub fn insert(&self, id: &str, data: Vec<u8>, media_type: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), (data, media_type.into()));
    }
}

#[async_trait]
impl AttachmentStore for MemoryAttachments {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        anyhow::bail!("not supported: {}", image.media_type)
    }
    async fn load(&self, attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.0
            .lock()
            .unwrap()
            .get(attachment_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no attachment {attachment_id}"))
    }
}

/// A client that knows the fixture image (as dsh's attachment store would).
pub fn fixture_client(config: dshx_llm::DeepSeekConfig) -> dshx_llm::DeepSeekClient {
    let attachments = MemoryAttachments::default();
    attachments.insert(
        "sha256:21f3b2a077613d783e48f3ed20841820cd61ea1031afe87bbb22bcdf115d08d3",
        read_fixture("image", "pixel.png"),
        "image/png",
    );
    dshx_llm::DeepSeekClient::builder(config)
        .api_key(Arc::new(dshx_llm::StaticApiKey("sk-test".into())))
        .attachments(Arc::new(attachments))
        .image_path(Arc::new(dsh_attachment_path))
        .build()
        .unwrap()
}

/// dsh `attachment-local` object path of an attachment id.
pub fn dsh_attachment_path(r: &ImageAttachmentRef) -> Option<String> {
    let hex = r.attachment_id.strip_prefix("sha256:")?;
    Some(format!(
        "/root/.dsh/attachments/v1/objects/{}/{hex}",
        &hex[..2]
    ))
}

/// Our bytes vs dsh's, with a readable diff position on mismatch.
pub fn assert_same_bytes(ours: &[u8], theirs: &[u8], what: &str) {
    if ours == theirs {
        return;
    }
    let at = ours
        .iter()
        .zip(theirs)
        .position(|(a, b)| a != b)
        .unwrap_or(ours.len().min(theirs.len()));
    let window = |b: &[u8]| {
        String::from_utf8_lossy(&b[at.saturating_sub(120)..(at + 120).min(b.len())]).into_owned()
    };
    panic!(
        "{what}: bodies differ at byte {at} (ours {} bytes, dsh {} bytes)\n ours: …{}…\n  dsh: …{}…",
        ours.len(),
        theirs.len(),
        window(ours),
        window(theirs)
    );
}
