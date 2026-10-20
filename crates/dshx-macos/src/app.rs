//! Model-facing `app` tool: background control of one Mac app window through
//! the helper's Accessibility layer. Port of `lib/app-tool.js`; schema, texts
//! and helper requests are verbatim.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dshx_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::MacError;
use crate::client::CallOptions;
use crate::helper::Helper;
use crate::js::{self, Obj};
use crate::output::{self, RunResult, Shot};

pub const TOOL_NAME: &str = "app";

pub const ACTIONS: [&str; 10] = [
    "state",
    "click",
    "type",
    "set_value",
    "key",
    "scroll",
    "menu",
    "action",
    "list_apps",
    "show",
];

pub const DESCRIPTION: &str = r#"Operate a Mac app in the BACKGROUND like a person, without taking over the screen. The app stays behind the user's windows, their pointer and keyboard are untouched, and a small floating mini window shows the app live with your cursor and a Stop button. Prefer this tool over "computer" for native apps. Use "browser" for websites.

Workflow:
- Start with action "state" and "app" (name or bundle id, e.g. "Notes", "Rechner", "com.apple.Notes"). It launches the app in the background if needed and returns a screenshot of its window plus an outline: [index] role "title" value="…" @(x,y).
- Act with element indices from the latest outline: "click", "type" (with index it focuses that field first, then inserts "text" at the caret), "set_value" (replace a field's whole value), "scroll", "action" (an action listed for that element, e.g. AXIncrement, AXOpen). Every action returns a fresh state; indices change, so always use the newest outline.
- Menu bar: "menu" with "path" in the app's language, e.g. ["Ablage", "Neue Notiz"]. A one-entry path lists that menu's items. Prefer menus over ⌘ shortcuts, which background apps often ignore.
- "key" sends keys to the app ("Return", "Escape", "Tab", "Down"). Return and Escape use Accessibility when the focused element supports it.
- x/y are screenshot pixels of the app window; use them only when the element is not in the outline. "window" picks another window by id or title. Right-click menus are not available in the background.
- "show" brings the app to the front, only when the user wants to see it. "list_apps" lists running apps.
- Never type passwords or payment data; password fields are refused. System Settings, permission and login dialogs, keychains and password managers are off limits, and the helper refuses them. Ask the user before sending, deleting, buying or publishing anything.
- If a result says the user stopped the agent, stop immediately and ask how to continue."#;

/// `INPUT_SCHEMA`
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ACTIONS, "description": "What to do." },
            "app": { "type": "string", "description": "For \"state\": app name or bundle id. Later actions use the app of the latest state." },
            "window": { "type": "string", "description": "For \"state\": window id or part of its title (default: main window)." },
            "index": { "type": "integer", "description": "Element [index] from the latest outline." },
            "x": { "type": "number", "description": "X in screenshot pixels when no index fits." },
            "y": { "type": "number", "description": "Y in screenshot pixels when no index fits." },
            "text": { "type": "string", "description": "For \"type\": text to insert." },
            "value": { "type": "string", "description": "For \"set_value\": the new value." },
            "keys": { "type": "string", "description": "For \"key\": e.g. \"Return\", \"Escape\", \"Tab\", \"Down\", \"cmd+s\"." },
            "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "description": "For \"scroll\" (default down)." },
            "amount": { "type": "number", "description": "For \"scroll\": steps (default 1)." },
            "path": { "type": "array", "items": { "type": "string" }, "description": "For \"menu\": titles from the menu bar down." },
            "name": { "type": "string", "description": "For \"action\": the AX action name shown in the outline." },
            "count": { "type": "integer", "description": "For \"click\": 2 for a double-click." }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// `clip(s, n)`
fn clip(s: &str, n: usize) -> String {
    if js::len16(s) > n {
        format!("{}…", js::slice(s, n))
    } else {
        s.to_owned()
    }
}

/// `input.x ?? ''` as a string.
fn or_empty(v: Option<&Value>) -> String {
    js::defined(v)
        .map(|v| js::text(Some(v)))
        .unwrap_or_default()
}

/// `input.x ?? fallback` as a string.
fn or(v: Option<&Value>, fallback: &str) -> String {
    js::defined(v).map_or_else(|| fallback.to_owned(), |v| js::text(Some(v)))
}

/// `(input.path ?? []).join(' › ')`
fn path_text(path: Option<&Value>) -> String {
    match js::defined(path) {
        Some(Value::Array(items)) => js::join(items, " › "),
        Some(other) => js::text(Some(other)),
        None => String::new(),
    }
}

/// Human-readable one-liner for the transcript row; `None` without an action.
pub fn describe(input: &Value) -> Option<String> {
    let at = if let Some(index) = js::number(input.get("index")) {
        format!(" [{}]", js::number_text(index))
    } else if let Some(x) = js::number(input.get("x")) {
        let y = js::to_number(input.get("y"));
        format!(
            " ({}, {})",
            js::number_text(js::round(x)),
            js::number_text(js::round(y))
        )
    } else {
        String::new()
    };
    let action = input.get("action")?;
    Some(match action.as_str() {
        Some("state") => format!("Look at {}", or(input.get("app"), "app")),
        Some("type") => format!("Type “{}”{at}", clip(&or_empty(input.get("text")), 40)),
        Some("set_value") => format!("Set value{at}"),
        Some("key") => format!("Press {}", or_empty(input.get("keys"))),
        Some("menu") => format!("Menu {}", path_text(input.get("path"))),
        Some("action") => format!("{}{at}", or(input.get("name"), "Action")),
        Some("scroll") => format!("Scroll {}{at}", or(input.get("direction"), "down")),
        Some("list_apps") => "List apps".into(),
        Some("show") => "Show app".into(),
        _ => {
            // `${input.action[0].toUpperCase()}${input.action.slice(1)}${at}`
            let name = js::text(Some(action));
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => format!("{}{}{at}", first.to_uppercase(), chars.as_str()),
                None => at,
            }
        }
    })
}

/// Status for the mini window's activity line.
pub fn activity(input: &Value, locale: &str) -> String {
    let action = input.get("action");
    let name = action.and_then(Value::as_str).unwrap_or_default();
    let verb = if locale == "de" {
        match name {
            "state" => "Sieht sich das Fenster an",
            "click" => "Klickt",
            "type" => "Tippt",
            "set_value" => "Trägt ein",
            "key" => "Drückt",
            "scroll" => "Scrollt",
            "menu" => "Menü",
            "action" => "Bedient",
            "show" => "Holt die App nach vorn",
            _ => "",
        }
    } else {
        match name {
            "state" => "Looking at the window",
            "click" => "Clicking",
            "type" => "Typing",
            "set_value" => "Filling in",
            "key" => "Pressing",
            "scroll" => "Scrolling",
            "menu" => "Menu",
            "action" => "Operating",
            "show" => "Bringing the app forward",
            _ => "",
        }
    };
    let verb = if verb.is_empty() {
        js::text(action)
    } else {
        verb.to_owned()
    };
    match name {
        "type" => {
            let text = clip(&or_empty(input.get("text")), 36);
            if locale == "de" {
                format!("{verb} „{text}“")
            } else {
                format!("{verb} “{text}”")
            }
        }
        "key" => format!("{verb} {}", or_empty(input.get("keys"))),
        "menu" => format!("{verb} {}", path_text(input.get("path"))),
        _ => verb,
    }
}

/// JS `\s`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `/^\[(\d+)\] \S+ "([^"]*)"/` at the start of `s`: (match length, index, title).
fn outline_entry(s: &str) -> Option<(usize, &str, &str)> {
    let rest = s.strip_prefix('[')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let (index, rest) = rest.split_at(digits);
    let rest = rest.strip_prefix("] ")?;
    let role = rest.find(js_space).unwrap_or(rest.len());
    if role == 0 {
        return None;
    }
    let rest = rest[role..].strip_prefix(" \"")?;
    let close = rest.find('"')?;
    let title = &rest[..close];
    let len = s.len() - rest.len() + close + 1;
    Some((len, index, title))
}

/// Element titles of an outline (`remember()`), scanning like `matchAll` with the `gm` flags.
fn outline_names(tree: &str) -> Vec<(u64, String)> {
    let mut starts = vec![0];
    starts.extend(
        tree.char_indices()
            .filter(|&(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(i, c)| i + c.len_utf8()),
    );
    let mut names = Vec::new();
    let mut pos = 0;
    for start in starts {
        if start < pos {
            continue;
        }
        if let Some((len, index, title)) = outline_entry(&tree[start..]) {
            if let Ok(index) = index.parse::<u64>() {
                names.push((index, title.to_owned()));
            }
            pos = start + len;
        }
    }
    names
}

fn state_text(s: &Value, header: &str) -> String {
    let mut lines = Vec::new();
    if !header.is_empty() {
        lines.push(header.to_owned());
    }
    let place = if js::truthy(s.get("frontmost")) {
        ", in front"
    } else {
        ", in the background"
    };
    lines.push(format!(
        "App: {} ({}){place}",
        js::text(s.get("app")),
        js::text(s.get("bundleId"))
    ));
    let window = s.get("window");
    let title = window.and_then(|w| w.get("title"));
    let title = if js::truthy(title) {
        js::text(title)
    } else {
        "(untitled)".into()
    };
    lines.push(format!(
        "Window {}: \"{title}\" {}×{} px screenshot",
        js::text(window.and_then(|w| w.get("id"))),
        js::text(s.get("width")),
        js::text(s.get("height"))
    ));
    if let Some(others) = s
        .get("otherWindows")
        .and_then(Value::as_array)
        .filter(|o| !o.is_empty())
    {
        lines.push(format!("Other windows: {}", js::join(others, " | ")));
    }
    lines.push("Outline, [index] role \"title\" @(x,y) in screenshot pixels:".into());
    let tree = s.get("tree");
    lines.push(if js::truthy(tree) {
        js::text(tree)
    } else {
        "(empty)".into()
    });
    lines.join("\n")
}

/// The `app` tool. Remembers the element titles of the latest outline so
/// transcript rows can name the element an action targets.
pub struct AppTool {
    helper: Arc<Helper>,
    names: Mutex<HashMap<u64, String>>,
}

impl AppTool {
    pub fn new(helper: Arc<Helper>) -> Self {
        AppTool {
            helper,
            names: Mutex::new(HashMap::new()),
        }
    }

    fn remember(&self, tree: &str) {
        let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        names.clear();
        names.extend(outline_names(tree));
    }

    /// `lastNames.get(input.index) ?? null`
    fn target(&self, index: Option<&Value>) -> Option<String> {
        let index = js::number(index)?;
        if index < 0.0 || index.fract() != 0.0 || index > 9_007_199_254_740_992.0 {
            return None;
        }
        self.names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(index as u64))
            .cloned()
    }

    /// Runs one action (`run()` in app-tool.js).
    async fn run(&self, input: &Value, cancel: &CancellationToken) -> Result<RunResult, MacError> {
        let settings = self.helper.settings();
        let client = self.helper.client();
        let common = Obj::new()
            .set("activity", activity(input, settings.locale()))
            .set("maxWidth", settings.max_width())
            .set("quality", settings.quality());
        let arg = |key: &str| input.get(key).map(js::roundtrip);
        let action = input.get("action");
        let name = action.and_then(Value::as_str);
        let mut request = match name {
            Some("list_apps") => {
                let res = call(
                    client,
                    &Obj::cmd("app_list"),
                    CallOptions::default(),
                    cancel,
                )
                .await?;
                if !js::truthy(res.get("ok")) {
                    return Err(output::helper_failure(&res, "app control failed"));
                }
                let apps = res
                    .get("apps")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let lines: Vec<String> = apps
                    .iter()
                    .map(|a| {
                        format!(
                            "{} ({}){}{}",
                            js::text(a.get("name")),
                            js::text(a.get("bundleId")),
                            if js::truthy(a.get("active")) {
                                ", in front"
                            } else {
                                ""
                            },
                            if js::truthy(a.get("hidden")) {
                                ", hidden"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect();
                return Ok(RunResult {
                    text: format!("Running apps:\n{}", lines.join("\n")),
                    ..Default::default()
                });
            }
            Some("state") => {
                if !js::truthy(input.get("app")) {
                    return Err(MacError::Input("\"state\" needs \"app\"".into()));
                }
                Obj::cmd("app_state")
                    .opt("app", arg("app"))
                    .opt("window", arg("window").filter(|w| js::truthy(Some(w))))
                    .spread(&common)
            }
            Some("click") => Obj::cmd("app_click")
                .opt("index", arg("index"))
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .set(
                    "count",
                    js::defined(input.get("count")).map_or(json!(1), js::roundtrip),
                )
                .spread(&common),
            Some("type") => {
                let Some(text) = input.get("text").filter(|t| t.is_string()) else {
                    return Err(MacError::Input("\"type\" needs \"text\"".into()));
                };
                Obj::cmd("app_type")
                    .opt("index", arg("index"))
                    .set("text", text.clone())
                    .spread(&common)
            }
            Some("set_value") => Obj::cmd("app_set_value")
                .opt("index", arg("index"))
                .set(
                    "value",
                    js::defined(input.get("value")).map_or(json!(""), js::roundtrip),
                )
                .spread(&common),
            Some("key") => {
                if !js::truthy(input.get("keys")) {
                    return Err(MacError::Input("\"key\" needs \"keys\"".into()));
                }
                Obj::cmd("app_key").opt("keys", arg("keys")).spread(&common)
            }
            Some("scroll") => Obj::cmd("app_scroll")
                .opt("index", arg("index"))
                .opt("x", arg("x"))
                .opt("y", arg("y"))
                .set(
                    "direction",
                    js::defined(input.get("direction")).map_or(json!("down"), js::roundtrip),
                )
                .set(
                    "amount",
                    js::defined(input.get("amount")).map_or(json!(1), js::roundtrip),
                )
                .spread(&common),
            Some("menu") => {
                let path = input
                    .get("path")
                    .filter(|p| p.is_array())
                    .map_or(json!([]), js::roundtrip);
                let press = path.as_array().is_some_and(|p| p.len() > 1);
                Obj::cmd("app_menu")
                    .set("path", path)
                    .set("press", press)
                    .spread(&common)
            }
            Some("action") => Obj::cmd("app_action")
                .opt("index", arg("index"))
                .set(
                    "action",
                    js::defined(input.get("name")).map_or(json!(""), js::roundtrip),
                )
                .spread(&common),
            Some("show") => Obj::cmd("app_reveal").spread(&common),
            _ => {
                return Err(MacError::Input(format!(
                    "unknown action \"{}\"",
                    js::text(action)
                )));
            }
        };
        let is_state = name == Some("state");
        if !is_state {
            request = request.set("stateAfter", true);
        }
        request = request.set("settle", js::num(0.6));

        let target = self.target(input.get("index"));
        let res = call(client, &request, CallOptions::timeout_ms(90_000), cancel).await?;
        if !js::truthy(res.get("ok")) {
            return Err(output::helper_failure(&res, "app control failed"));
        }
        let state = if is_state {
            Some(&res)
        } else {
            js::defined(res.get("state"))
        };
        if let Some(tree) = state
            .and_then(|s| s.get("tree"))
            .filter(|t| js::truthy(Some(t)))
        {
            self.remember(&js::text(Some(tree)));
        }
        let header = if js::truthy(res.get("items")) {
            let path = match path_text(input.get("path")) {
                p if p.is_empty() => "bar".to_owned(),
                p => p,
            };
            let items = match res.get("items") {
                Some(Value::Array(items)) => js::join(items, " | "),
                other => js::text(other),
            };
            format!("Menu {path} contains: {items}")
        } else if !is_state {
            format!(
                "{} — done via {}.",
                describe(input).unwrap_or_default(),
                js::text(res.get("method"))
            )
        } else {
            String::new()
        };
        let Some(state) = state.filter(|s| js::truthy(Some(s))) else {
            return Ok(RunResult {
                text: header,
                ..Default::default()
            });
        };
        Ok(RunResult {
            text: state_text(state, &header),
            image: Some(Shot {
                mime: state.get("mime").cloned(),
                data: state.get("data").cloned(),
                width: state.get("width").cloned(),
                height: state.get("height").cloned(),
            }),
            cursor: js::defined(res.get("aim")).cloned(),
            target: target.map(Value::from),
            // registerApps: `{ ...result, place: result.app }`
            place: state.get("app").cloned(),
        })
    }
}

/// One helper call that gives up when the turn is cancelled.
async fn call(
    client: &crate::CuaClient,
    request: &Obj,
    options: CallOptions,
    cancel: &CancellationToken,
) -> Result<Value, MacError> {
    tokio::select! {
        res = client.call(&request.0, options) => Ok(res?),
        _ = cancel.cancelled() => Err(MacError::Aborted),
    }
}

#[async_trait]
impl Tool for AppTool {
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
        self.helper.setup().await;
        let result = self.run(&args, &ctx.cancel).await?;
        Ok(output::finish(TOOL_NAME, describe(&args), &args, result, &ctx, None).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_names_follow_the_js_regex() {
        let tree = "[0] AXButton \"Save\" @(10,20)\n[1] AXTextField \"\" value=\"x\" @(5,5)\n  [2] AXButton \"Indented\"\n[3] AXGroup @(1,1)\n[12] AXMenuItem \"Neue Notiz\"";
        assert_eq!(
            outline_names(tree),
            vec![
                (0, "Save".into()),
                (1, String::new()),
                (12, "Neue Notiz".into())
            ]
        );
        // `[^"]*` may run across lines; the scan resumes after the match.
        assert_eq!(
            outline_names("[4] AXRow \"multi\nline\" x\n[5] AXCell \"ok\""),
            vec![(4, "multi\nline".into()), (5, "ok".into())]
        );
    }

    #[test]
    fn describe_and_activity() {
        assert_eq!(
            describe(&json!({"action": "click", "index": 3})).unwrap(),
            "Click [3]"
        );
        assert_eq!(
            describe(&json!({"action": "click", "x": 10.5, "y": 2})).unwrap(),
            "Click (11, 2)"
        );
        assert_eq!(
            describe(&json!({"action": "menu", "path": ["Ablage", "Neue Notiz"]})).unwrap(),
            "Menu Ablage › Neue Notiz"
        );
        assert_eq!(
            activity(&json!({"action": "type", "text": "hi"}), "de"),
            "Tippt „hi“"
        );
        assert_eq!(activity(&json!({"action": "list_apps"}), "en"), "list_apps");
    }
}
