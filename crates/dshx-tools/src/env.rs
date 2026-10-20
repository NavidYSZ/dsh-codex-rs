//! Child environment of model shell commands (dsh `subprocess.scrubbedParentEnv`,
//! bash-local `ENV_OVERRIDES`, `shell-env`): the parent environment without
//! credentials and without stale `DSH_*` facts, then the model-friendly
//! terminal overrides, then the current managed `DSH_*` facts.
use std::ffi::OsString;

/// Prefix of the harness-managed variables.
pub const DSH_ENV_PREFIX: &str = "DSH_";

/// Terminal overrides that keep tool output free of colours and pagers.
pub const ENV_OVERRIDES: [(&str, &str); 4] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
];

/// dsh `SENSITIVE_ENV_PATTERN` (`/KEY|PASSWORD|SECRET|TOKEN/i`).
pub fn is_sensitive(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|needle| upper.contains(needle))
}

/// The managed `DSH_*` facts for one call, sorted by name.
pub fn managed_env(session_id: &str) -> Vec<(String, String)> {
    let home = dshx_core::config::home_dir();
    let mut vars = vec![
        ("DSH_HOME".to_owned(), home.to_string_lossy().into_owned()),
        ("DSH_SESSION_ID".to_owned(), session_id.to_owned()),
        ("DSH_SHELL".to_owned(), "1".to_owned()),
    ];
    if session_id.is_empty() {
        vars.retain(|(k, _)| k != "DSH_SESSION_ID");
    }
    vars.sort();
    vars
}

/// The complete environment for a model shell command.
pub fn child_env(session_id: &str) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !is_sensitive(&key) && !key.to_ascii_uppercase().starts_with(DSH_ENV_PREFIX)
        })
        .collect();
    let mut set = |key: &str, value: &str| {
        env.retain(|(k, _)| k != key);
        env.push((key.into(), value.into()));
    };
    for (key, value) in ENV_OVERRIDES {
        set(key, value);
    }
    for (key, value) in managed_env(session_id) {
        set(&key, &value);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_scrubbed() {
        assert!(is_sensitive("DEEPSEEK_API_KEY"));
        assert!(is_sensitive("github_token"));
        assert!(is_sensitive("DB_PASSWORD"));
        assert!(is_sensitive("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_sensitive("PATH"));
        let env = child_env("s-1");
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("TERM").as_deref(), Some("dumb"));
        assert_eq!(get("NO_COLOR").as_deref(), Some("1"));
        assert_eq!(get("PAGER").as_deref(), Some("cat"));
        assert_eq!(get("GIT_PAGER").as_deref(), Some("cat"));
        assert_eq!(get("DSH_SHELL").as_deref(), Some("1"));
        assert_eq!(get("DSH_SESSION_ID").as_deref(), Some("s-1"));
        assert!(env.iter().all(|(k, _)| !is_sensitive(&k.to_string_lossy())));
    }
}
