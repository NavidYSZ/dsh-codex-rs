//! One `dsh-codex web` per data directory: a non-blocking exclusive `flock`
//! on `$DSHX_HOME/run/web.lock`, held until the process exits (the kernel
//! releases it on any exit, crashes included). A second server on the same
//! data directory would hold chat locks and drive Goals of its own next to
//! the first one, so it refuses to start and names the running PID.
use std::path::Path;
use std::sync::OnceLock;

/// The held lock file; never dropped before exit.
static HELD: OnceLock<std::fs::File> = OnceLock::new();

/// Takes the lock of `home` for the rest of this process.
#[cfg(unix)]
pub fn acquire(home: &Path) -> anyhow::Result<()> {
    use std::io::{Read as _, Seek as _, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    let dir = home.join("run");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    let path = dir.join("web.lock");
    // No truncation before the lock is ours: a refused start must not wipe
    // the PID of the server that holds it.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN => {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            let holder = match text.trim().parse::<u32>() {
                Ok(pid) => format!("another dsh-codex web server (PID {pid})"),
                Err(_) => "another dsh-codex web server".to_owned(),
            };
            anyhow::bail!(
                "{holder} already uses the data directory {}. Stop it first, or give this server its own data directory with DSHX_HOME.",
                home.display()
            );
        }
        Err(e) => anyhow::bail!("locking {}: {e}", path.display()),
    }
    file.set_len(0)?;
    file.rewind()?;
    writeln!(file, "{}", std::process::id())?;
    let _ = HELD.set(file);
    Ok(())
}

#[cfg(not(unix))]
pub fn acquire(_home: &Path) -> anyhow::Result<()> {
    let _ = &HELD;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn a_second_server_is_refused_and_named() {
        let home = tempfile::tempdir().unwrap();
        super::acquire(home.path()).unwrap();
        // A second descriptor conflicts like a second process would.
        let error = super::acquire(home.path()).unwrap_err().to_string();
        let pid = std::process::id();
        assert!(error.contains(&format!("(PID {pid})")), "{error}");
        let text = std::fs::read_to_string(home.path().join("run/web.lock")).unwrap();
        assert_eq!(text.trim(), pid.to_string(), "the holder's PID survives");
    }
}
