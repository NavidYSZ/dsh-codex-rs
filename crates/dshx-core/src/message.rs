//! Canonical conversation messages (dsh `Message` / `ContentBlock`).
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::attachment::ImageAttachmentRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

/// One content block. Serialized with dsh's `type` tags
/// (`text`, `reasoning`, `image`, `tool-call`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    /// Model reasoning ("thinking"). The provider signature needed to replay it
    /// lives in the assistant message's `source.replayState`, not here.
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
    #[serde(rename = "image")]
    Image {
        attachment: ImageAttachmentRef,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        offloaded: bool,
    },
    /// `arguments` is the raw JSON string exactly as the model produced it.
    #[serde(rename = "tool-call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "tool-addition")]
    ToolAddition {
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    #[serde(rename = "tool-removal")]
    ToolRemoval {
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    /// Blocks this build does not know (forward compatibility with newer logs).
    #[serde(untagged)]
    Unknown(Value),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }
}

/// Where a message came from (dsh `MessageSource`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MessageSource {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "model")]
    Model {
        provider: String,
        model: String,
        /// Adapter-private replay data (dsh `ReplayEnvelope`), opaque to everyone
        /// but the adapter that produced it.
        #[serde(
            rename = "replayState",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        replay_state: Option<Value>,
    },
    #[serde(rename = "tool")]
    Tool {
        #[serde(rename = "callId")]
        call_id: String,
    },
    #[serde(rename = "system-prompt")]
    SystemPrompt,
    #[serde(untagged)]
    Other(Value),
}

impl MessageSource {
    /// A message the human wrote: plain `user`, or dsh's web form with extra
    /// fields (`{kind: "user", rpcId, clientTimeZone}`).
    pub fn is_user(&self) -> bool {
        match self {
            MessageSource::User => true,
            MessageSource::Other(v) => v["kind"] == "user",
            _ => false,
        }
    }
}

/// A durable conversation message. Extra dsh fields (e.g. `form`, `sections`
/// on context messages) are preserved in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub source: MessageSource,
    /// Only on `role: tool`.
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "isError", default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            id: crate::new_id(),
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            source: MessageSource::User,
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Message {
            id: crate::new_id(),
            role: Role::System,
            content: vec![ContentBlock::text(text)],
            source: MessageSource::SystemPrompt,
            tool_call_id: None,
            is_error: None,
            extra: Default::default(),
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        content: Vec<ContentBlock>,
        is_error: bool,
    ) -> Self {
        let call_id = call_id.into();
        Message {
            id: crate::new_id(),
            role: Role::Tool,
            content,
            source: MessageSource::Tool {
                call_id: call_id.clone(),
            },
            tool_call_id: Some(call_id),
            is_error: Some(is_error),
            extra: Default::default(),
        }
    }

    /// Concatenated visible text of the message.
    pub fn plain_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.as_str(), name.as_str(), arguments.as_str())),
            _ => None,
        })
    }
}
