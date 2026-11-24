#[cfg(target_os = "macos")]
pub(super) mod finder;
#[cfg(target_os = "macos")]
pub(super) mod recovery;

use super::{artifacts, inventory};
use anyhow::{bail, Context, Result};
use std::fs::Metadata;
use std::path::Path;

// Release explicitly so a child process forked by another thread cannot
// keep a short-lived lock held until that child finishes exec.
pub(super) struct FileLock(std::fs::File);

impl FileLock {
    pub(super) fn try_acquire(file: std::fs::File) -> Result<Self, std::fs::TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self.0
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

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
    windows_identity(&open_windows(path, true, false)?)
}

#[cfg(windows)]
fn windows_identity(handle: &std::fs::File) -> Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

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

pub fn stamp(
    path: &Path,
    metadata: &Metadata,
    fixture: Option<[u8; 32]>,
) -> Result<artifacts::Stamp> {
    #[cfg(unix)]
    let (identity, changed, contents) = {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::OpenOptionsExt;
        let contents = if metadata.is_file() {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            let before = file.metadata()?;
            if !before.is_file()
                || identity(path, &before)? != identity(path, metadata)?
                || before.len() != metadata.len()
                || before.modified()? != metadata.modified()?
                || (before.ctime(), before.ctime_nsec())
                    != (metadata.ctime(), metadata.ctime_nsec())
            {
                bail!("File changed during inspection: {path:?}");
            }
            let hash = contents_hash(&mut file, before.len(), fixture)?;
            let after = file.metadata()?;
            if after.len() != before.len()
                || after.modified()? != before.modified()?
                || (after.ctime(), after.ctime_nsec()) != (before.ctime(), before.ctime_nsec())
            {
                bail!("File changed during inspection: {path:?}");
            }
            Some(hash)
        } else {
            None
        };
        (
            identity(path, metadata)?,
            (metadata.ctime(), metadata.ctime_nsec()),
            contents,
        )
    };

    #[cfg(windows)]
    let (metadata, identity, changed, contents) = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO,
        };

        let mut handle = open_windows(path, true, metadata.is_file())?;
        let current = handle.metadata()?;
        if current.is_dir() != metadata.is_dir() || current.is_file() != metadata.is_file() {
            bail!("Entry type changed after inspection: {path:?}");
        }
        let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
        // The handle stays open and a successful query initializes the buffer.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                FileBasicInfo,
                basic.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let basic = unsafe { basic.assume_init() };
        // NTFS can report identical timestamps for successive same-size writes.
        // Hash regular files only when reviewing or applying a selection.
        let contents = if current.is_file() {
            Some(contents_hash(&mut handle, current.len(), fixture)?)
        } else {
            None
        };
        (
            current,
            windows_identity(&handle)?,
            (basic.ChangeTime, i64::from(basic.FileAttributes)),
            contents,
        )
    };

    Ok(artifacts::Stamp {
        identity,
        is_dir: metadata.is_dir(),
        len: metadata.len(),
        modified: metadata.modified()?,
        changed,
        contents,
    })
}

fn contents_hash(file: &mut std::fs::File, mut len: u64, fixture: Option<[u8; 32]>) -> Result<u64> {
    use std::hash::{BuildHasher, Hasher, RandomState};
    use std::io::Read;
    use std::sync::LazyLock;

    // Review stamps are compared only within the process that created them.
    static KEY: LazyLock<RandomState> = LazyLock::new(RandomState::new);
    use sha2::{Digest, Sha256};
    let mut published = fixture.map(|_| Sha256::new());
    let mut hash = KEY.build_hasher();
    let mut buffer = [0u8; 64 * 1024];
    while len != 0 {
        let count = len.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..count])?;
        hash.write(&buffer[..count]);
        if let Some(digest) = &mut published {
            digest.update(&buffer[..count]);
        }
        len -= count as u64;
    }
    if let (Some(digest), Some(expected)) = (published, fixture) {
        if digest.finalize().as_slice() != expected {
            bail!("Published fixture changed during review");
        }
    }
    Ok(hash.finish())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn ensure_not_open(path: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let lsof = "/usr/sbin/lsof";
    #[cfg(target_os = "linux")]
    let lsof = "lsof";
    let output = Command::new(lsof)
        .args(["-nP", "-t", "+D"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context(super::message::Message::new(
            "Could not check for open files with lsof",
            "ファイルが使用中か確認できませんでした。",
        ))?;
    if !output.stdout.is_empty() {
        return Err(super::message::Message::new(
            "A process has files open in this output",
            "別のアプリや処理がフォルダ内のファイルを使用中です。終了してから再確認してください。",
        )
        .into());
    }
    if output.status.code() != Some(1) || !output.stderr.is_empty() {
        return Err(super::message::Message::new(
            "Could not verify open files with lsof",
            "ファイルが使用中か確認できませんでした。",
        )
        .into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn ensure_not_open(_: &Path) -> Result<()> {
    // Windows additionally enforces file sharing modes during move and removal.
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn mount_id(path: &Path) -> Result<u64> {
    use rustix::fs::{statx, AtFlags, StatxFlags, CWD};

    let stat = statx(CWD, path, AtFlags::SYMLINK_NOFOLLOW, StatxFlags::MNT_ID)?;
    if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        bail!("Could not verify the Linux mount boundary: {path:?}");
    }
    Ok(stat.stx_mnt_id)
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
    finish_output(path, tree, false)
}

#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) fn trash_output(path: &Path, tree: &artifacts::Tree) -> Result<()> {
    finish_output(path, tree, true)
}

#[cfg(unix)]
fn finish_output(path: &Path, tree: &artifacts::Tree, to_trash: bool) -> Result<()> {
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

    let payload_name = if to_trash
        && !name
            .as_encoded_bytes()
            .eq_ignore_ascii_case(b"original-path.txt")
    {
        name
    } else {
        std::ffi::OsStr::new("payload")
    };

    // Keep the payload recoverable if the process is interrupted or a check fails.
    fs::renameat(&parent, name, &stage, payload_name)?;
    let attempt = (|| {
        let info = fs::statat(&stage, payload_name, AtFlags::SYMLINK_NOFOLLOW)?;
        if stat_identity(&info) != tree.entries[Path::new("")].identity {
            bail!("A different directory reached staging");
        }
        let payload = stage_path.join(payload_name);
        artifacts::verify_staged(tree, &payload)?;
        if to_trash {
            #[cfg(all(feature = "desktop", target_os = "macos"))]
            {
                use trash::macos::{DeleteMethod, TrashContextExtMacos};
                // Verify that the absolute path used by NSFileManager still names
                // the pinned private stage and its reviewed payload.
                use anyhow::ensure;
                ensure!(
                    inventory::safe_dir(&payload)? == payload
                        && identity(&stage_path, &std::fs::symlink_metadata(&stage_path)?)?
                            == stat_identity(&fs::fstat(&stage)?)
                        && identity(&payload, &std::fs::symlink_metadata(&payload)?)?
                            == stat_identity(&info),
                    "Staged Trash source changed"
                );
                let mut trash = trash::TrashContext::new();
                trash.set_delete_method(DeleteMethod::NsFileManager);
                return trash
                    .delete(&payload)
                    .context("Could not move the reviewed folder to Trash");
            }
            #[cfg(not(all(feature = "desktop", target_os = "macos")))]
            bail!("Moving reviewed outputs to Trash is unavailable");
        }
        remove_tree(&stage, payload_name, Path::new(""), tree, flags)?;
        Ok::<(), anyhow::Error>(())
    })();
    attempt.with_context(|| {
        format!(
            "Staged data may remain at {}",
            inventory::display(&stage_path)
        )
    })?;
    fs::unlinkat(&stage, "original-path.txt", AtFlags::empty())?;
    #[cfg(target_os = "macos")]
    finder::finish(&parent, stage_name.as_ref(), &stage).with_context(|| {
        format!(
            "Check staged residue using still stage-cleanup: {}",
            inventory::display(&stage_path)
        )
    })?;
    #[cfg(not(target_os = "macos"))]
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
) -> Result<()> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
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
        #[cfg(target_os = "linux")]
        let directory = fs::openat2(
            parent,
            name,
            flags,
            Mode::empty(),
            fs::ResolveFlags::NO_XDEV | fs::ResolveFlags::NO_SYMLINKS,
        )?;
        #[cfg(not(target_os = "linux"))]
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
            #[cfg(target_os = "macos")]
            if !tree.entries.contains_key(&relative.join(&child)) {
                finder::remove_metadata(&directory, &child)?;
                continue;
            }
            remove_tree(&directory, &child, &relative.join(&child), tree, flags)?;
        }
        #[cfg(target_os = "macos")]
        finder::finish(parent, name, &directory)?;
        #[cfg(not(target_os = "macos"))]
        fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    } else if !expected.is_dir && kind == FileType::RegularFile {
        let modified = std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(
            stat.st_mtime as u64,
            stat.st_mtime_nsec as u32,
        ));
        // Metadata changes after staging (including unlinking another hard link)
        // can change ctime without changing the reviewed file contents. Compare
        // the bytes below and still detect changes while that comparison runs.
        if stat.st_size as u64 != expected.len || modified != Some(expected.modified) {
            bail!("Staged file was modified: {relative:?}");
        }
        let file = fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        if stat_identity(&fs::fstat(&file)?) != expected.identity {
            bail!("Staged file was replaced: {relative:?}");
        }
        let mut file = std::fs::File::from(file);
        if Some(contents_hash(
            &mut file,
            expected.len,
            tree.fixture_digests.get(relative).copied(),
        )?) != expected.contents
        {
            bail!("Staged file contents changed: {relative:?}");
        }
        let after = fs::fstat(&file)?;
        if stat_identity(&after) != expected.identity
            || after.st_size != stat.st_size
            || after.st_mtime != stat.st_mtime
            || after.st_mtime_nsec != stat.st_mtime_nsec
            || after.st_ctime != stat.st_ctime
            || after.st_ctime_nsec != stat.st_ctime_nsec
        {
            bail!("Staged file changed during inspection: {relative:?}");
        }
        fs::unlinkat(parent, name, AtFlags::empty())?;
    } else {
        bail!("Unsupported staged entry: {relative:?}");
    }
    Ok(())
}

#[cfg(all(test, unix))]
pub(super) fn test_remove_staged(parent: &Path, tree: &artifacts::Tree) -> Result<()> {
    use rustix::fs::{self, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = fs::open(parent, flags, Mode::empty())?;
    remove_tree(&parent, "payload".as_ref(), Path::new(""), tree, flags)
}

#[cfg(windows)]
fn open_windows(path: &Path, allow_move: bool, read_contents: bool) -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_DATA,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let write = if read_contents { 0 } else { FILE_SHARE_WRITE };
    let delete = if allow_move { FILE_SHARE_DELETE } else { 0 };
    let handle = std::fs::OpenOptions::new()
        .access_mode(if read_contents { FILE_READ_DATA } else { 0 })
        .share_mode(FILE_SHARE_READ | write | delete)
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
        .map(|p| open_windows(p, false, false))
        .collect::<Result<Vec<_>>>()?;
    if identity(parent, &fs::symlink_metadata(parent)?)? != tree.parent {
        bail!("Output parent changed after review");
    }
    if &artifacts::stamp_tree(path, tree)? != tree {
        bail!("Output changed after review");
    }
    let stage = parent.join(format!(".still-stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let stage_guard = open_windows(&stage, false, false)?;
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
