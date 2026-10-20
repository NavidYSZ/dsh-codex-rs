//! YAML frontmatter of skill files (dsh `skill-filesystem` `parseFrontmatter`,
//! `parseInvocationPolicy`). YAML 1.2 core schema like the `yaml` npm package:
//! `yes`/`no` stay strings, duplicate keys are an error.
use serde_json::{Map, Number, Value};
use yaml_rust2::{Yaml, YamlLoader};

/// Split `raw` into its YAML mapping and the body after the closing `---`.
///
/// `Ok(None)` means "no frontmatter" (no opening/closing line, or the YAML is
/// not a mapping); `Err` carries a YAML syntax error.
/// Frontmatter mapping and the body that follows it.
pub(crate) type Parsed<'a> = (Map<String, Value>, &'a str);

pub(crate) fn parse(raw: &str) -> Result<Option<Parsed<'_>>, String> {
    let Some(first_end) = raw.find('\n') else {
        return Ok(None);
    };
    if raw[..first_end].trim_end_matches('\r') != "---" {
        return Ok(None);
    }
    let start = first_end + 1;
    let Some((yaml_end, body_start)) = closing(raw, start) else {
        return Ok(None);
    };
    let data = match load(&raw[start..yaml_end])? {
        Value::Object(map) => map,
        _ => return Ok(None),
    };
    Ok(Some((data, &raw[body_start..])))
}

/// The closing `---` line at or after `start`: (start of that line, body start).
fn closing(raw: &str, start: usize) -> Option<(usize, usize)> {
    let mut line_start = start;
    while line_start <= raw.len() {
        let next = raw[line_start..].find('\n').map(|i| line_start + i);
        let line_end = next.unwrap_or(raw.len());
        if raw[line_start..line_end].trim_end_matches('\r') == "---" {
            return Some((line_start, next.map_or(raw.len(), |n| n + 1)));
        }
        line_start = next? + 1;
    }
    None
}

fn load(yaml: &str) -> Result<Value, String> {
    let docs = YamlLoader::load_from_str(yaml).map_err(|e| e.to_string())?;
    if docs.len() > 1 {
        return Err("expected a single YAML document".into());
    }
    Ok(docs.into_iter().next().map_or(Value::Null, to_json))
}

fn to_json(yaml: Yaml) -> Value {
    match yaml {
        Yaml::Real(text) => yaml_float(&text)
            .and_then(Number::from_f64)
            .map_or(Value::String(text), Value::Number),
        Yaml::Integer(n) => Value::Number(n.into()),
        Yaml::String(s) => Value::String(s),
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Array(items) => Value::Array(items.into_iter().map(to_json).collect()),
        Yaml::Hash(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (key_string(k), to_json(v)))
                .collect(),
        ),
        Yaml::Alias(_) | Yaml::Null | Yaml::BadValue => Value::Null,
    }
}

fn yaml_float(text: &str) -> Option<f64> {
    let lower = text.to_ascii_lowercase();
    match lower.trim_start_matches(['+', '-']) {
        ".inf" | ".nan" => None,
        _ => lower.parse().ok(),
    }
}

/// Map keys become strings the way JavaScript stringifies object keys.
fn key_string(key: Yaml) -> String {
    match key {
        Yaml::String(s) | Yaml::Real(s) => s,
        Yaml::Integer(n) => n.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Null => "null".into(),
        other => format!("{other:?}"),
    }
}

/// A non-empty string field (dsh `stringField`).
pub(crate) fn string_field(data: &Map<String, Value>, key: &str) -> Option<String> {
    match data.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// `(model_invocable, user_invocable)` from `disable-model-invocation` and
/// `user-invocable`; camelCase spellings are rejected like dsh does.
pub(crate) fn invocation(data: &Map<String, Value>) -> Result<(bool, bool), String> {
    for (legacy, canonical) in [
        ("disableModelInvocation", "disable-model-invocation"),
        ("modelInvocable", "disable-model-invocation"),
        ("userInvocable", "user-invocable"),
    ] {
        if data.contains_key(legacy) {
            return Err(format!(
                "frontmatter field \"{legacy}\" is unsupported; use \"{canonical}\""
            ));
        }
    }
    let disable_model = boolean(data, "disable-model-invocation")?;
    let user = boolean(data, "user-invocable")?;
    Ok((disable_model != Some(true), user != Some(false)))
}

fn boolean(data: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    let Some(value) = data.get(key) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
        Value::String(s) => match s.to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    };
    parsed
        .map(Some)
        .ok_or_else(|| format!("frontmatter field \"{key}\" must be a boolean"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let (data, body) = parse("---\nname: a\ndescription: b\n---\nbody\n")
            .unwrap()
            .unwrap();
        assert_eq!(data["name"], "a");
        assert_eq!(body, "body\n");
        let (_, body) = parse("---\r\nname: a\r\n---\r\nx\r\n").unwrap().unwrap();
        assert_eq!(body, "x\r\n");
        // Closing line at end of file without a newline.
        let (_, body) = parse("---\nname: a\n---").unwrap().unwrap();
        assert_eq!(body, "");
    }

    #[test]
    fn missing_or_non_mapping_frontmatter() {
        assert!(parse("no frontmatter\n").unwrap().is_none());
        assert!(parse("---\nname: a\n").unwrap().is_none());
        assert!(parse("---\n---\nbody").unwrap().is_none());
        assert!(parse("---\n- a\n- b\n---\n").unwrap().is_none());
        assert!(parse("\u{feff}---\nname: a\n---\n").unwrap().is_none());
        assert!(parse("---\nname: [unclosed\n---\n").is_err());
        assert!(parse("---\nname: a\nname: b\n---\n").is_err());
    }

    #[test]
    fn yaml_1_2_scalars() {
        let (data, _) = parse("---\na: yes\nb: true\nc: 12\nd: 1.5\ne: ~\nf: \"x\\ny\"\n---\n")
            .unwrap()
            .unwrap();
        assert_eq!(data["a"], "yes");
        assert_eq!(data["b"], true);
        assert_eq!(data["c"], 12);
        assert_eq!(data["d"], 1.5);
        assert_eq!(data["e"], Value::Null);
        assert_eq!(data["f"], "x\ny");
        assert_eq!(string_field(&data, "c"), None);
    }

    #[test]
    fn invocation_policy() {
        let map = |yaml: &str| parse(&format!("---\n{yaml}\n---\n")).unwrap().unwrap().0;
        assert_eq!(invocation(&map("a: 1")), Ok((true, true)));
        assert_eq!(
            invocation(&map("disable-model-invocation: true\nuser-invocable: no")),
            Ok((false, false))
        );
        assert_eq!(invocation(&map("user-invocable: 'OFF'")), Ok((true, false)));
        assert_eq!(
            invocation(&map("disable-model-invocation: 1")),
            Ok((false, true))
        );
        assert!(invocation(&map("user-invocable: maybe")).is_err());
        assert!(invocation(&map("modelInvocable: false")).is_err());
    }
}
