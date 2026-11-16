use super::{artifacts, inventory};
use anyhow::{bail, Context, Result};
use std::fs::Metadata;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Identity {
    pub device: u64,
    inode: u64,
}

#[cfg(unix)]
pub fn identity(_: &Path, metadata: &Metadata) -> Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
pub fn identity(path: &Path, _: &Metadata) -> Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let handle = open_windows(path, true)?;
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The handle stays open and the API initializes the struct on success.
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let info = unsafe { info.assume_init() };
    Ok(Identity {
        device: u64::from(info.dwVolumeSerialNumber),
        inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(unix)]
pub fn changed(metadata: &Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.ctime(), metadata.ctime_nsec())
}

#[cfg(windows)]
pub fn changed(metadata: &Metadata) -> (i64, i64) {
    use std::os::windows::fs::MetadataExt;
    (
        metadata.creation_time() as i64,
        i64::from(metadata.file_attributes()),
    )
}

#[cfg(target_os = "macos")]
pub fn ensure_not_open(path: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-t", "+D"])
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if !output.stdout.is_empty() {
        bail!("A process has files open in this output");
    }
    if output.status.code() != Some(1) || !output.stderr.is_empty() {
        bail!("Could not verify open files with lsof");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_not_open(_: &Path) -> Result<()> {
    // Windows additionally enforces file sharing modes during move and removal.
    Ok(())
}

#[cfg(unix)]
pub fn available(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(windows)]
pub fn available(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut bytes = 0;
    // The path is NUL terminated and the output pointer is writable.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(bytes)
}

#[cfg(unix)]
pub fn remove_output(path: &Path, tree: &artifacts::Tree) -> Result<()> {
    use rustix::fs::{self, AtFlags, Mode, OFlags};
    use std::io::Write;
    use std::path::Component;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent_path = path.parent().context("Missing output parent")?;
    let name = path.file_name().context("Missing output name")?;
    let mut parent = fs::open("/", flags, Mode::empty())?;
    for component in parent_path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => parent = fs::openat(&parent, name, flags, Mode::empty())?,
            _ => bail!("Unexpected path component in output parent"),
        }
    }
    let parent_info = fs::fstat(&parent)?;
    let parent_id = stat_identity(&parent_info);
    if parent_id != tree.parent {
        bail!("Output parent changed after review");
    }
    let original = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if stat_identity(&original) != tree.entries[Path::new("")].identity {
        bail!("Output was replaced after review");
    }

    let stage_name = format!(".still-stage-{}", uuid::Uuid::new_v4());
    let stage_path = parent_path.join(&stage_name);
    fs::mkdirat(&parent, stage_name.as_str(), Mode::RWXU)?;
    let stage = fs::openat(&parent, stage_name.as_str(), flags, Mode::empty())?;
    let note = fs::openat(
        &stage,
        "original-path.txt",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut note = std::fs::File::from(note);
    writeln!(note, "{}", inventory::display(path))?;
    note.sync_all()?;

    // Keep the payload recoverable if the process is interrupted or a check fails.
    fs::renameat(&parent, name, &stage, "payload")?;
    let attempt = (|| {
        let info = fs::statat(&stage, "payload", AtFlags::SYMLINK_NOFOLLOW)?;
        if stat_identity(&info) != tree.entries[Path::new("")].identity {
            bail!("A different directory reached staging");
        }
        artifacts::verify_staged(tree, &stage_path.join("payload"))?;
        remove_tree(
            &stage,
            std::ffi::OsStr::new("payload"),
            Path::new(""),
            tree,
            flags,
            &mut std::collections::HashSet::new(),
        )?;
        Ok::<(), anyhow::Error>(())
    })();
    attempt.with_context(|| {
        format!(
            "Staged data may remain at {}",
            inventory::display(&stage_path)
        )
    })?;
    fs::unlinkat(&stage, "original-path.txt", AtFlags::empty())?;
    fs::unlinkat(&parent, stage_name.as_str(), AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // Stat field widths differ across supported Unix targets.
fn stat_identity(stat: &rustix::fs::Stat) -> Identity {
    Identity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    }
}

#[cfg(unix)]
fn remove_tree(
    parent: &rustix::fd::OwnedFd,
    name: &std::ffi::OsStr,
    relative: &Path,
    tree: &artifacts::Tree,
    flags: rustix::fs::OFlags,
    unlinked: &mut std::collections::HashSet<Identity>,
) -> Result<()> {
    use rustix::fs::{self, AtFlags, FileType, Mode};
    use std::os::unix::ffi::OsStrExt;

    let expected = tree
        .entries
        .get(relative)
        .context("An unreviewed entry appeared in staging")?;
    let stat = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if stat_identity(&stat) != expected.identity {
        bail!("Staged entry changed: {relative:?}");
    }
    let kind = FileType::from_raw_mode(stat.st_mode);
    if expected.is_dir && kind == FileType::Directory {
        let directory = fs::openat(parent, name, flags, Mode::empty())?;
        if stat_identity(&fs::fstat(&directory)?) != expected.identity {
            bail!("Staged directory was replaced: {relative:?}");
        }
        let mut children = Vec::new();
        for entry in fs::Dir::read_from(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                children.push(std::ffi::OsStr::from_bytes(name).to_owned());
            }
        }
        for child in children {
            remove_tree(
                &directory,
                &child,
                &relative.join(&child),
                tree,
                flags,
                unlinked,
            )?;
        }
        fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    } else if !expected.is_dir && kind == FileType::RegularFile {
        let modified = std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(
            stat.st_mtime as u64,
            stat.st_mtime_nsec as u32,
        ));
        // Unlinking one name changes ctime for the other names of the same inode.
        let changed = (stat.st_ctime as i64, stat.st_ctime_nsec as i64);
        if stat.st_size as u64 != expected.len
            || modified != Some(expected.modified)
            || (changed != expected.changed && !unlinked.contains(&expected.identity))
        {
            bail!("Staged file was modified: {relative:?}");
        }
        fs::unlinkat(parent, name, AtFlags::empty())?;
        if stat.st_nlink > 1 {
            unlinked.insert(expected.identity);
        }
    } else {
        bail!("Unsupported staged entry: {relative:?}");
    }
    Ok(())
}

#[cfg(windows)]
fn open_windows(path: &Path, allow_move: bool) -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let handle = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(
            FILE_SHARE_READ | FILE_SHARE_WRITE | if allow_move { FILE_SHARE_DELETE } else { 0 },
        )
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if inventory::is_link(&handle.metadata()?) {
        bail!("Reparse point: {path:?}");
    }
    Ok(handle)
}

#[cfg(windows)]
pub fn remove_output(path: &Path, tree: &artifacts::Tree) -> Result<()> {
    use std::fs;
    use std::io::Write;
    let parent = path.parent().context("Missing output parent")?;
    // Keep every ancestor open without delete sharing during the operation.
    let _ancestors = parent
        .ancestors()
        .map(|p| open_windows(p, false))
        .collect::<Result<Vec<_>>>()?;
    if identity(parent, &fs::symlink_metadata(parent)?)? != tree.parent {
        bail!("Output parent changed after review");
    }
    if &artifacts::stamp_tree(path)? != tree {
        bail!("Output changed after review");
    }
    let stage = parent.join(format!(".still-stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let stage_guard = open_windows(&stage, false)?;
    let note_path = stage.join("original-path.txt");
    let mut note = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&note_path)?;
    writeln!(note, "{}", inventory::display(path))?;
    note.sync_all()?;
    drop(note);
    let payload = stage.join("payload");
    fs::rename(path, &payload)?;
    artifacts::verify_staged(tree, &payload)
        .and_then(|()| Ok(fs::remove_dir_all(&payload)?))
        .with_context(|| format!("Staged data may remain at {}", inventory::display(&stage)))?;
    fs::remove_file(note_path)?;
    drop(stage_guard);
    fs::remove_dir(stage)?;
    Ok(())
}
