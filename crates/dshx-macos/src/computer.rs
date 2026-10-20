//! Model-facing `computer` tool (foreground: takes over the real pointer and
//! keyboard). Port of `lib/computer-tool.js`; schema, texts and helper
//! requests are verbatim.
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::MacError;
use crate::client::{CallOptions, CuaClient};
use crate::helper::Helper;
use crate::js::{self, Obj};
use crate::output::{self, RunResult, Shot};

pub const TOOL_NAME: &str = "computer";

pub const ACTIONS: [&str; 12] = [
    "screenshot",
    "click",
    "double_click",
    "right_click",
    "move",
    "drag",
    "scroll",
    "type",
    "key",
    "open_app",
    "wait",
    "cursor_position",
];

pub const DESCRIPTION: &str = r#"Operate the user's whole Mac screen like a person: look at the screen, move a visible agent cursor, click, type and press keys. This takes over the user's real mouse and keyboard while it runs, so prefer "browser" for websites and "app" for a single native app; both work in the background. Use "computer" when those cannot do the job (several apps at once, drag and drop, context menus) or when the user wants to watch on their own screen.

Workflow:
- Start with action "screenshot". All x/y coordinates are pixels in the most recent screenshot.
- Every pointer, keyboard and app action returns a fresh screenshot of the result, so you rarely need a separate screenshot call.
- Prefer keyboard shortcuts and "open_app" over hunting for icons. Use "key" for shortcuts ("cmd+l", "Return", "cmd+shift+t") and "type" for text.
- Click the centre of controls. After typing into a search or address field, press "Return" with "key".
- Scroll with dy > 0 to move content down (reveal what is below), dy < 0 to go up.
- The user sees your cursor and can press Esc at any time; if a result says the user cancelled, stop immediately and ask how to continue.
- Never enter passwords, payment data or other secrets, and ask before sending, buying, deleting or publishing anything.
- macOS permission prompts ("Allow"/"Erlauben"), login or Touch ID dialogs, System Settings, keychains and password managers are off limits: never click or type there. If one appears or blocks you, stop and ask the user to handle it. The helper refuses such targets anyway."#;

const TEXT_ONLY_HINT: &str =
    " (The current model cannot see images; switch to an image-capable model for computer use.)";

/// `INPUT_SCHEMA`
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ACTIONS, "description": "What to do." },
            "x": { "type": "number", "description": "X in screenshot pixels (click, move, drag start, scroll)." },
            "y": { "type": "number", "description": "Y in screenshot pixels." },
            "to_x": { "type": "number", "description": "Drag end X." },
            "to_y": { "type": "number", "description": "Drag end Y." },
            "text": {
                "type": "string",
                "description": "Text for \"type\"; key combination for \"key\" (e.g. \"cmd+c\", \"Return\", \"Escape\"); app name or bundle id for \"open_app\"."
            },
            "modifiers": {
                "type": "array",
                "items": { "type": "string", "enum": ["cmd", "shift", "alt", "ctrl"] },
                "description": "Modifier keys held during a click."
            },
            "dx": { "type": "number", "description": "Horizontal scroll in pixels." },
            "dy": { "type": "number", "description": "Vertical scroll in pixels (positive = further down the page)." },
            "seconds": { "type": "number", "description": "For \"wait\": seconds to wait (max 10)." }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// `input.text ?? ''` as a string.
fn or_empty(v: Option<&Value>) -> String {
    js::defined(v)
        .map(|v| js::text(Some(v)))
        .unwrap_or_default()
}

/// `String(Math.round(v))`
fn rounded(v: Option<&Value>) -> String {
    js::number_text(js::round(js::to_number(v)))
}

/// Human-readable one-liner for the transcript row; `None` when the input
/// has no action (the JS returns `undefined`).
pub fn describe(input: &Value) -> Option<String> {
    let at = match js::number(input.get("x")) {
        Some(_) => format!(
            " ({}, {})",
            rounded(input.get("x")),
            rounded(input.get("y"))
        ),
        None => String::new(),
    };
    let action = input.get("action")?;
    let Some(action) = action.as_str() else {
        return Some(js::text(Some(action)));
    };
    Some(match action {
        "screenshot" => "Screenshot".into(),
        "click" => format!("Click{at}"),
        "double_click" => format!("Double-click{at}"),
        "right_click" => format!("Right-click{at}"),
        "move" => format!("Move pointer{at}"),
        "drag" => format!(
            "Drag{at} → ({}, {})",
            rounded(input.get("to_x")),
            rounded(input.get("to_y"))
        ),
        "scroll" => {
            let dy = js::defined(input.get("dy")).map_or(0.0, |v| js::to_number(Some(v)));
            format!("Scroll {}{at}", if dy > 0.0 { "down" } else { "up" })
        }
        "type" => {
            let text = or_empty(input.get("text"));
            let more = if js::len16(&text) > 40 { "…" } else { "" };
            format!("Type “{}{more}”", js::slice(&text, 40))
        }
        "key" => format!("Press {}", js::text(input.get("text"))),
        "open_app" => format!("Open {}", js::text(input.get("text"))),
        "wait" => format!(
            "Wait {}s",
            js::defined(input.get("seconds")).map_or("1".into(), |v| js::text(Some(v)))
        ),
        "cursor_position" => "Cursor position".into(),
        other => other.to_owned(),
    })
}

/// Short status shown in the pill next to the on-screen agent cursor.
pub fn activity_label(input: &Value, locale: &str) -> Option<String> {
    let action = input.get("action")?.as_str()?;
    let verb = if locale == "de" {
        match action {
            "click" => "Klickt",
            "double_click" => "Doppelklickt",
            "right_click" => "Rechtsklick",
            "move" => "Bewegt den Zeiger",
            "drag" => "Zieht",
            "scroll" => "Scrollt",
            "type" => "Tippt …",
            "key" => "Drückt",
            "open_app" => "Öffnet",
            "wait" => "Wartet",
            _ => return None,
        }
    } else {
        match action {
            "click" => "Clicking",
            "double_click" => "Double-clicking",
            "right_click" => "Right-clicking",
            "move" => "Moving",
            "drag" => "Dragging",
            "scroll" => "Scrolling",
            "type" => "Typing …",
            "key" => "Pressing",
            "open_app" => "Opening",
            "wait" => "Waiting",
            _ => return None,
        }
    };
    if action == "key" || action == "open_app" {
        return Some(
            format!("{verb} {}", or_empty(input.get("text")))
                .trim()
                .to_owned(),
        );
    }
    Some(verb.to_owned())
}

/// `need(input, ...keys)`
fn need(input: &Value, keys: &[&str]) -> Result<(), MacError> {
    for key in keys {
        if js::number(input.get(*key)).is_none() {
            return Err(MacError::Input(format!(
                "\"{}\" needs numeric \"{key}\"",
                js::text(input.get("action"))
            )));
        }
    }
    Ok(())
}

/// Per-call settings (`maxWidth`, `quality`, `settle`, `locale`).
pub(crate) struct RunSettings<'a> {
    pub max_width: Value,
    pub quality: Value,
    pub settle: f64,
    pub locale: &'a str,
}

/// Runs one action against the helper (`run()` in computer-tool.js).
pub(crate) async fn run(
    client: &CuaClient,
    input: &Value,
    settings: &RunSettings<'_>,
    cancel: &CancellationToken,
) -> Result<RunResult, MacError> {
    let RunSettings {
        max_width,
        quality,
        settle,
        locale,
    } = settings;
    let activity = activity_label(input, locale);
    let after = Obj::new()
        .set("screenshotAfter", true)
        .set("settle", js::num(*settle))
        .set("maxWidth", max_width.clone())
        .set("quality", quality.clone())
        .opt("activity", activity.map(Value::from));
    let arg = |key: &str| input.get(key).map(js::roundtrip);
    let screenshot = || {
        Obj::cmd("screenshot")
            .set("maxWidth", max_width.clone())
            .set("quality", quality.clone())
    };
    let action = input.get("action");
    let request = match action.and_then(Value::as_str) {
        Some("screenshot") => screenshot(),
        Some(a @ ("click" | "double_click" | "right_click")) => {
            need(input, &["x", "y"])?;
            Obj::cmd("click")
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .set("button", if a == "right_click" { "right" } else { "left" })
                .set("count", if a == "double_click" { 2 } else { 1 })
                .set(
                    "modifiers",
                    js::defined(input.get("modifiers")).map_or(json!([]), js::roundtrip),
                )
                .spread(&after)
        }
        Some("move") => {
            need(input, &["x", "y"])?;
            Obj::cmd("move")
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .spread(&after)
        }
        Some("drag") => {
            need(input, &["x", "y", "to_x", "to_y"])?;
            Obj::cmd("drag")
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .opt("toX", arg("to_x"))
                .opt("toY", arg("to_y"))
                .spread(&after)
        }
        Some("scroll") => {
            need(input, &["x", "y"])?;
            let delta = |key: &str| {
                js::num(js::round(
                    js::defined(input.get(key)).map_or(0.0, |v| js::to_number(Some(v))),
                ))
            };
            Obj::cmd("scroll")
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .set("dx", delta("dx"))
                .set("dy", delta("dy"))
                .spread(&after)
        }
        Some("type") => {
            let Some(text) = input.get("text").filter(|t| t.is_string()) else {
                return Err(MacError::Input("\"type\" needs \"text\"".into()));
            };
            Obj::cmd("type").set("text", text.clone()).spread(&after)
        }
        Some("key") => {
            let Some(text) = input.get("text").filter(|t| t.is_string()) else {
                return Err(MacError::Input(
                    "\"key\" needs \"text\", e.g. \"cmd+c\"".into(),
                ));
            };
            Obj::cmd("key").set("keys", text.clone()).spread(&after)
        }
        Some("open_app") => {
            let Some(text) = input.get("text").filter(|t| t.is_string()) else {
                return Err(MacError::Input(
                    "\"open_app\" needs \"text\" with the app name".into(),
                ));
            };
            Obj::cmd("open_app")
                .set("name", text.clone())
                .spread(&after)
                .set("settle", js::num(1.0))
        }
        Some("wait") => {
            let seconds = js::defined(input.get("seconds")).map_or(1.0, |v| js::to_number(Some(v)));
            // NaN (a non-numeric value) waits 0 ms, like setTimeout(NaN).
            let ms = (seconds.clamp(0.0, 10.0) * 1000.0).max(0.0);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms as u64)) => {}
                _ = cancel.cancelled() => {}
            }
            screenshot()
        }
        Some("cursor_position") => Obj::cmd("cursor_position"),
        _ => {
            return Err(MacError::Input(format!(
                "unknown action \"{}\"",
                js::text(action)
            )));
        }
    };

    if cancel.is_cancelled() {
        return Err(MacError::Aborted);
    }
    let res = tokio::select! {
        res = client.call(&request.0, CallOptions::default()) => res?,
        _ = cancel.cancelled() => return Err(MacError::Aborted),
    };
    if !js::truthy(res.get("ok")) {
        return Err(output::helper_failure(&res, "computer-use helper failed"));
    }

    let is_screenshot = request.0.get("cmd").and_then(Value::as_str) == Some("screenshot");
    let shot = if is_screenshot {
        Some(&res)
    } else {
        js::defined(res.get("screenshot"))
    };
    let cursor =
        js::defined(res.get("cursor")).or_else(|| shot.and_then(|s| js::defined(s.get("cursor"))));
    let mut lines = vec![format!(
        "{} — done.",
        describe(input).unwrap_or_else(|| "undefined".into())
    )];
    if let Some(shot) = shot.filter(|s| js::truthy(Some(s))) {
        let front = if js::truthy(shot.get("frontmostApp")) {
            format!("; frontmost app: {}", js::text(shot.get("frontmostApp")))
        } else {
            String::new()
        };
        lines.push(format!(
            "Screenshot {}×{}px of the display{front}.",
            js::text(shot.get("width")),
            js::text(shot.get("height"))
        ));
    }
    if let Some(cursor) = cursor.filter(|c| js::truthy(Some(c))) {
        lines.push(format!(
            "Agent cursor at ({}, {}).",
            js::text(cursor.get("x")),
            js::text(cursor.get("y"))
        ));
    }
    let image = shot.filter(|s| js::truthy(s.get("data"))).map(|s| Shot {
        mime: s.get("mime").cloned(),
        data: s.get("data").cloned(),
        width: s.get("width").cloned(),
        height: s.get("height").cloned(),
    });
    Ok(RunResult {
        text: lines.join(" "),
        image,
        cursor: cursor.cloned(),
        ..Default::default()
    })
}

/// The `computer` tool. Configures the helper before its first action.
pub struct ComputerTool {
    helper: Arc<Helper>,
}

impl ComputerTool {
    pub fn new(helper: Arc<Helper>) -> Self {
        ComputerTool { helper }
    }
}

#[async_trait]
impl Tool for ComputerTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TOOL_NAME.into(),
            description: DESCRIPTION.into(),
            parameters: input_schema(),
        }
    }

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }

    fn describe(&self, args: &Value) -> String {
        describe(args).unwrap_or_else(|| TOOL_NAME.into())
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        self.helper
            .configure_computer()
            .await
            .map_err(MacError::from)?;
        let s = self.helper.settings();
        let settings = RunSettings {
            max_width: s.max_width(),
            quality: s.quality(),
            settle: 0.5,
            locale: s.locale(),
        };
        let result = run(self.helper.client(), &args, &settings, &ctx.cancel).await?;
        Ok(output::finish(
            TOOL_NAME,
            describe(&args),
            &args,
            result,
            &ctx,
            Some(TEXT_ONLY_HINT),
        )
        .await?)
    }
}
