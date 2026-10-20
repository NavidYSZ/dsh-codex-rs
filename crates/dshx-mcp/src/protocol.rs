//! Validation and normalization of MCP results the way the official SDK's
//! schemas admit them: known keys first in schema order, unknown keys after
//! them (loose objects) or dropped (strict objects). dsh renders the
//! normalized values, so key order is model-visible.
use serde_json::{Map, Value};

pub(crate) const PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const SUPPORTED_VERSIONS: [&str; 4] =
    ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// One tool of a `tools/list` result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawTool {
    pub name: String,
    pub description: String,
    /// `type`, `properties`, `required` first, then the remaining keys.
    pub input_schema: Value,
    pub has_output_schema: bool,
    pub task_required: bool,
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{path}: expected an object"))
}

fn string<'a>(map: &'a Map<String, Value>, key: &str, path: &str) -> Result<&'a str, String> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path}.{key}: expected a string"))
}

fn optional_string(map: &Map<String, Value>, key: &str, path: &str) -> Result<(), String> {
    match map.get(key) {
        None | Some(Value::String(_)) => Ok(()),
        Some(_) => Err(format!("{path}.{key}: expected a string")),
    }
}

/// Copies `keys` (in this order) that exist in `map`, then, for loose
/// objects, every other key in input order.
fn reorder(map: &Map<String, Value>, keys: &[&str], loose: bool) -> Map<String, Value> {
    let mut out = Map::new();
    for key in keys {
        if let Some(v) = map.get(*key) {
            out.insert((*key).to_owned(), v.clone());
        }
    }
    if loose {
        for (k, v) in map {
            if !keys.contains(&k.as_str()) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// `tools/list` page → tools plus the next cursor.
pub(crate) fn tools_page(result: &Value) -> Result<(Vec<RawTool>, Option<String>), String> {
    let map = object(result, "result")?;
    let tools = map
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("tools: expected an array")?;
    let mut out = Vec::new();
    for (i, tool) in tools.iter().enumerate() {
        let path = format!("tools[{i}]");
        let t = object(tool, &path)?;
        let name = string(t, "name", &path)?;
        optional_string(t, "description", &path)?;
        let schema = object(
            t.get("inputSchema").unwrap_or(&Value::Null),
            &format!("{path}.inputSchema"),
        )?;
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(format!("{path}.inputSchema.type: expected \"object\""));
        }
        if schema.get("properties").is_some_and(|p| !p.is_object()) {
            return Err(format!("{path}.inputSchema.properties: expected an object"));
        }
        if let Some(required) = schema.get("required")
            && !required
                .as_array()
                .is_some_and(|r| r.iter().all(Value::is_string))
        {
            return Err(format!(
                "{path}.inputSchema.required: expected an array of strings"
            ));
        }
        if t.get("outputSchema").is_some_and(|o| !o.is_object()) {
            return Err(format!("{path}.outputSchema: expected an object"));
        }
        out.push(RawTool {
            name: name.to_owned(),
            description: t
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            input_schema: Value::Object(reorder(schema, &["type", "properties", "required"], true)),
            has_output_schema: t.get("outputSchema").is_some(),
            task_required: t
                .get("execution")
                .and_then(|e| e.get("taskSupport"))
                .is_some_and(|s| s == "required"),
        });
    }
    Ok((out, cursor(map)))
}

fn cursor(map: &Map<String, Value>) -> Option<String> {
    map.get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A validated `tools/call` result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallResult {
    pub content: Vec<Value>,
    pub structured: Option<Value>,
    pub is_error: bool,
}

pub(crate) fn call_result(result: &Value) -> Result<CallResult, String> {
    let map = object(result, "result")?;
    let content = match map.get("content") {
        None => Vec::new(),
        Some(Value::Array(blocks)) => blocks.clone(),
        Some(_) => return Err("content: expected an array".into()),
    };
    for (i, block) in content.iter().enumerate() {
        content_block(block, &format!("content[{i}]"))?;
    }
    let is_error = match map.get("isError") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("isError: expected a boolean".into()),
    };
    Ok(CallResult {
        content,
        structured: map.get("structuredContent").cloned(),
        is_error,
    })
}

fn content_block(block: &Value, path: &str) -> Result<(), String> {
    let b = object(block, path)?;
    match b.get("type").and_then(Value::as_str) {
        Some("text") => string(b, "text", path).map(drop),
        Some("image" | "audio") => {
            string(b, "data", path)?;
            string(b, "mimeType", path).map(drop)
        }
        Some("resource_link") => {
            string(b, "name", path)?;
            string(b, "uri", path).map(drop)
        }
        Some("resource") => {
            let r = object(
                b.get("resource").unwrap_or(&Value::Null),
                &format!("{path}.resource"),
            )?;
            string(r, "uri", &format!("{path}.resource"))?;
            if r.get("text").is_some_and(Value::is_string)
                || r.get("blob").is_some_and(Value::is_string)
            {
                Ok(())
            } else {
                Err(format!("{path}.resource: expected text or blob"))
            }
        }
        Some(other) => Err(format!("{path}.type: unsupported content type \"{other}\"")),
        None => Err(format!("{path}.type: expected a string")),
    }
}

/// The list item key of a paginated resource method.
pub(crate) fn list_key(method: &str) -> &'static str {
    if method == "resources/templates/list" {
        "resourceTemplates"
    } else {
        "resources"
    }
}

/// Normalizes one page of `resources/list` or `resources/templates/list`.
pub(crate) fn resource_list(method: &str, result: &Value) -> Result<Map<String, Value>, String> {
    let key = list_key(method);
    let map = object(result, "result")?;
    let items = map
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key}: expected an array"))?;
    let mut normalized = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let path = format!("{key}[{i}]");
        let m = object(item, &path)?;
        string(m, "name", &path)?;
        let keys: &[&str] = if key == "resources" {
            string(m, "uri", &path)?;
            &[
                "name",
                "title",
                "icons",
                "uri",
                "description",
                "mimeType",
                "size",
                "annotations",
                "_meta",
            ]
        } else {
            string(m, "uriTemplate", &path)?;
            &[
                "name",
                "title",
                "icons",
                "uriTemplate",
                "description",
                "mimeType",
                "annotations",
                "_meta",
            ]
        };
        let mut out = reorder(m, keys, false);
        if let Some(Value::Array(icons)) = out.get_mut("icons") {
            for icon in icons.iter_mut() {
                if let Value::Object(o) = icon {
                    *o = reorder(o, &["src", "mimeType", "sizes", "theme"], false);
                }
            }
        }
        if let Some(Value::Object(a)) = out.get_mut("annotations") {
            *a = reorder(a, &["audience", "priority", "lastModified"], false);
        }
        normalized.push(Value::Object(out));
    }
    let mut out = reorder(map, &["_meta", "nextCursor", key], true);
    out.insert(key.to_owned(), Value::Array(normalized));
    Ok(out)
}

/// Normalizes a `resources/read` result.
pub(crate) fn read_result(result: &Value) -> Result<Map<String, Value>, String> {
    let map = object(result, "result")?;
    let contents = map
        .get("contents")
        .and_then(Value::as_array)
        .ok_or("contents: expected an array")?;
    let mut normalized = Vec::new();
    for (i, item) in contents.iter().enumerate() {
        let path = format!("contents[{i}]");
        let m = object(item, &path)?;
        string(m, "uri", &path)?;
        let keys: &[&str] = if m.get("text").is_some_and(Value::is_string) {
            &["uri", "mimeType", "_meta", "text"]
        } else if m.get("blob").is_some_and(Value::is_string) {
            &["uri", "mimeType", "_meta", "blob"]
        } else {
            return Err(format!("{path}: expected text or blob"));
        };
        normalized.push(Value::Object(reorder(m, keys, false)));
    }
    let mut out = reorder(map, &["_meta", "contents"], true);
    out.insert("contents".into(), Value::Array(normalized));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_schemas_are_reordered_like_the_sdk() {
        let (tools, next) = tools_page(
            &json!({"tools": [{"title": "T", "name": "o", "inputSchema": {
            "$schema": "s", "additionalProperties": false, "required": ["a"],
            "properties": {"b": {"type": "number"}, "a": {"description": "A", "type": "string"}},
            "type": "object"}}], "nextCursor": "c"}),
        )
        .unwrap();
        assert_eq!(next.as_deref(), Some("c"));
        assert_eq!(
            serde_json::to_string(&tools[0].input_schema).unwrap(),
            r#"{"type":"object","properties":{"b":{"type":"number"},"a":{"description":"A","type":"string"}},"required":["a"],"$schema":"s","additionalProperties":false}"#
        );
        assert_eq!(tools[0].description, "");
        assert!(
            tools_page(&json!({"tools": [{"name": "x", "inputSchema": {"type": "string"}}]}))
                .is_err()
        );
        assert!(tools_page(&json!({"tools": [{"inputSchema": {"type": "object"}}]})).is_err());
    }

    #[test]
    fn resource_shapes_match_zod_output() {
        // Expected strings produced by the SDK's schemas under Node.
        let page = resource_list(
            "resources/list",
            &json!({"zzz": 1, "resources": [{"extra": "x", "mimeType": "text/plain", "uri": "a://b", "name": "n", "size": 3, "title": "t"}], "nextCursor": "c", "_meta": {"k": 1}}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&page).unwrap(),
            r#"{"_meta":{"k":1},"nextCursor":"c","resources":[{"name":"n","title":"t","uri":"a://b","mimeType":"text/plain","size":3}],"zzz":1}"#
        );
        let read = read_result(&json!({"contents": [{"text": "x", "extra": 1, "uri": "u", "mimeType": "m"}, {"blob": "AAAA", "uri": "u2"}], "other": 2})).unwrap();
        assert_eq!(
            serde_json::to_string(&read).unwrap(),
            r#"{"contents":[{"uri":"u","mimeType":"m","text":"x"},{"uri":"u2","blob":"AAAA"}],"other":2}"#
        );
        let templates = resource_list(
            "resources/templates/list",
            &json!({"resourceTemplates": [{"uriTemplate": "x/{id}", "description": "d", "name": "n"}]}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&templates).unwrap(),
            r#"{"resourceTemplates":[{"name":"n","uriTemplate":"x/{id}","description":"d"}]}"#
        );
    }

    #[test]
    fn call_results_are_validated() {
        let ok = call_result(&json!({"content": [{"type": "text", "text": "a"}], "isError": true}))
            .unwrap();
        assert!(ok.is_error);
        assert!(call_result(&json!({})).unwrap().content.is_empty());
        let err =
            call_result(&json!({"content": [{"type": "text", "text": "a"}, {"type": "hologram"}]}))
                .unwrap_err();
        assert_eq!(
            err,
            "content[1].type: unsupported content type \"hologram\""
        );
        assert!(call_result(&json!({"content": [{"type": "image", "data": "x"}]})).is_err());
    }
}
