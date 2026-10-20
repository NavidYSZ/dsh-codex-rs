//! Tool-argument validation with dsh's JSON-schema subset and its exact
//! diagnostics (`packages/core/tools/src/json-schema.ts`), so invalid calls read
//! `Error: invalid arguments: ...` exactly like dsh.
use serde_json::Value;

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

/// Path-qualified violations of `value` against `schema`; empty means valid.
pub(crate) fn validate(schema: &Value, value: &Value) -> Vec<String> {
    check(schema, value, "")
}

fn check(node: &Value, value: &Value, path: &str) -> Vec<String> {
    let Some(node) = node.as_object() else {
        return Vec::new();
    };
    if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
        let matches = branches
            .iter()
            .filter(|b| check(b, value, path).is_empty())
            .count();
        return if matches == 1 {
            Vec::new()
        } else {
            vec![format!(
                "\"{}\" must match exactly one oneOf branch (matched {matches})",
                diagnostic_path(path)
            )]
        };
    }
    match node.get("type") {
        Some(Value::String(t)) => check_typed(node, t, value, path),
        Some(Value::Array(types)) => {
            // Outside dsh's subset; accept the first matching member type.
            for t in types.iter().filter_map(Value::as_str) {
                if type_matches(t, value) {
                    return check_typed(node, t, value, path);
                }
            }
            let first = types
                .iter()
                .filter_map(Value::as_str)
                .next()
                .unwrap_or("object");
            check_typed(node, first, value, path)
        }
        _ => Vec::new(),
    }
}

fn type_matches(t: &str, value: &Value) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => is_integer(value),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

fn check_scalar(node: &serde_json::Map<String, Value>, value: &Value, path: &str) -> Vec<String> {
    if let Some(allowed) = node.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|a| json_equal(a, value))
    {
        return vec![format!(
            "\"{}\" must be one of {}",
            diagnostic_path(path),
            serde_json::to_string(allowed).unwrap_or_default()
        )];
    }
    if let Some(c) = node.get("const")
        && !json_equal(c, value)
    {
        return vec![format!(
            "\"{}\" must be {}",
            diagnostic_path(path),
            serde_json::to_string(c).unwrap_or_default()
        )];
    }
    Vec::new()
}

/// JS `===` over JSON scalars (numbers compare by value).
fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn check_typed(
    node: &serde_json::Map<String, Value>,
    t: &str,
    value: &Value,
    path: &str,
) -> Vec<String> {
    let dp = diagnostic_path(path);
    match t {
        "object" => {
            let Some(obj) = value.as_object() else {
                return vec![format!("\"{dp}\" must be an object")];
            };
            let empty = serde_json::Map::new();
            let props = node
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let mut violations = Vec::new();
            if let Some(required) = node.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(key) {
                        violations.push(format!(
                            "missing required property \"{}\"",
                            property_path(path, key)
                        ));
                    }
                }
            }
            for (key, child) in props {
                if let Some(v) = obj.get(key) {
                    violations.extend(check(child, v, &property_path(path, key)));
                }
            }
            if node.get("additionalProperties") == Some(&Value::Bool(false)) {
                for key in obj.keys() {
                    if !props.contains_key(key) {
                        violations.push(format!(
                            "\"{}\" is not a declared property (additionalProperties: false)",
                            property_path(path, key)
                        ));
                    }
                }
            }
            violations
        }
        "array" => {
            let Some(items) = value.as_array() else {
                return vec![format!("\"{dp}\" must be an array")];
            };
            let mut violations = Vec::new();
            if let Some(schema) = node.get("items") {
                for (i, item) in items.iter().enumerate() {
                    violations.extend(check(schema, item, &format!("{path}[{i}]")));
                }
            }
            violations
        }
        "string" => {
            if value.is_string() {
                check_scalar(node, value, path)
            } else {
                vec![format!("\"{dp}\" must be a string")]
            }
        }
        "number" => {
            if value.is_number() {
                check_scalar(node, value, path)
            } else {
                vec![format!("\"{dp}\" must be a number")]
            }
        }
        "integer" => {
            if is_integer(value) {
                check_scalar(node, value, path)
            } else {
                vec![format!("\"{dp}\" must be an integer")]
            }
        }
        "boolean" => {
            if value.is_boolean() {
                check_scalar(node, value, path)
            } else {
                vec![format!("\"{dp}\" must be a boolean")]
            }
        }
        "null" => {
            if value.is_null() {
                check_scalar(node, value, path)
            } else {
                vec![format!("\"{dp}\" must be null")]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn todo_schema() -> Value {
        json!({"type": "object", "properties": {"todos": {"type": "array", "items": {"type": "object",
            "additionalProperties": false, "properties": {"content": {"type": "string"},
            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}},
            "required": ["content", "status"]}}}, "required": ["todos"]})
    }

    #[test]
    fn dsh_wording() {
        let s = todo_schema();
        let v = json!({"todos": [{"content": "Do it", "status": "in_progress", "activeForm": "Doing it"}]});
        assert_eq!(
            validate(&s, &v),
            vec![
                "\"todos[0].activeForm\" is not a declared property (additionalProperties: false)"
            ]
        );
        assert_eq!(
            validate(&s, &json!("{\"todos\": ")),
            vec!["\"arguments\" must be an object"]
        );
        assert_eq!(
            validate(&s, &json!({})),
            vec!["missing required property \"todos\""]
        );
        assert_eq!(
            validate(&s, &json!({"todos": [{"content": 1, "status": "x"}]})),
            vec![
                "\"todos[0].content\" must be a string",
                "\"todos[0].status\" must be one of [\"pending\",\"in_progress\",\"completed\"]"
            ]
        );
    }
}
