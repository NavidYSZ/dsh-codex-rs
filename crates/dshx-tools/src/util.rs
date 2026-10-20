//! Small text helpers that keep model-visible output identical to dsh, which
//! counts string lengths in UTF-16 code units (JavaScript `string.length`).

/// Length of `s` in UTF-16 code units (JavaScript `s.length`).
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The longest prefix of `s` with at most `max` UTF-16 code units. A
/// surrogate pair is never split (JavaScript would leave half of it).
pub(crate) fn utf16_prefix(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, ch) in s.char_indices() {
        units += ch.len_utf16();
        if units > max {
            return &s[..index];
        }
    }
    s
}

/// The longest prefix of `s` with at most `max` bytes, cut on a char boundary.
pub(crate) fn utf8_prefix(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The longest suffix of `s` with at most `max` bytes, cut on a char boundary.
pub(crate) fn utf8_suffix(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Formats a number the way JavaScript template literals do for the values
/// tools print (millisecond counts): integers without a fraction.
pub(crate) fn js_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// `JSON.stringify` of a string.
pub(crate) fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    dshx_core::now_ms()
}

/// Random lowercase hex string of `len` characters.
pub(crate) fn random_hex(len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_helpers_count_like_javascript() {
        assert_eq!(utf16_len("a😀"), 3);
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(utf8_prefix("aé", 2), "a");
        assert_eq!(utf8_suffix("éa", 2), "a");
        assert_eq!(js_number(120000.0), "120000");
        assert_eq!(js_number(1500.5), "1500.5");
    }
}
