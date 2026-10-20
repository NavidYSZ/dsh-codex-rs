//! Durable attachments (screenshots, images read by tools, user uploads).
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// dsh `ImageAttachmentRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachmentRef {
    #[serde(rename = "attachmentId")]
    pub attachment_id: String,
    /// `image/png`, `image/jpeg`, `image/webp` or `image/gif`.
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        rename = "originalDimensions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub original_dimensions: Option<Dimensions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// Image bytes handed to the store. Width/height may be unknown (0); the store
/// then probes them from the bytes.
pub struct NewImage {
    pub data: Vec<u8>,
    pub media_type: String,
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[async_trait]
pub trait AttachmentStore: Send + Sync {
    /// Stores the image (possibly downscaled to the model's budget) and returns its reference.
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef>;
    /// Reads the stored bytes back (for the model request and the UI).
    async fn load(&self, attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)>;
}
