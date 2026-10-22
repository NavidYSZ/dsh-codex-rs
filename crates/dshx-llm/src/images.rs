//! Model-facing image texts and request budgets (dsh `llm/src/content.ts`).
use dshx_core::{ImageAttachmentRef, LlmFailure};

/// Resolves the read-only path of a normalized attachment in the tool
/// execution world; when known it is shown to the model beside the image.
pub type ImagePathResolver = dyn Fn(&ImageAttachmentRef) -> Option<String> + Send + Sync;

/// One image as sent in this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestImage {
    pub data: Vec<u8>,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn identity(r: &ImageAttachmentRef) -> String {
    match &r.name {
        None => r.attachment_id.clone(),
        Some(name) => format!("{} ({})", quoted(name), r.attachment_id),
    }
}

fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => ".jpg",
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        _ => ".png",
    }
}

fn normalized_access_text(r: &ImageAttachmentRef, path: &str) -> String {
    format!(
        " Normalized copy (read-only; may be resized or re-encoded): {} ({}x{}px, {}). Source dimensions, format, and byte size may differ. Copy to a writable path ending in {} before editing.",
        quoted(path),
        r.width,
        r.height,
        r.media_type,
        extension(&r.media_type)
    )
}

/// Text block sent before each request image (dsh `requestImageHandleText`).
pub fn request_image_handle_text(
    r: &ImageAttachmentRef,
    width: u32,
    height: u32,
    path: Option<&str>,
) -> String {
    let preview = format!("Image {}; request preview {width}x{height}px.", identity(r));
    match path {
        None => format!(
            "{preview} It may be resized or re-encoded; source dimensions, format, and byte size may differ."
        ),
        Some(path) => preview + &normalized_access_text(r, path),
    }
}

/// Placeholder for an image a text-only model cannot see (dsh `textOnlyImageText`).
pub fn text_only_image_text(r: &ImageAttachmentRef) -> String {
    let digest: String = r
        .attachment_id
        .chars()
        .skip("sha256:".len())
        .take(8)
        .collect();
    format!("[image omitted because this model accepts text only; attachment sha256:{digest}]")
}

/// Placeholder for an image offloaded to fit request limits (dsh `offloadedImageText`).
pub fn offloaded_image_text(r: &ImageAttachmentRef, path: Option<&str>) -> String {
    let identity = format!(
        "image omitted to fit request image limits; {}.",
        identity(r)
    );
    match path {
        None => format!(
            "[{identity} No local normalized image path is available; ask the user to attach it again if needed.]"
        ),
        Some(path) => format!("[{identity}{}]", normalized_access_text(r, path)),
    }
}

/// Base64 length of `bytes` raw bytes, padding included.
pub fn base64_len(bytes: u64) -> u64 {
    bytes.div_ceil(3) * 4
}

/// How many of the oldest retained image occurrences must be offloaded so
/// the request fits (dsh `offloadedImagePrefixCount`); `lengths` are the
/// represented sizes, oldest first.
pub fn offloaded_image_prefix_count(
    lengths: &[u64],
    max_images: u64,
    max_bytes: u64,
    count_quantum: u64,
    byte_quantum: u64,
) -> u64 {
    let total: u64 = lengths.iter().sum();
    let excess_count = (lengths.len() as u64).saturating_sub(max_images);
    let excess_bytes = total.saturating_sub(max_bytes);
    if excess_count == 0 && excess_bytes == 0 {
        return 0;
    }
    let count_quantum = count_quantum.max(1);
    let byte_quantum = byte_quantum.max(1);
    let remove_count = if excess_count == 0 {
        0
    } else {
        excess_count.div_ceil(count_quantum) * count_quantum
    };
    let remove_bytes = if excess_bytes == 0 {
        0
    } else {
        excess_bytes.div_ceil(byte_quantum) * byte_quantum
    };
    let mut count = 0;
    let mut removed = 0;
    for &bytes in lengths {
        let byte_target_met = remove_bytes == 0
            || if byte_quantum == 1 {
                removed >= remove_bytes
            } else {
                removed > remove_bytes
            };
        if count >= remove_count && byte_target_met {
            break;
        }
        removed += bytes;
        count += 1;
    }
    count
}

/// `IMAGE_OFFLOAD_REQUIRED`: the agent must offload `count` more of the
/// oldest images (log `image/offload`) and rebuild the request.
pub fn image_offload_required(count: u64) -> LlmFailure {
    LlmFailure {
        offload_images: Some(count),
        ..LlmFailure::new(
            "IMAGE_OFFLOAD_REQUIRED",
            format!(
                "DeepSeek Messages base64 request images exceed the route budget; {count} more oldest occurrence(s) must be offloaded."
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: Option<&str>) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: format!("sha256:{}", "a".repeat(64)),
            media_type: "image/png".into(),
            bytes: 3,
            width: 4,
            height: 2,
            name: name.map(Into::into),
            original_dimensions: None,
        }
    }

    #[test]
    fn texts_match_dsh() {
        let id = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            request_image_handle_text(&image(None), 4, 2, None),
            format!(
                "Image {id}; request preview 4x2px. It may be resized or re-encoded; source dimensions, format, and byte size may differ."
            )
        );
        assert_eq!(
            request_image_handle_text(&image(Some("shot.png")), 4, 2, Some("/w/a.png")),
            format!(
                "Image \"shot.png\" ({id}); request preview 4x2px. Normalized copy (read-only; may be resized or re-encoded): \"/w/a.png\" (4x2px, image/png). Source dimensions, format, and byte size may differ. Copy to a writable path ending in .png before editing."
            )
        );
        assert_eq!(
            text_only_image_text(&image(None)),
            "[image omitted because this model accepts text only; attachment sha256:aaaaaaaa]"
        );
        assert_eq!(
            offloaded_image_text(&image(None), None),
            format!(
                "[image omitted to fit request image limits; {id}. No local normalized image path is available; ask the user to attach it again if needed.]"
            )
        );
    }

    #[test]
    fn offload_count_uses_quanta() {
        assert_eq!(offloaded_image_prefix_count(&[4, 4], 2, 4, 1, 1), 1);
        assert_eq!(offloaded_image_prefix_count(&[4, 4], 1, 100, 1, 1), 1);
        assert_eq!(offloaded_image_prefix_count(&[1; 30], 25, 100, 20, 1), 20);
        assert_eq!(offloaded_image_prefix_count(&[4], 2, 4, 1, 1), 0);
        assert_eq!(offloaded_image_prefix_count(&[40], 2, 4, 1, 1), 1);
        assert_eq!(base64_len(3), 4);
        assert_eq!(base64_len(4), 8);
    }
}
