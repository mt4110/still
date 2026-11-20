//! Finder can write directory metadata after the reviewed entries have been
//! enumerated. Only accept recognizable regular files inside the pinned stage.
use anyhow::{ensure, Context, Result};
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(super) const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn header(name: &OsStr) -> Option<&'static [u8]> {
    let name = name.as_bytes();
    if name == b".DS_Store" {
        Some(b"\0\0\0\x01Bud1")
    } else if name.starts_with(b"._") && name.len() > 2 {
        Some(b"\0\x05\x16\x07\0\x02\0\0")
    } else {
        None
    }
}

pub(super) fn children(directory: &OwnedFd) -> Result<Vec<OsString>> {
    Dir::read_from(directory)?
        .filter_map(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => return Some(Err(err.into())),
            };
            let name = entry.file_name().to_bytes();
            (name != b"." && name != b"..").then(|| Ok(OsStr::from_bytes(name).to_owned()))
        })
        .collect()
}

pub(super) fn open_regular(parent: &OwnedFd, name: &OsStr, max_size: u64) -> Result<std::fs::File> {
    let stat = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    ensure!(
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile && stat.st_nlink == 1,
        "Expected an ordinary single-link file: {name:?}"
    );
    let fd = fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let opened = fs::fstat(&fd)?;
    ensure!(
        super::stat_identity(&opened) == super::stat_identity(&stat)
            && opened.st_size >= 0
            && opened.st_size as u64 <= max_size
            && opened.st_dev == fs::fstat(parent)?.st_dev,
        "Unexpected file in stage: {name:?}"
    );
    Ok(std::fs::File::from(fd))
}

pub(super) fn inspect(parent: &OwnedFd, name: &OsStr) -> Result<fs::Stat> {
    let expected = header(name).context("Unreviewed file remains in staging")?;
    let mut file = open_regular(parent, name, 16 * 1024 * 1024)?;
    let mut actual = [0; 8];
    file.read_exact(&mut actual)?;
    ensure!(actual == expected, "Unrecognized Finder metadata: {name:?}");
    Ok(fs::fstat(&file)?)
}

pub(crate) fn recognized(path: &Path) -> bool {
    let check = || -> Result<()> {
        let name = path.file_name().context("Missing metadata name")?;
        ensure!(header(name).is_some(), "Unrecognized name");
        let parent = fs::open(
            path.parent().context("Missing parent")?,
            DIRECTORY,
            Mode::empty(),
        )?;
        inspect(&parent, name)?;
        Ok(())
    };
    check().is_ok()
}

pub(super) fn unlink_checked(parent: &OwnedFd, name: &OsStr, expected: &fs::Stat) -> Result<()> {
    let stat = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    ensure!(
        super::stat_identity(&stat) == super::stat_identity(expected)
            && stat.st_nlink == 1
            && stat.st_ctime == expected.st_ctime
            && stat.st_ctime_nsec == expected.st_ctime_nsec,
        "Staged file changed: {name:?}"
    );
    fs::unlinkat(parent, name, AtFlags::empty())?;
    Ok(())
}

pub(super) fn remove_metadata(parent: &OwnedFd, name: &OsStr) -> Result<()> {
    let stat = inspect(parent, name)?;
    unlink_checked(parent, name, &stat)
}

pub(super) fn finish(parent: &OwnedFd, name: &OsStr, directory: &OwnedFd) -> Result<()> {
    let same_directory = || -> Result<()> {
        ensure!(
            super::stat_identity(&fs::fstat(directory)?)
                == super::stat_identity(&fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?),
            "Staged directory changed: {name:?}"
        );
        Ok(())
    };
    same_directory()?;
    match fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::NOTEMPTY) => {
            // One bounded retry. Inspect every remaining name before touching any.
            let entries = children(directory)?
                .into_iter()
                .map(|name| Ok((inspect(directory, &name)?, name)))
                .collect::<Result<Vec<_>>>()?;
            for (stat, child) in entries {
                unlink_checked(directory, &child, &stat)?;
            }
            same_directory()?;
            Ok(fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?)
        }
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;

    #[test]
    fn late_metadata_retries_directory_removal_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        stdfs::create_dir(root.join("directory"))?;
        let parent = fs::open(root, DIRECTORY, Mode::empty())?;
        let name = OsStr::new("directory");
        let directory = fs::openat(&parent, name, DIRECTORY, Mode::empty())?;
        stdfs::write(root.join("directory/.DS_Store"), b"\0\0\0\x01Bud1metadata")?;
        stdfs::write(
            root.join("directory/._late"),
            b"\0\x05\x16\x07\0\x02\0\0metadata",
        )?;
        finish(&parent, name, &directory)?;
        assert!(!root.join("directory").exists());
        Ok(())
    }

    #[test]
    fn retry_stops_on_any_non_metadata_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        stdfs::create_dir(root.join("directory"))?;
        let parent = fs::open(root, DIRECTORY, Mode::empty())?;
        let name = OsStr::new("directory");
        let directory = fs::openat(&parent, name, DIRECTORY, Mode::empty())?;
        stdfs::write(root.join("directory/.DS_Store"), b"\0\0\0\x01Bud1metadata")?;
        stdfs::write(root.join("directory/keep.txt"), b"keep")?;
        assert!(finish(&parent, name, &directory).is_err());
        assert!(root.join("directory/.DS_Store").is_file());
        assert!(root.join("directory/keep.txt").is_file());
        Ok(())
    }
}
