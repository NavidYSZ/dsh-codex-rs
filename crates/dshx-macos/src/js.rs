//! The bits of JavaScript semantics the original plugin's texts and requests
//! depend on (template strings, truthiness, `Math.round`, UTF-16 `slice`,
//! `JSON.stringify` of numbers). Tool arguments reach the JS tools unvalidated,
//! so the port works on raw JSON values with the same rules.
use serde_json::{Map, Number, Value};

/// `${v}` in a template literal; `None` is `undefined`.
pub(crate) fn text(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => number_text(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => join(items, ","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// `Array.prototype.join`: null entries become empty strings.
pub(crate) fn join(items: &[Value], sep: &str) -> String {
    items
        .iter()
        .map(|v| {
            if v.is_null() {
                String::new()
            } else {
                text(Some(v))
            }
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// `String(n)`.
pub(crate) fn number_text(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f == 0.0 {
        return "0".into();
    }
    if f.fract() == 0.0 && f.abs() < 1e21 {
        return format!("{f:.0}");
    }
    format!("{f}")
}

/// JS truthiness.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The left side of `a ?? b`: present and not null.
pub(crate) fn defined(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

/// `typeof v === 'number' ? v : …`
pub(crate) fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// `Number(v)` (arrays and objects are NaN here).
pub(crate) fn to_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `Math.round`: halves go up (towards +∞), unlike `f64::round`.
pub(crate) fn round(f: f64) -> f64 {
    if !f.is_finite() {
        return f;
    }
    let floor = f.floor();
    if f - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// A JS number as `JSON.stringify` writes it: integral values without a
/// fraction, NaN and ±∞ as null.
pub(crate) fn num(f: f64) -> Value {
    if !f.is_finite() {
        return Value::Null;
    }
    if f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_992.0 {
        return Value::from(f as i64);
    }
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A value after a JS parse + stringify round trip (`12.0` becomes `12`).
pub(crate) fn roundtrip(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => num(n.as_f64().unwrap_or(f64::NAN)),
        Value::Array(items) => Value::Array(items.iter().map(roundtrip).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), roundtrip(v))).collect())
        }
        other => other.clone(),
    }
}

/// `s.slice(0, n)` counted in UTF-16 code units (never splits a character).
pub(crate) fn slice(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

/// `s.length`.
pub(crate) fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// An object literal built key by key. Setting an existing key keeps its
/// position, like a later key in `{ ...a, key }`; `undefined` values are
/// skipped, like `JSON.stringify` does.
#[derive(Debug, Clone, Default)]
pub(crate) struct Obj(pub Map<String, Value>);

impl Obj {
    pub fn new() -> Self {
        Obj(Map::new())
    }
    pub fn cmd(cmd: &str) -> Self {
        Obj::new().set("cmd", cmd)
    }
    pub fn set(mut self, key: &str, v: impl Into<Value>) -> Self {
        self.0.insert(key.to_owned(), v.into());
        self
    }
    /// `key: maybeUndefined`
    pub fn opt(mut self, key: &str, v: Option<Value>) -> Self {
        if let Some(v) = v {
            self.0.insert(key.to_owned(), v);
        }
        self
    }
    /// `...other`
    pub fn spread(mut self, other: &Obj) -> Self {
        for (k, v) in &other.0 {
            self.0.insert(k.clone(), v.clone());
        }
        self
    }
    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_print_like_js() {
        assert_eq!(number_text(1280.0), "1280");
        assert_eq!(number_text(0.5), "0.5");
        assert_eq!(number_text(-0.0), "0");
        assert_eq!(number_text(f64::NAN), "NaN");
        assert_eq!(text(Some(&json!(12.0))), "12");
        assert_eq!(text(None), "undefined");
        assert_eq!(text(Some(&json!(["a", null, 3]))), "a,,3");
        assert_eq!(
            serde_json::to_string(&roundtrip(&json!({"x": 12.0, "y": [1.5, 2.0]}))).unwrap(),
            r#"{"x":12,"y":[1.5,2]}"#
        );
    }

    #[test]
    fn math_round_rounds_halves_up() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(-2.6), -3.0);
        assert_eq!(round(0.49), 0.0);
    }

    #[test]
    fn slice_counts_utf16_units() {
        assert_eq!(slice("abc", 2), "ab");
        assert_eq!(slice("a😀b", 2), "a");
        assert_eq!(slice("a😀b", 3), "a😀");
        assert_eq!(len16("a😀b"), 4);
    }

    #[test]
    fn truthiness() {
        assert!(!truthy(Some(&json!(""))));
        assert!(!truthy(Some(&json!(0))));
        assert!(truthy(Some(&json!([]))));
        assert!(!truthy(None));
    }
}
