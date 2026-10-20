//! Small JavaScript semantics the port depends on (string lengths in UTF-16
//! units, number formatting, `Math.round`, `encodeURIComponent`, `\s`), so
//! texts come out exactly like the original plugin's.
use serde_json::Value;

/// `s.length` in JavaScript.
pub(crate) fn len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)`; never splits a character (JS could leave half a surrogate pair).
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

/// `clip` from browser-tool.js: cut to `n` units and add an ellipsis.
pub(crate) fn clip(s: &str, n: usize) -> String {
    if len(s) > n {
        format!("{}…", slice(s, n))
    } else {
        s.to_owned()
    }
}

/// `Math.round`: halves round towards +∞.
pub(crate) fn round(x: f64) -> f64 {
    if !x.is_finite() || x.fract() == 0.0 {
        return x;
    }
    (x + 0.5).floor()
}

/// `String(x)` for a number.
pub(crate) fn num(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if x.fract() == 0.0 && x.abs() < 1e21 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// A number as JSON the way `JSON.stringify` writes it (no `.0` on integers).
pub(crate) fn json_num(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9.0e15 {
        Value::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

/// Template-literal interpolation of an argument (`${input.tab}`).
pub(crate) fn display(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => num(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Characters matched by `\s` in a JavaScript regular expression.
pub(crate) fn is_space(c: char) -> bool {
    matches!(c, '\t'..='\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}')
        || matches!(
            c,
            '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
        )
}

/// `String.prototype.trim`.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `encodeURIComponent`.
pub(crate) fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_semantics() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(10.4), 10.0);
        assert_eq!(num(1280.0), "1280");
        assert_eq!(num(2.5), "2.5");
        assert_eq!(num(f64::NAN), "NaN");
        assert_eq!(len("😀a"), 3);
        assert_eq!(slice("a😀b", 2), "a");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(encode_uri_component("a b/ß?"), "a%20b%2F%C3%9F%3F");
        assert_eq!(json_num(3.0), serde_json::json!(3));
        assert_eq!(json_num(3.5), serde_json::json!(3.5));
    }
}
