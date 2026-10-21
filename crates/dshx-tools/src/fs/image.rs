//! The `read_image` tool (dsh `tool-fs/read-image.ts`): stores the image in the
//! session's attachment store and returns it as an image part.
use async_trait::async_trait;
use dshx_core::{
    ImageAttachmentRef, NewImage, Tool, ToolContext, ToolError, ToolOutput, ToolResultPart,
    ToolSchema,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::read::regular_target;
use super::resolve;
use crate::args::{self, fail};
use crate::schemas::upstream_schema;
use crate::state::{Observation, ToolState};

/// Largest image read (dsh attachment-local `maxImageBytes`).
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

fn media_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// dsh `sniffImageMediaType`.
pub fn sniff_media_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    None
}

/// Extension of the last path segment, lowercase, without the dot (Node `extname`).
fn extension(path: &str) -> Option<String> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some(base[dot + 1..].to_ascii_lowercase())
}

/// dsh `formatImageReadOutput`.
pub fn format_image_output(path: &str, image: &ImageAttachmentRef) -> String {
    let mut scaled = String::new();
    if let Some(original) = image.original_dimensions {
        let x = format!(
            "{:.2}",
            f64::from(original.width) / f64::from(image.width.max(1))
        );
        let y = format!(
            "{:.2}",
            f64::from(original.height) / f64::from(image.height.max(1))
        );
        let advice = if x == y {
            format!("multiply coordinates by {x}")
        } else {
            format!("multiply x coordinates by {x} and y coordinates by {y}")
        };
        scaled = format!(
            " (downscaled from {}x{} px; {advice} to locate features in the original file)",
            original.width, original.height
        );
    }
    format!(
        "<path>{path}</path>\n<type>image</type>\n<content>\n{} image, {}x{} px, {} bytes{scaled}\n</content>",
        image.media_type, image.width, image.height, image.bytes
    )
}

#[derive(Deserialize)]
struct ReadImageArgs {
    file_path: String,
}

/// The `read_image` tool.
pub struct ReadImageTool;

#[async_trait]
impl Tool for ReadImageTool {
    fn schema(&self) -> ToolSchema {
        upstream_schema("read_image")
    }

    fn concurrency_safe(&self) -> bool {
        true
    }

    fn describe(&self, args: &Value) -> String {
        format!("Read image {}", args["file_path"].as_str().unwrap_or(""))
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadImageArgs = args::parse("read_image", args)?;
        let requested = args.file_path;
        if requested.trim().is_empty() {
            return Err(fail("file_path must be a non-empty string"));
        }
        let ext = extension(&requested);
        let declared = ext.as_deref().and_then(media_type_for_extension);
        if let (None, Some(ext)) = (declared, &ext) {
            return Err(fail(format!(
                "cannot read \"{requested}\": the .{ext} extension does not declare a supported image format; read_image accepts PNG/JPEG/WebP/GIF files, including extension-less files in those formats"
            )));
        }
        let state = ToolState::get(&ctx)?;
        if !ctx.model_accepts_images {
            let model = match state.model() {
                Some(model) => format!("model \"{model}\""),
                None => "the current model".to_owned(),
            };
            return Err(fail(format!(
                "cannot read \"{requested}\" as an image: {model} does not declare image input; switch to an image-capable model to read images"
            )));
        }
        let target = resolve(&ctx.cwd, &requested)?;
        let info = regular_target(state, &target)?;
        let display = target.display.clone();
        if info.size > MAX_IMAGE_BYTES {
            return Err(fail(format!(
                "cannot read \"{display}\": {} bytes exceeds the {MAX_IMAGE_BYTES}-byte limit",
                info.size
            )));
        }
        let data = tokio::fs::read(&target.key)
            .await
            .map_err(|e| fail(format!("cannot read \"{display}\": {e}")))?;
        if data.len() as u64 > MAX_IMAGE_BYTES {
            return Err(fail(format!(
                "cannot read \"{display}\": content exceeds the {MAX_IMAGE_BYTES}-byte limit"
            )));
        }
        let Some(media_type) = declared.or_else(|| sniff_media_type(&data)) else {
            return Err(fail(format!(
                "cannot read \"{display}\": the file content is not a supported image format; read_image accepts PNG/JPEG/WebP/GIF"
            )));
        };
        let sniffed = sniff_media_type(&data);
        if let (Some(declared), Some(actual)) = (declared, sniffed)
            && declared != actual
        {
            return Err(fail(format!(
                "cannot read \"{display}\": the .{} extension declares {declared}, but the bytes use a different image format; rename the file to match its actual format if it is PNG/JPEG/WebP/GIF, or convert it to one of those formats",
                ext.unwrap_or_default()
            )));
        }
        let name = display.rsplit('/').next().map(str::to_owned);
        let image = NewImage {
            data,
            media_type: media_type.to_owned(),
            name,
            width: 0,
            height: 0,
        };
        let attachment = ctx
            .attachments
            .save_image(image)
            .await
            .map_err(|e| fail(format!("cannot read \"{display}\": {e}")))?;
        state.observe(&target.key, Observation::Present(info.version));
        let path = target.process_path();
        let text = format_image_output(&path, &attachment);
        let value = json!({"path": path, "image": attachment});
        Ok(ToolOutput {
            content: vec![
                ToolResultPart::Text { text },
                ToolResultPart::Image { attachment },
            ],
            is_error: false,
            value: Some(value),
            meta: Some(json!({"path": path})),
            contexts: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshx_core::Dimensions;

    #[test]
    fn sniffing_and_text() {
        assert_eq!(sniff_media_type(b"GIF89a...."), Some("image/gif"));
        assert_eq!(
            sniff_media_type(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(extension("a/b.PNG").as_deref(), Some("png"));
        assert_eq!(extension("a/.hidden"), None);
        let image = ImageAttachmentRef {
            attachment_id: "sha256:x".into(),
            media_type: "image/png".into(),
            bytes: 69,
            width: 1,
            height: 1,
            name: Some("red.png".into()),
            original_dimensions: None,
        };
        assert_eq!(
            format_image_output("/w/red.png", &image),
            "<path>/w/red.png</path>\n<type>image</type>\n<content>\nimage/png image, 1x1 px, 69 bytes\n</content>"
        );
        let scaled = ImageAttachmentRef {
            width: 1000,
            height: 500,
            original_dimensions: Some(Dimensions {
                width: 2000,
                height: 1000,
            }),
            ..image
        };
        assert!(format_image_output("/p", &scaled).contains(
            "(downscaled from 2000x1000 px; multiply coordinates by 2.00 to locate features in the original file)"
        ));
    }
}
