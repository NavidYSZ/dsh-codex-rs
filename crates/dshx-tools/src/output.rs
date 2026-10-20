//! Bounded capture of one process stream (dsh `subprocess-local/output.ts`):
//! an in-memory tail of `max_bytes`, and on first overflow a private spill
//! file that receives the complete stream up to `max_spill_bytes`.
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::spill::create_private_dir;
use crate::util::random_hex;

/// Per-stream in-memory tail (dsh bash-local `maxOutputBytes`).
pub const MAX_OUTPUT_BYTES: usize = 64_000;
/// Per-stream spill cap (dsh bash-local `maxSpillBytes`).
pub const MAX_SPILL_BYTES: u64 = 64 * 1024 * 1024;

fn spill_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("dsh-subprocess-")
            .tempdir()
            .ok()?
            .keep();
        let _ = create_private_dir(&dir);
        Some(dir)
    })
    .as_deref()
}

static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Final shape of a captured stream (dsh `CollectedOutput`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Collected {
    pub text: String,
    pub truncated: bool,
    pub spill_path: Option<String>,
}

/// One incremental read in whole-stream byte coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamRead {
    pub text: String,
    pub next_offset: u64,
    /// Bytes before the read's start were dropped from memory unread.
    pub lossy: bool,
    pub spill_path: Option<String>,
}

#[derive(Debug)]
pub struct OutputCollector {
    label: &'static str,
    max_bytes: usize,
    max_spill_bytes: Option<u64>,
    tail: Vec<u8>,
    total: u64,
    dropped: bool,
    spill: Option<(File, PathBuf)>,
    spill_disabled: bool,
}

impl OutputCollector {
    /// `max_spill_bytes: None` keeps only the tail (no spill file).
    pub fn new(label: &'static str, max_bytes: usize, max_spill_bytes: Option<u64>) -> Self {
        OutputCollector {
            label,
            max_bytes,
            max_spill_bytes,
            tail: Vec::new(),
            total: 0,
            dropped: false,
            spill: None,
            spill_disabled: max_spill_bytes.is_none(),
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let overflows = self.tail.len() + chunk.len() > self.max_bytes;
        if !self.spill_disabled && (overflows || self.spill.is_some()) {
            self.spill_chunk(chunk);
        }
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > self.max_bytes {
            let excess = self.tail.len() - self.max_bytes;
            self.tail.drain(..excess);
            self.dropped = true;
        }
    }

    fn spill_chunk(&mut self, chunk: &[u8]) {
        if self.max_spill_bytes.is_some_and(|max| self.total > max) {
            self.discard_spill();
            return;
        }
        let result = (|| -> std::io::Result<()> {
            if self.spill.is_none() {
                let dir = spill_dir().ok_or_else(|| std::io::Error::other("no spill directory"))?;
                let path = dir.join(format!(
                    "dsh-subprocess-{}-{}-{}-{}.log",
                    std::process::id(),
                    SPILL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1,
                    random_hex(12),
                    self.label
                ));
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path)?;
                file.write_all(&self.tail)?;
                self.spill = Some((file, path));
            }
            if let Some((file, _)) = self.spill.as_mut() {
                file.write_all(chunk)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::error!(
                "could not write the complete {} stream to its spill file; only the in-memory tail is kept: {error}",
                self.label
            );
            self.discard_spill();
        }
    }

    fn discard_spill(&mut self) {
        self.spill_disabled = true;
        if let Some((file, path)) = self.spill.take() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
    }

    fn spill_path(&self) -> Option<String> {
        self.spill
            .as_ref()
            .map(|(_, path)| path.to_string_lossy().into_owned())
    }

    /// Everything pushed since `from` (lossy when `from` slid out of the tail).
    pub fn read_from(&self, from: u64) -> StreamRead {
        let window_start = self.total - self.tail.len() as u64;
        let lossy = from < window_start;
        let slice = if lossy {
            &self.tail[..]
        } else {
            let skip = (from - window_start).min(self.tail.len() as u64) as usize;
            &self.tail[skip..]
        };
        StreamRead {
            text: String::from_utf8_lossy(slice).into_owned(),
            next_offset: self.total,
            lossy,
            spill_path: self.spill_path(),
        }
    }

    /// Closes the spill file (the stream ended).
    pub fn seal(&mut self) {
        if let Some((file, _)) = self.spill.as_mut()
            && file.flush().is_err()
        {
            self.spill = None;
        }
    }

    pub fn finalize(&self) -> Collected {
        Collected {
            text: String::from_utf8_lossy(&self.tail).into_owned(),
            truncated: self.dropped,
            spill_path: self.spill_path(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_tail_and_spills_complete_stream() {
        let mut c = OutputCollector::new("stdout", 10, Some(1_000));
        c.push(b"0123456789");
        assert!(!c.finalize().truncated);
        c.push(b"abcdef");
        let out = c.finalize();
        assert_eq!(out.text, "6789abcdef");
        assert!(out.truncated);
        let path = out.spill_path.expect("spill file");
        c.seal();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0123456789abcdef");
        let read = c.read_from(0);
        assert!(read.lossy);
        assert_eq!(c.read_from(12).text, "cdef");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn oversized_stream_discards_its_spill() {
        let mut c = OutputCollector::new("stderr", 4, Some(8));
        c.push(b"12345");
        let path = c.finalize().spill_path.unwrap();
        c.push(b"6789");
        assert_eq!(c.finalize().spill_path, None);
        assert!(!Path::new(&path).exists());
    }
}
