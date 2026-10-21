//! Helpers to build a [`ToolContext`] outside the agent (tests, tools run by
//! hand). The agent builds its contexts the same way, with its own approver
//! and attachment store.
//!
//! ```no_run
//! # async fn demo() -> Result<(), dshx_core::ToolError> {
//! use dshx_core::Tool;
//! let ctx = dshx_tools::testing::context(std::path::Path::new("/tmp"));
//! let out = dshx_tools::bash::BashTool::new()
//!     .execute(serde_json::json!({"command": "ls", "description": "List files"}), ctx)
//!     .await?;
//! # Ok(()) }
//! ```
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dshx_core::{
    ApprovalDecision, Approver, AttachmentStore, ImageAttachmentRef, NewImage, SandboxMode,
    ToolContext,
};
use tokio_util::sync::CancellationToken;

use crate::state::ToolState;

/// Answers every approval with a fixed decision and records the requests.
pub struct FixedApprover {
    pub decision: ApprovalDecision,
    pub asked: Mutex<Vec<(String, String, Option<String>)>>,
}

impl FixedApprover {
    pub fn new(decision: ApprovalDecision) -> Arc<Self> {
        Arc::new(FixedApprover {
            decision,
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Approver for FixedApprover {
    async fn approve(
        &self,
        _call_id: &str,
        tool: &str,
        summary: &str,
        justification: Option<&str>,
    ) -> ApprovalDecision {
        self.asked.lock().unwrap().push((
            tool.to_owned(),
            summary.to_owned(),
            justification.map(str::to_owned),
        ));
        self.decision
    }
}

/// In-memory attachment store; reads PNG/GIF dimensions from the header.
#[derive(Default)]
pub struct MemoryAttachments {
    images: Mutex<HashMap<String, (Vec<u8>, String)>>,
}

fn probe_dimensions(data: &[u8], media_type: &str) -> (u32, u32) {
    match media_type {
        "image/png" if data.len() >= 24 => (
            u32::from_be_bytes([data[16], data[17], data[18], data[19]]),
            u32::from_be_bytes([data[20], data[21], data[22], data[23]]),
        ),
        "image/gif" if data.len() >= 10 => (
            u32::from(u16::from_le_bytes([data[6], data[7]])),
            u32::from(u16::from_le_bytes([data[8], data[9]])),
        ),
        _ => (0, 0),
    }
}

#[async_trait]
impl AttachmentStore for MemoryAttachments {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(&image.data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let id = format!("sha256:{digest}");
        let (width, height) = if image.width > 0 {
            (image.width, image.height)
        } else {
            probe_dimensions(&image.data, &image.media_type)
        };
        let reference = ImageAttachmentRef {
            attachment_id: id.clone(),
            media_type: image.media_type.clone(),
            bytes: image.data.len() as u64,
            width,
            height,
            name: image.name,
            original_dimensions: None,
        };
        self.images
            .lock()
            .unwrap()
            .insert(id, (image.data, image.media_type));
        Ok(reference)
    }

    async fn load(&self, attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        self.images
            .lock()
            .unwrap()
            .get(attachment_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown attachment {attachment_id}"))
    }
}

/// Builder for test contexts.
pub struct ContextBuilder {
    ctx: ToolContext,
}

impl ContextBuilder {
    pub fn sandbox(mut self, mode: SandboxMode) -> Self {
        self.ctx.sandbox = mode;
        self
    }

    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.ctx.approver = approver;
        self
    }

    pub fn state(mut self, state: Arc<ToolState>) -> Self {
        self.ctx.state = state;
        self
    }

    pub fn session(mut self, session_id: &str) -> Self {
        self.ctx.session_id = session_id.to_owned();
        self
    }

    /// Fresh state whose spill files go under `root`.
    pub fn spill_root(mut self, root: &Path) -> Self {
        self.ctx.state = Arc::new(ToolState::with_parts(
            crate::jobs::JobRegistry::default(),
            crate::spill::SpillStore::at(root),
        ));
        self
    }

    pub fn images(mut self, accepts: bool) -> Self {
        self.ctx.model_accepts_images = accepts;
        self
    }

    pub fn attachments(mut self, store: Arc<dyn AttachmentStore>) -> Self {
        self.ctx.attachments = store;
        self
    }

    pub fn build(self) -> ToolContext {
        self.ctx
    }
}

/// A context for `cwd` with full access, a rejecting approver, fresh state
/// and an in-memory attachment store.
pub fn builder(cwd: &Path) -> ContextBuilder {
    ContextBuilder {
        ctx: ToolContext {
            session_id: "test-session".into(),
            call_id: format!("call-{}", uuid::Uuid::new_v4().simple()),
            cwd: cwd.to_path_buf(),
            sandbox: SandboxMode::DangerFullAccess,
            cancel: CancellationToken::new(),
            attachments: Arc::new(MemoryAttachments::default()),
            model_accepts_images: true,
            approver: FixedApprover::new(ApprovalDecision::Rejected),
            state: Arc::new(ToolState::new()),
            caller: None,
        },
    }
}

/// Shorthand for `builder(cwd).build()`.
pub fn context(cwd: &Path) -> ToolContext {
    builder(cwd).build()
}

/// A copy of `ctx` for the next call of the same session (new call id and
/// cancel token, same state).
pub fn next_call(ctx: &ToolContext) -> ToolContext {
    ToolContext {
        call_id: format!("call-{}", uuid::Uuid::new_v4().simple()),
        cancel: CancellationToken::new(),
        ..ctx.clone()
    }
}
