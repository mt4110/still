//! The open file holds the OS lock. Keeping the file across launches avoids an
//! unlink/reopen race and the OS releases the lock if the desktop process exits.
use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::path::Path;

pub(super) fn acquire() -> Result<Option<File>> {
    let dirs = crate::storage_paths::project_dirs().context("Could not locate still data")?;
    let directory = dirs.data_local_dir();
    fs::create_dir_all(directory)?;
    lock(&directory.join("desktop.lock"))
}

fn lock(path: &Path) -> Result<Option<File>> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .context("Could not open the desktop lock file")?;
    anyhow::ensure!(file.metadata()?.is_file(), "Invalid desktop lock file");
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(err) => Err(err).context("Could not acquire the desktop lock"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lock_lives_until_the_first_handle_closes() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("desktop.lock");
        let first = super::lock(&path)?.unwrap();
        assert!(super::lock(&path)?.is_none());
        drop(first);
        assert!(super::lock(&path)?.is_some());
        Ok(())
    }
}
