//! Argument validation with dsh's wording (`core/tools/src/json-schema.ts`):
//! every tool checks its arguments against its upstream schema before it runs,
//! and the model reads `invalid arguments: <violation>; <violation>`.
use dshx_core::ToolError;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::schemas;

fn diagnostic_path(path: &str) -> &str {
    if path.is_empty() { "arguments" } else { path }
}

fn property_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn check_scalar(node: &Value, value: &Value, path: &str) -> Vec<String> {
    if let Some(allowed) = node.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|entry| entry == value)
    {
        let list = serde_json::to_string(allowed).unwrap_or_default();
        return vec![format!(
            "\"{}\" must be one of {list}",
            diagnostic_path(path)
        )];
    }
    if let Some(constant) = node.get("const")
        && constant != value
    {
        let text = serde_json::to_string(constant).unwrap_or_default();
        return vec![format!("\"{}\" must be {text}", diagnostic_path(path))];
    }
    Vec::new()
}

/// Violations of `value` against the schema node, in dsh's order.
pub fn validate(node: &Value, value: &Value, path: &str) -> Vec<String> {
    let at = diagnostic_path(path);
    match node.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return vec![format!("\"{at}\" must be an object")];
            };
            let mut violations = Vec::new();
            if let Some(required) = node.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        violations.push(format!(
                            "missing required property \"{}\"",
                            property_path(path, key)
                        ));
                    }
                }
            }
            let properties = node.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (key, child) in properties {
                    if let Some(entry) = object.get(key) {
                        violations.extend(validate(child, entry, &property_path(path, key)));
                    }
                }
            }
            if node.get("additionalProperties") == Some(&Value::Bool(false)) {
                for key in object.keys() {
                    if !properties.is_some_and(|p| p.contains_key(key)) {
                        violations.push(format!(
                            "\"{}\" is not a declared property (additionalProperties: false)",
                            property_path(path, key)
                        ));
                    }
                }
            }
            violations
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return vec![format!("\"{at}\" must be an array")];
            };
            match node.get("items") {
                Some(item_node) => items
                    .iter()
                    .enumerate()
                    .flat_map(|(index, item)| {
                        validate(item_node, item, &format!("{path}[{index}]"))
                    })
                    .collect(),
                None => Vec::new(),
            }
        }
        Some("string") => match value {
            Value::String(_) => check_scalar(node, value, path),
            _ => vec![format!("\"{at}\" must be a string")],
        },
        Some("number") => match value {
            Value::Number(_) => check_scalar(node, value, path),
            _ => vec![format!("\"{at}\" must be a number")],
        },
        Some("integer") => match value.as_f64() {
            Some(n) if n.fract() == 0.0 => check_scalar(node, value, path),
            _ => vec![format!("\"{at}\" must be an integer")],
        },
        Some("boolean") => match value {
            Value::Bool(_) => check_scalar(node, value, path),
            _ => vec![format!("\"{at}\" must be a boolean")],
        },
        Some("null") => match value {
            Value::Null => check_scalar(node, value, path),
            _ => vec![format!("\"{at}\" must be null")],
        },
        _ => Vec::new(),
    }
}

/// Validates `args` against the named tool's upstream schema, then decodes them.
pub(crate) fn parse<T: DeserializeOwned>(tool: &str, args: Value) -> Result<T, ToolError> {
    let violations = validate(schemas::parameters(tool), &args, "");
    if !violations.is_empty() {
        return Err(ToolError::InvalidArguments(format!(
            "invalid arguments: {}",
            violations.join("; ")
        )));
    }
    serde_json::from_value(args)
        .map_err(|e| ToolError::InvalidArguments(format!("invalid arguments: {e}")))
}

/// Shorthand for a plain failure the model reads as `Error: <message>`.
pub(crate) fn fail(message: impl Into<String>) -> ToolError {
    ToolError::Failed(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn violations_use_dsh_wording() {
        let schema = schemas::parameters("todo_write");
        let v = validate(schema, &json!({}), "");
        assert_eq!(v, vec!["missing required property \"todos\""]);
        let v = validate(
            schema,
            &json!({"todos": [{"content": "a", "status": "done", "x": 1}]}),
            "",
        );
        assert_eq!(
            v,
            vec![
                "\"todos[0].status\" must be one of [\"pending\",\"in_progress\",\"completed\"]",
                "\"todos[0].x\" is not a declared property (additionalProperties: false)",
            ]
        );
        let v = validate(schemas::parameters("bash"), &json!("ls"), "");
        assert_eq!(v, vec!["\"arguments\" must be an object"]);
        let v = validate(
            schemas::parameters("read"),
            &json!({"file_path": 3, "offset": "1"}),
            "",
        );
        assert_eq!(
            v,
            vec![
                "\"file_path\" must be a string",
                "\"offset\" must be a number"
            ]
        );
    }
}
