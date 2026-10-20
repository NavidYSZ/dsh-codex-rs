//! Cross-process write ownership of one session directory, compatible with
//! dsh's `lease.ts`: a non-blocking `flock(2)` on `session.lock` beside the
//! log, verified against the path's current inode. The kernel releases it
//! when the descriptor closes, including on process death.
use std::fs::File;
use std::path::Path;

use crate::error::{IoContext, Result, SessionError};
use crate::format::LOCK_FILENAME;

/// A held session write lock; dropping it releases the lock (the file stays,
/// like dsh, so later lockers see a stable inode).
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

/// `mkdir -p` with owner-only permissions on every created directory.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).at(dir)
}

impl SessionLock {
    /// Takes the lock for `dir` (created if absent) or fails with [`SessionError::Locked`].
    pub fn acquire(dir: &Path, id: &str) -> Result<SessionLock> {
        create_private_dir(dir)?;
        let path = dir.join(LOCK_FILENAME);
        // Bounded retry: a lock file unlinked and recreated by someone else
        // carries a fresh inode, and a lock on the orphan proves nothing.
        for _ in 0..3 {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&path).at(&path)?;
            #[cfg(unix)]
            {
                use rustix::fs::{FlockOperation, flock};
                use std::os::unix::fs::MetadataExt;
                match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                    Ok(()) => {}
                    Err(e)
                        if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN =>
                    {
                        return Err(SessionError::Locked(id.to_owned()));
                    }
                    Err(e) => return Err(SessionError::io(&path, e.into())),
                }
                let held = file.metadata().at(&path)?;
                match std::fs::metadata(&path) {
                    Ok(current) if current.ino() == held.ino() && current.dev() == held.dev() => {
                        return Ok(SessionLock { _file: file });
                    }
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(SessionError::io(&path, e)),
                }
            }
            #[cfg(not(unix))]
            return Ok(SessionLock { _file: file });
        }
        Err(SessionError::Locked(id.to_owned()))
    }

    /// Whether some descriptor currently holds the lock of `dir`. Probes with
    /// a momentary shared lock, so call it only when it matters (open turns).
    pub fn is_held(dir: &Path) -> bool {
        #[cfg(unix)]
        {
            use rustix::fs::{FlockOperation, flock};
            let Ok(file) = File::open(dir.join(LOCK_FILENAME)) else {
                return false;
            };
            matches!(flock(&file, FlockOperation::NonBlockingLockShared), Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN)
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_until_release() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        let lock = SessionLock::acquire(&dir, "s").unwrap();
        assert!(SessionLock::is_held(&dir));
        assert!(matches!(
            SessionLock::acquire(&dir, "s"),
            Err(SessionError::Locked(_))
        ));
        drop(lock);
        assert!(!SessionLock::is_held(&dir));
        let _again = SessionLock::acquire(&dir, "s").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
