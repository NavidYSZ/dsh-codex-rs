//! Complete results that do not fit inline (dsh `spill-local`): one private
//! root per process under the temp dir, one directory per session, one file
//! per saved result. The model gets the path and reads it with `read`/`grep`.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::util::random_hex;

/// Guidance appended to every spill notice (dsh `retrievalHint`).
pub const RETRIEVAL_HINT: &str =
    "Use read with offset/limit, or grep this path to search within it.";

/// A saved complete result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRef {
    /// Absolute path of the saved file.
    pub locator: String,
    pub bytes: u64,
    pub retrieval_hint: &'static str,
}

/// Where complete results are saved. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct SpillStore {
    root: Option<PathBuf>,
}

fn private_root() -> std::io::Result<&'static Path> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    if let Some(root) = ROOT.get() {
        return Ok(root);
    }
    let dir = tempfile::Builder::new()
        .prefix("dsh-spill-")
        .tempdir()?
        .keep();
    Ok(ROOT.get_or_init(|| dir))
}

/// dsh `encodeSegment`: keep `[A-Za-z0-9._-]`, escape the rest as `~XXXX`.
fn encode_segment(raw: &str) -> String {
    match raw {
        "" => return "~".into(),
        "." => return "~002E".into(),
        ".." => return "~002E~002E".into(),
        _ => {}
    }
    let mut out = String::new();
    for unit in raw.encode_utf16() {
        let ch = char::from_u32(u32::from(unit))
            .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c));
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&format!("~{unit:04X}")),
        }
    }
    out
}

impl SpillStore {
    /// The default store: a private `dsh-spill-*` directory (dsh's layout) in the temp dir.
    pub fn new() -> Self {
        SpillStore { root: None }
    }

    /// A store rooted at `root` (tests, deployments with their own location).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        SpillStore {
            root: Some(root.into()),
        }
    }

    fn root(&self) -> std::io::Result<PathBuf> {
        match &self.root {
            Some(root) => Ok(root.clone()),
            None => private_root().map(Path::to_path_buf),
        }
    }

    /// Saves `content` for `session_id` under a name derived from `suggested_name`.
    pub fn save_text(
        &self,
        session_id: &str,
        suggested_name: &str,
        content: &str,
    ) -> std::io::Result<SpillRef> {
        let hash = Sha256::digest(session_id.as_bytes());
        let hex: String = hash.iter().take(6).map(|b| format!("{b:02x}")).collect();
        let dir = self.root()?.join(format!("session-{hex}"));
        create_private_dir(&dir)?;
        let path = dir.join(format!(
            "{}-{}",
            random_hex(12),
            encode_segment(suggested_name)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(content.as_bytes())?;
        Ok(SpillRef {
            locator: path.to_string_lossy().into_owned(),
            bytes: content.len() as u64,
            retrieval_hint: RETRIEVAL_HINT,
        })
    }
}

/// dsh's inline budget for tool results (`spill-policy` `maxInlineTokens`).
pub const DEFAULT_MAX_INLINE_TOKENS: usize = 12_500;

/// dsh's rough token estimate of one text block (`estimateContent`).
fn price(text: &[u16]) -> usize {
    text.len().div_ceil(4) + 4
}

/// dsh `textSlice`: `length` UTF-16 units from the start (or end) without
/// splitting a surrogate pair.
fn text_slice(text: &[u16], length: usize, tail: bool) -> &[u16] {
    let mut cut = if tail { text.len() - length } else { length };
    if cut > 0 && cut < text.len() {
        let previous = text[cut - 1];
        let current = text[cut];
        if (0xd800..=0xdbff).contains(&previous) && (0xdc00..=0xdfff).contains(&current) {
            if tail {
                cut += 1;
            } else {
                cut -= 1;
            }
        }
    }
    if tail { &text[cut..] } else { &text[..cut] }
}

/// dsh `fitText`: the longest end of `text` within `budget` tokens.
fn fit_text(text: &[u16], budget: usize, tail: bool) -> &[u16] {
    let (mut low, mut high) = (0usize, text.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate = text_slice(text, middle, tail);
        if candidate.is_empty() || price(candidate) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    text_slice(text, low, tail)
}

/// The agent-side `spill-policy` of dsh for a text-only tool result: when
/// `text` costs more than `max_tokens` (estimated like dsh, 4 chars per token),
/// the complete text is saved as `<tool>.txt` and the model gets its head and
/// tail around `[...]` with a notice naming the saved file. `None` when it
/// fits (or cannot be saved: dsh then keeps the inline content). dsh skips
/// this for `read` results.
pub fn bound_inline_text(
    store: &SpillStore,
    session_id: &str,
    tool: &str,
    text: &str,
    max_tokens: usize,
) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if price(&units) <= max_tokens {
        return None;
    }
    let saved = match store.save_text(session_id, &format!("{tool}.txt"), text) {
        Ok(saved) => saved,
        Err(error) => {
            tracing::warn!("spill-policy: {error}; keeping the inline content");
            return None;
        }
    };
    let notice = |bytes: usize| {
        format!(
            "(Omitted {bytes} bytes. Full formatted result stored at: {}. {})",
            saved.locator, saved.retrieval_hint
        )
    };
    let gap: Vec<u16> = "\n\n[...]\n\n".encode_utf16().collect();
    let worst = notice(text.len());
    let worst_units: Vec<u16> = worst.encode_utf16().collect();
    let framed: Vec<u16> = format!("\n\n{worst}").encode_utf16().collect();
    if price(&worst_units) > max_tokens {
        tracing::warn!(
            "spill-policy: spill notice for {tool} exceeds maxInlineTokens; keeping the inline content"
        );
        return None;
    }
    let budget = max_tokens.saturating_sub(price(&gap) + price(&framed));
    let head = fit_text(&units, budget.div_ceil(2), false);
    let rest = &units[head.len()..];
    let tail = fit_text(rest, budget / 2, true);
    let head = String::from_utf16_lossy(head);
    let tail = String::from_utf16_lossy(tail);
    let footer = notice(text.len() - head.len() - tail.len());
    if head.is_empty() && tail.is_empty() {
        return Some(footer);
    }
    Some(format!("{head}\n\n[...]\n\n{tail}\n\n{footer}"))
}

pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_under_a_hashed_session_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SpillStore::at(tmp.path());
        let saved = store.save_text("s1", "glob-results.txt", "a\nb").unwrap();
        assert!(saved.locator.ends_with("-glob-results.txt"));
        assert!(saved.locator.contains("/session-"));
        assert_eq!(std::fs::read_to_string(&saved.locator).unwrap(), "a\nb");
        assert_eq!(saved.bytes, 3);
        assert_eq!(encode_segment("a b"), "a~0020b");
    }

    /// A temp dir with a short path: `/tmp` where it exists (macOS's
    /// `$TMPDIR` under `/var/folders/…` alone is longer than dsh's root).
    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .tempdir_in("/tmp")
            .or_else(|_| tempfile::tempdir())
            .unwrap()
    }

    /// Bounds `text` at 200 tokens and returns (head, tail, notice).
    fn bound_200(store: &SpillStore, text: &str) -> (String, String, String) {
        let bounded = bound_inline_text(store, "s", "bash", text, 200).unwrap();
        let (kept, notice) = bounded.rsplit_once("\n\n(").unwrap();
        let (head, tail) = kept.split_once("\n\n[...]\n\n").unwrap();
        assert!(head.starts_with("SPILL_START-x"));
        assert!(tail.ends_with("x-SPILL_END"));
        assert!(notice.ends_with(
            "-bash.txt. Use read with offset/limit, or grep this path to search within it.)"
        ));
        (head.into(), tail.into(), notice.into())
    }

    #[test]
    fn bounds_large_results_like_dsh_spill_policy() {
        let tmp = short_tempdir();
        let text = format!("SPILL_START-{}-SPILL_END", "x".repeat(2000));
        // The notice's path length is part of the budget: use a root as long
        // as the snapshot's `/tmp/dsh-acp-snap-ee77dff02`.
        const SNAPSHOT_ROOT: usize = "/tmp/dsh-acp-snap-ee77dff02".len();
        let Some(pad) = SNAPSHOT_ROOT.checked_sub(tmp.path().to_string_lossy().len() + 1) else {
            eprintln!("skipping the dsh byte counts: no temp dir shorter than {SNAPSHOT_ROOT}");
            return;
        };
        let store = SpillStore::at(tmp.path().join("x".repeat(pad)));
        assert_eq!(bound_inline_text(&store, "s", "bash", "small", 100), None);
        let (head, tail, notice) = bound_200(&store, &text);
        // dsh snapshot bash-spill: 2022 bytes at 200 tokens keep 528, omit 1494.
        assert_eq!(head.len() + tail.len(), text.len() - 1494, "{notice}");
        assert!(notice.starts_with("Omitted 1494 bytes. Full formatted result stored at: "));
    }

    #[test]
    fn long_roots_shrink_the_kept_text_without_overflow() {
        // Real roots are long on macOS (`/var/folders/…/T/dsh-spill-…`).
        let tmp = tempfile::tempdir().unwrap();
        let store = SpillStore::at(tmp.path().join("y".repeat(120)));
        let text = format!("SPILL_START-{}-SPILL_END", "x".repeat(2000));
        let (head, tail, notice) = bound_200(&store, &text);
        let omitted = text.len() - head.len() - tail.len();
        assert!(notice.starts_with(&format!("Omitted {omitted} bytes.")));
        assert!(
            omitted > 1494,
            "a longer notice leaves less room: {omitted}"
        );
        // A notice that alone exceeds the budget keeps the inline content.
        let deep: PathBuf = std::iter::repeat_n("z".repeat(200), 4).collect();
        let deep = SpillStore::at(tmp.path().join(deep));
        assert_eq!(bound_inline_text(&deep, "s", "bash", &text, 200), None);
        assert!(
            std::fs::read_dir(tmp.path().join("z".repeat(200))).is_ok(),
            "it was saved, then kept inline"
        );
    }
}
