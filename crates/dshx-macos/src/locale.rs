//! Interface language of the helper (banner, mini window, activity pills).

/// Localized helper strings (`STRINGS` in `index.js`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strings {
    pub banner: &'static str,
    pub esc_hint: &'static str,
    pub done: &'static str,
    pub stopped: &'static str,
    pub stop: &'static str,
    pub open: &'static str,
}

pub const DE: Strings = Strings {
    banner: "DeepSeek verwendet deinen Computer",
    esc_hint: "Esc zum Abbrechen",
    done: "Fertig",
    stopped: "Gestoppt",
    stop: "Stoppen",
    open: "Anzeigen",
};

pub const EN: Strings = Strings {
    banner: "DeepSeek is using your computer",
    esc_hint: "Esc to cancel",
    done: "Done",
    stopped: "Stopped",
    stop: "Stop",
    open: "Show",
};

/// `STRINGS[locale] ?? STRINGS.en`
pub fn strings(locale: &str) -> Strings {
    if locale == "de" { DE } else { EN }
}

/// `"de"` or `"en"` from the environment, else macOS's first preferred language.
pub fn system_locale() -> String {
    detect_locale(
        |name| std::env::var(name).ok(),
        cfg!(target_os = "macos"),
        apple_languages,
    )
}

/// [`system_locale`] with injected environment and `defaults` lookup.
pub fn detect_locale(
    env: impl Fn(&str) -> Option<String>,
    mac: bool,
    apple_languages: impl FnOnce() -> Option<String>,
) -> String {
    // `LC_ALL || LC_MESSAGES || LANG`: empty values fall through.
    let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| env(name).filter(|v| !v.is_empty()));
    let mut lang = env.filter(|v| !is_c_locale(v)).unwrap_or_default();
    if lang.is_empty() && mac {
        lang = apple_languages()
            .and_then(|out| first_language(&out))
            .unwrap_or_default();
    }
    if lang.to_lowercase().starts_with("de") {
        "de".into()
    } else {
        "en".into()
    }
}

/// `/^(C|POSIX)\b/`
fn is_c_locale(v: &str) -> bool {
    let word_end = |rest: &str| {
        rest.chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    };
    v.strip_prefix("POSIX").is_some_and(word_end) || v.strip_prefix('C').is_some_and(word_end)
}

/// First `/[A-Za-z]{2,3}/` match, i.e. the first run of at least two ASCII letters.
fn first_language(out: &str) -> Option<String> {
    let bytes = out.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i - start >= 2 {
                return Some(out[start..start + (i - start).min(3)].to_owned());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// `defaults read -g AppleLanguages` with a 2 s limit.
fn apple_languages() -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)], mac: bool, defaults: Option<&str>) -> String {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        detect_locale(
            |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            mac,
            || defaults.map(str::to_owned),
        )
    }

    #[test]
    fn environment_wins() {
        assert_eq!(detect(&[("LANG", "de_DE.UTF-8")], true, Some("(en)")), "de");
        assert_eq!(
            detect(
                &[("LC_ALL", "en_US.UTF-8"), ("LANG", "de_DE.UTF-8")],
                true,
                None
            ),
            "en"
        );
        assert_eq!(
            detect(
                &[("LC_ALL", ""), ("LC_MESSAGES", "de_AT"), ("LANG", "en_US")],
                false,
                None
            ),
            "de"
        );
    }

    #[test]
    fn c_locale_falls_back_to_apple_languages() {
        let out = "(\n    \"de-DE\",\n    \"en-US\"\n)\n";
        assert_eq!(detect(&[("LANG", "C.UTF-8")], true, Some(out)), "de");
        assert_eq!(detect(&[("LANG", "POSIX")], true, Some(out)), "de");
        assert_eq!(detect(&[("LANG", "C")], false, Some(out)), "en");
        assert_eq!(detect(&[], true, Some("(\n    \"en-DE\"\n)")), "en");
        assert_eq!(detect(&[], true, None), "en");
        // "CA" is not the C locale.
        assert_eq!(detect(&[("LANG", "CA")], true, Some(out)), "en");
    }

    #[test]
    fn strings_fall_back_to_english() {
        assert_eq!(strings("de").banner, "DeepSeek verwendet deinen Computer");
        assert_eq!(strings("fr"), EN);
    }
}
