//! Model-facing tool names (dsh `publicToolName`, a pinned v1 contract).
use sha2::{Digest, Sha256};

/// DeepSeek function names: at most 64 characters of `[A-Za-z0-9_-]`.
const MAX_PUBLIC_NAME_LENGTH: usize = 64;
/// Hex characters of the identity hash appended on lossy normalization.
const HASH_LENGTH: usize = 12;

/// `mcp__<server>__<raw>`; when characters must be replaced or the name is
/// too long, the normalized name is cut and a SHA-256 of `server\0raw` is
/// appended so distinct tools never collide.
pub fn public_tool_name(server: &str, raw: &str) -> String {
    let joined = format!("mcp__{server}__{raw}");
    // JavaScript replaces per UTF-16 unit; astral characters become two `_`.
    let normalized: String = joined
        .chars()
        .flat_map(|c| {
            let keep = c.is_ascii_alphanumeric() || c == '_' || c == '-';
            let n = if keep { 1 } else { c.len_utf16() };
            std::iter::repeat_n(if keep { c } else { '_' }, n)
        })
        .collect();
    if normalized == joined && normalized.len() <= MAX_PUBLIC_NAME_LENGTH {
        return normalized;
    }
    let hash: String = Sha256::digest(format!("{server}\0{raw}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let cut = MAX_PUBLIC_NAME_LENGTH - HASH_LENGTH - 1;
    let prefix: String = normalized.chars().take(cut).collect();
    format!("{prefix}_{}", &hash[..HASH_LENGTH])
}

/// `[A-Za-z0-9_-]{1,32}`.
pub fn is_server_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_names_are_verbatim() {
        assert_eq!(
            public_tool_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
    }

    #[test]
    fn lossy_names_get_a_hash() {
        // Value taken from dsh 0.2.0-rc.2 against tests/fixtures/mcp_server.py.
        assert_eq!(
            public_tool_name("t", "weird name.with/dots"),
            "mcp__t__weird_name_with_dots_10f7f669ff8b"
        );
        let long = "x".repeat(80);
        let name = public_tool_name("s", &long);
        assert_eq!(name.len(), 64);
        assert!(name.starts_with("mcp__s__xxx"));
    }

    #[test]
    fn server_names() {
        assert!(is_server_name("a-b_C9"));
        assert!(!is_server_name(""));
        assert!(!is_server_name("a.b"));
        assert!(!is_server_name(&"a".repeat(33)));
    }
}
