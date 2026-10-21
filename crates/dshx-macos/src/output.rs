//! The shared output step of `registerTool` in `index.js`: summary text,
//! screenshot attachment (only for image-capable models), the canonical
//! `value` and the transcript row's `meta`.
use base64::Engine as _;
use dshx_core::{NewImage, ToolContext, ToolOutput, ToolResultPart};
use serde_json::Value;

use crate::MacError;
use crate::js::{self, Obj};

/// A screenshot as the helper returns it (`{ mime, data(base64), width, height }`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Shot {
    pub mime: Option<Value>,
    pub data: Option<Value>,
    pub width: Option<Value>,
    pub height: Option<Value>,
}

/// What one action produced, before the output step.
#[derive(Debug, Clone, Default)]
pub(crate) struct RunResult {
    pub text: String,
    pub image: Option<Shot>,
    pub cursor: Option<Value>,
    pub place: Option<Value>,
    pub target: Option<Value>,
}

/// `p && typeof p.x === 'number' ? { x: Math.round(p.x), y: Math.round(p.y) } : null`
fn round_point(p: Option<&Value>) -> Option<Value> {
    let p = p.filter(|p| js::truthy(Some(p)))?;
    let x = js::number(p.get("x"))?;
    let y = js::to_number(p.get("y"));
    Some(
        Obj::new()
            .set("x", js::num(js::round(x)))
            .set("y", js::num(js::round(y)))
            .into_value(),
    )
}

fn or_null(v: Option<&Value>) -> Value {
    v.cloned().unwrap_or(Value::Null)
}

/// Turns a run result into the tool output exactly like `registerTool`.
pub(crate) async fn finish(
    tool: &str,
    label: Option<String>,
    args: &Value,
    result: RunResult,
    ctx: &ToolContext,
    text_only_hint: Option<&str>,
) -> Result<ToolOutput, MacError> {
    let mut summary = result.text;
    let mut value = Obj::new().set("summary", "");
    if let Some(cursor) = round_point(result.cursor.as_ref()) {
        value = value.set("cursor", cursor);
    }
    for (key, v) in [("place", &result.place), ("target", &result.target)] {
        if js::truthy(v.as_ref()) {
            value = value.set(key, js::slice(&js::text(v.as_ref()), 120));
        }
    }

    let mut attachment = None;
    if let Some(image) = result.image {
        value = value.set(
            "screen",
            Obj::new()
                .opt("width", image.width.as_ref().map(js::roundtrip))
                .opt("height", image.height.as_ref().map(js::roundtrip))
                .into_value(),
        );
        if !ctx.model_accepts_images {
            if let Some(hint) = text_only_hint {
                summary.push_str(hint);
            }
        } else {
            let data = base64::engine::general_purpose::STANDARD
                .decode(js::text(image.data.as_ref()))
                .map_err(|e| MacError::Attachment(format!("invalid screenshot data: {e}")))?;
            let dim = |v: &Option<Value>| {
                v.as_ref()
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0)
            };
            let stored = ctx
                .attachments
                .save_image(NewImage {
                    data,
                    media_type: js::text(image.mime.as_ref()),
                    name: Some("screen.jpg".into()),
                    width: dim(&image.width),
                    height: dim(&image.height),
                })
                .await
                .map_err(|e| MacError::Attachment(e.to_string()))?;
            value = value.set(
                "image",
                serde_json::to_value(&stored).map_err(|e| MacError::Attachment(e.to_string()))?,
            );
            if js::number(image.width.as_ref()) != Some(f64::from(stored.width)) {
                summary.push_str(&format!(
                    " (The attached image is scaled to {}×{}; keep using coordinates of the {}×{} screenshot.)",
                    stored.width,
                    stored.height,
                    js::text(image.width.as_ref()),
                    js::text(image.height.as_ref()),
                ));
            }
            attachment = Some(stored);
        }
    }
    value = value.set("summary", summary.as_str());

    // Replayed to the UI as the tool result's `meta` (presentationMeta).
    let arg = |key: &str| js::number(args.get(key)).map_or(Value::Null, js::num);
    let meta = Obj::new()
        .set("tool", tool)
        .opt("action", args.get("action").map(js::roundtrip))
        .opt("label", label.map(Value::from))
        .set("x", arg("x"))
        .set("y", arg("y"))
        .set("toX", arg("to_x"))
        .set("toY", arg("to_y"))
        .set("screen", or_null(value.0.get("screen")))
        .set("cursor", or_null(value.0.get("cursor")))
        .set("place", or_null(value.0.get("place")))
        .set("target", or_null(value.0.get("target")))
        .into_value();

    let mut content = vec![ToolResultPart::Text { text: summary }];
    if let Some(attachment) = attachment {
        content.push(ToolResultPart::Image { attachment });
    }
    Ok(ToolOutput {
        content,
        is_error: false,
        value: Some(value.into_value()),
        meta: Some(meta),
        contexts: Vec::new(),
    })
}

/// `new Error(res.error || fallback)` with `err.cancelled = !!res.cancelled`.
pub(crate) fn helper_failure(res: &Value, fallback: &str) -> MacError {
    let message = if js::truthy(res.get("error")) {
        js::text(res.get("error"))
    } else {
        fallback.to_owned()
    };
    MacError::Helper {
        message,
        cancelled: js::truthy(res.get("cancelled")),
    }
}
