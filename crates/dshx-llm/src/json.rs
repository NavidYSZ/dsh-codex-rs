//! JSON output identical to JavaScript's `JSON.stringify`.
//!
//! dsh serializes every request body with `JSON.stringify`, so to send the
//! very same bytes (and keep DeepSeek's prefix cache warm across both
//! harnesses) we mirror its two differences from serde_json: integer-like
//! object keys come first in ascending order, and numbers use the
//! `Number.prototype.toString` format (`1` instead of `1.0`, `1e+21`).
use std::io;

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::Value;

/// Serializes `value` exactly like `JSON.stringify(value)`.
pub fn to_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, JsFormatter);
    // Writing into a Vec cannot fail and every Value is serializable.
    Js(value).serialize(&mut ser).expect("serialize JSON value");
    out
}

/// [`to_vec`] as a `String`.
pub fn to_string(value: &Value) -> String {
    String::from_utf8(to_vec(value)).expect("serde_json writes UTF-8")
}

struct Js<'a>(&'a Value);

impl Serialize for Js<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    s.serialize_u64(u)
                } else if let Some(i) = n.as_i64() {
                    s.serialize_i64(i)
                } else {
                    s.serialize_f64(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(text) => s.serialize_str(text),
            Value::Array(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Js(item))?;
                }
                seq.end()
            }
            Value::Object(map) => {
                let mut out = s.serialize_map(Some(map.len()))?;
                if map.keys().any(|k| array_index(k).is_some()) {
                    // JS property order: array indices ascending, then insertion order.
                    let mut indexed: Vec<(u32, &String, &Value)> = map
                        .iter()
                        .filter_map(|(k, v)| array_index(k).map(|i| (i, k, v)))
                        .collect();
                    indexed.sort_by_key(|(i, _, _)| *i);
                    for (_, k, v) in indexed {
                        out.serialize_entry(k, &Js(v))?;
                    }
                    for (k, v) in map.iter().filter(|(k, _)| array_index(k).is_none()) {
                        out.serialize_entry(k, &Js(v))?;
                    }
                } else {
                    for (k, v) in map {
                        out.serialize_entry(k, &Js(v))?;
                    }
                }
                out.end()
            }
        }
    }
}

/// A canonical array index (`0` … `2^32 - 2`), which JS objects order first.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    key.parse::<u32>().ok().filter(|i| *i != u32::MAX)
}

struct JsFormatter;

impl serde_json::ser::Formatter for JsFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(js_number(value).as_bytes())
    }
    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        // Shortest f32 text, read back as the f64 JS would hold for it.
        let value: f64 = value.to_string().parse().unwrap_or(value as f64);
        writer.write_all(js_number(value).as_bytes())
    }
}

/// `Number.prototype.toString()` for finite numbers (ECMA-262 Number::toString).
pub fn js_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value < 0.0 {
        return format!("-{}", js_number(-value));
    }
    // Rust's `{:e}` yields the shortest round-trip digits, like JS.
    let formatted = format!("{value:e}");
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let e = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{e}")
        } else {
            format!("{}.{}e{sign}{e}", &digits[..1], &digits[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_match_javascript() {
        for (value, expected) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (123.456, "123.456"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (-2.5, "-2.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (2.5e25, "2.5e+25"),
        ] {
            assert_eq!(js_number(value), expected, "{value}");
        }
    }

    #[test]
    fn integer_keys_come_first_like_js() {
        let value: Value = serde_json::from_str(r#"{"b":1,"10":2,"2":3,"01":4,"a":1.0}"#).unwrap();
        assert_eq!(to_string(&value), r#"{"2":3,"10":2,"b":1,"01":4,"a":1}"#);
    }

    #[test]
    fn strings_escape_like_json_stringify() {
        let value = json!({"t": "a\"b\\c\n\u{1f}\u{7f}é/"});
        assert_eq!(
            to_string(&value),
            "{\"t\":\"a\\\"b\\\\c\\n\\u001f\u{7f}é/\"}"
        );
    }
}
