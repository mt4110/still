//! Google Chrome's macOS default profile HTTP disk cache. Only this specific
//! subtree uses the rules below. A browser's profile also holds persistent
//! user data and is never a cleanup target here.
use super::{library, Row};
use crate::review::{artifacts, cleanup_history, docker, filesystem, git, inventory};
use anyhow::{ensure, Context, Result};
use chrono::Utc;
use std::fs::{self, Metadata};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const ID: &str = "com.google.Chrome";
const ROOT: &str = "Library/Caches/Google/Chrome";
const HTTP: &str = "Default/Cache/Cache_Data";

// Chromium net/disk_cache/simple/simple_entry_format.h and simple_index_file.h.
// The serialized real index stores its magic after the 8-byte Pickle header.
const ENTRY_MAGIC: u64 = 0xfcfb6d1ba7725c30;
const INDEX_MAGIC: u64 = 0x656e74657220796f;

fn home() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("ホームフォルダを確認できません。")?;
    inventory::safe_dir(dirs.home_dir())
}

pub(in crate::review) fn candidate(row: &Row) -> Option<PathBuf> {
    if row.label != "Chrome cache" || row.partial {
        return None;
    }
    let base = home().ok()?.join(ROOT);
    if row.native_path.as_deref()? != base {
        return None;
    }
    let target = base.join(HTTP);
    target
        .symlink_metadata()
        .is_ok_and(|m| m.is_dir())
        .then_some(target)
}

fn stopped_chrome() -> Result<PathBuf> {
    let bundle = library::lookup_bundles(&[ID.to_owned()])?
        .remove(ID)
        .context("Chrome のインストール先を確認できません。")?;
    ensure!(
        !bundle.running,
        "Chrome が起動中です。終了してから確認してください。"
    );
    inventory::safe_dir(&bundle.path)
}

#[derive(Debug)]
pub(in crate::review) struct PreparedCache {
    pub path: PathBuf,
    pub bundle_path: PathBuf,
    pub allocated_bytes: u64,
    pub logical_bytes: u64,
    pub files: usize,
    tree: artifacts::Tree,
    docker_context: Option<String>,
}

pub(in crate::review) fn prepare(
    path: &Path,
    docker_context: Option<&str>,
) -> Result<PreparedCache> {
    inspect(&home()?, path, docker_context, &stopped_chrome)
}

fn inspect(
    home: &Path,
    path: &Path,
    context: Option<&str>,
    owner: &impl Fn() -> Result<PathBuf>,
) -> Result<PreparedCache> {
    let fixed = home.join(ROOT).join(HTTP);
    ensure!(
        path == fixed && inventory::safe_dir(path)? == fixed,
        "Chrome の HTTP キャッシュ以外は対象にできません。"
    );
    ensure!(
        fs::symlink_metadata(path)?.dev() == fs::metadata(home)?.dev(),
        "別のボリュームにあります。"
    );
    let bundle_path = owner()?;
    git::protect_tracked(path, true)?;
    if let Some(context) = context {
        docker::protect_bind_mounts(context, path)?;
    }
    filesystem::ensure_not_open(path)?;
    let mut logical_bytes = 0u64;
    let mut allocated_bytes = 0u64;
    let mut files = 0;
    let tree = artifacts::stamp_with(
        path,
        artifacts::Producer::DeclaredOutput,
        |entry, relative, meta| {
            if !relative.as_os_str().is_empty() {
                verify_entry(entry, relative, meta)?;
            }
            if meta.is_file() {
                files += 1;
                logical_bytes = logical_bytes.saturating_add(meta.len());
                allocated_bytes = allocated_bytes.saturating_add(meta.blocks().saturating_mul(512));
            }
            Ok(())
        },
    )?;
    ensure!(
        tree.entries.contains_key(Path::new("index"))
            && tree
                .entries
                .contains_key(Path::new("index-dir/the-real-index")),
        "Chrome のディスクキャッシュ索引を確認できません。"
    );
    Ok(PreparedCache {
        path: fixed,
        bundle_path,
        files,
        allocated_bytes,
        logical_bytes,
        tree,
        docker_context: context.map(str::to_owned),
    })
}

fn verify_entry(entry: &Path, relative: &Path, meta: &Metadata) -> Result<()> {
    let unknown = || {
        format!(
            "Chrome のキャッシュ形式と一致しない項目があります: {}",
            inventory::display(relative)
        )
    };
    if meta.is_dir() {
        ensure!(relative == Path::new("index-dir"), "{}", unknown());
        return Ok(());
    }
    ensure!(meta.is_file() && meta.nlink() == 1, "{}", unknown());
    let (offset, expected) = if relative == Path::new("index-dir/the-real-index") {
        (8, INDEX_MAGIC)
    } else if relative == Path::new("index") {
        ensure!(meta.len() == 24, "{}", unknown());
        (0, ENTRY_MAGIC)
    } else {
        let valid = relative
            .to_str()
            .and_then(|s| s.rsplit_once('_'))
            .is_some_and(|(hash, suffix)| {
                hash.len() == 16
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && matches!(suffix, "0" | "1" | "s")
            });
        ensure!(valid, "{}", unknown());
        (0, ENTRY_MAGIC)
    };
    let mut file = fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(entry)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file()
            && (
                opened.dev(),
                opened.ino(),
                opened.ctime(),
                opened.ctime_nsec()
            ) == (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        "確認中にキャッシュが変更されました。"
    );
    let mut prefix = [0; 16];
    file.read_exact(&mut prefix[..offset + 8])
        .with_context(unknown)?;
    ensure!(
        prefix[offset..offset + 8] == expected.to_le_bytes(),
        "{}",
        unknown()
    );
    Ok(())
}

pub(in crate::review) fn apply(approved: PreparedCache) -> Result<cleanup_history::Record> {
    apply_in(&home()?, approved, &stopped_chrome)
}

fn apply_in(
    home: &Path,
    approved: PreparedCache,
    owner: &impl Fn() -> Result<PathBuf>,
) -> Result<cleanup_history::Record> {
    let parent = approved
        .path
        .parent()
        .context("キャッシュの親フォルダを確認できません。")?;
    let started_at = Utc::now();
    let before = filesystem::available(parent);
    let removal = (|| -> Result<()> {
        let current = inspect(
            home,
            &approved.path,
            approved.docker_context.as_deref(),
            owner,
        )?;
        ensure!(
            current.tree == approved.tree && current.bundle_path == approved.bundle_path,
            "確認後に内容や所有元が変わりました。再確認してください。"
        );
        // Computing a tree stamp can take time. Recheck live guards immediately
        // before the filesystem workflow renames the selected directory.
        ensure!(
            owner()? == approved.bundle_path,
            "Chrome の状態が変わりました。"
        );
        if let Some(context) = approved.docker_context.as_deref() {
            docker::protect_bind_mounts(context, &approved.path)?;
        }
        git::protect_tracked(&approved.path, true)?;
        filesystem::ensure_not_open(&approved.path)?;
        filesystem::remove_output(&approved.path, &approved.tree)
    })();
    let after = filesystem::available(parent);
    let exists = match fs::symlink_metadata(&approved.path) {
        Ok(_) => Some(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    };
    let reason = removal
        .err()
        .map(|err| format!("{err:#}"))
        .or_else(|| match exists {
            Some(true) => {
                Some("整理後に同じパスが存在します。再作成の可能性があります。".to_owned())
            }
            None => Some("整理後のパスを確認できませんでした。".to_owned()),
            Some(false) => None,
        });
    Ok(cleanup_history::Record {
        version: 1,
        root: home.join(ROOT),
        started_at,
        completed_at: Utc::now(),
        available_before: before,
        available_after: after,
        items: vec![cleanup_history::Item {
            path: approved.path,
            producer: "Google Chrome HTTP cache (Default)".to_owned(),
            logical_bytes: approved.logical_bytes,
            status: if reason.is_none() {
                "removed"
            } else {
                "incomplete"
            }
            .to_owned(),
            reason,
            original_path_exists: exists,
        }],
    })
}

#[cfg(test)]
fn fixture_layout(home: &Path) -> Result<(PathBuf, PathBuf)> {
    let path = home.join(ROOT).join(HTTP);
    let bundle = home.join("Applications/Google Chrome.app");
    fs::create_dir_all(&bundle)?;
    fs::create_dir_all(path.join("index-dir"))?;
    let mut index = [0; 24];
    index[..8].copy_from_slice(&ENTRY_MAGIC.to_le_bytes());
    fs::write(path.join("index"), index)?;
    let mut real = [0; 16];
    real[8..].copy_from_slice(&INDEX_MAGIC.to_le_bytes());
    fs::write(path.join("index-dir/the-real-index"), real)?;
    fs::write(path.join("0123456789abcdef_0"), ENTRY_MAGIC.to_le_bytes())?;
    Ok((path, bundle))
}

#[cfg(test)]
pub(in crate::review) fn prepared_fixture_for_ui(
    temp: &tempfile::TempDir,
) -> Result<PreparedCache> {
    let home = temp.path().canonicalize()?;
    let (path, bundle) = fixture_layout(&home)?;
    inspect(&home, &path, None, &|| Ok(bundle.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Cache {
        _temp: tempfile::TempDir,
        home: PathBuf,
        path: PathBuf,
        bundle: PathBuf,
    }

    impl Cache {
        fn new() -> Result<Self> {
            let temp = tempfile::tempdir()?;
            let home = temp.path().canonicalize()?;
            let (path, bundle) = fixture_layout(&home)?;
            Ok(Self {
                _temp: temp,
                home,
                path,
                bundle,
            })
        }

        fn prepare(&self) -> Result<PreparedCache> {
            inspect(&self.home, &self.path, None, &|| Ok(self.bundle.clone()))
        }
    }

    #[test]
    fn removes_only_confirmed_http_cache_and_records_observation() -> Result<()> {
        let cache = Cache::new()?;
        let no_vary = cache.path.with_file_name("No_Vary_Search");
        fs::write(&no_vary, b"keep sibling")?;
        let descriptor = cache.prepare()?;
        assert_eq!(descriptor.files, 3);
        assert_eq!(descriptor.logical_bytes, 48);
        let record = apply_in(&cache.home, descriptor, &|| Ok(cache.bundle.clone()))?;
        assert_eq!(record.items[0].status, "removed");
        assert_eq!(record.items[0].original_path_exists, Some(false));
        assert!(record.available_before.is_some() && record.available_after.is_some());
        assert_eq!(fs::read(no_vary)?, b"keep sibling");
        Ok(())
    }

    #[test]
    fn refuses_running_owner_and_changed_cache_between_review_and_apply() -> Result<()> {
        let cache = Cache::new()?;
        let not_stopped = || anyhow::bail!("Chrome is running");
        assert!(inspect(&cache.home, &cache.path, None, &not_stopped).is_err());
        let record = apply_in(&cache.home, cache.prepare()?, &not_stopped)?;
        assert_eq!(record.items[0].status, "incomplete");
        assert_eq!(record.items[0].original_path_exists, Some(true));
        let descriptor = cache.prepare()?;
        fs::write(
            cache.path.join("0123456789abcdef_1"),
            ENTRY_MAGIC.to_le_bytes(),
        )?;
        let record = apply_in(&cache.home, descriptor, &|| Ok(cache.bundle.clone()))?;
        assert_eq!(record.items[0].status, "incomplete");
        assert!(cache.path.join("index").exists());
        Ok(())
    }

    #[test]
    fn refuses_profile_other_locations_and_unrecognized_entries() -> Result<()> {
        let cache = Cache::new()?;
        let profile = cache
            .home
            .join("Library/Application Support/Google/Chrome/Default");
        fs::create_dir_all(&profile)?;
        assert!(inspect(&cache.home, &profile, None, &|| Ok(cache.bundle.clone())).is_err());
        for name in ["history.db", "notes", "1234567890abcdef_0"] {
            let c = Cache::new()?;
            fs::write(c.path.join(name), b"user contents")?;
            assert!(c.prepare().is_err(), "{name}");
        }
        let outside = cache.home.join("outside");
        fs::write(&outside, ENTRY_MAGIC.to_le_bytes())?;
        symlink(&outside, cache.path.join("0123456789abcdef_1"))?;
        assert!(cache.prepare().is_err());
        assert!(outside.is_file());
        Ok(())
    }

    #[test]
    fn refuses_busy_and_git_tracked_cache_entries() -> Result<()> {
        let cache = Cache::new()?;
        let opened = fs::File::open(cache.path.join("index"))?;
        assert!(cache.prepare().is_err());
        drop(opened);
        git::check(git::run(&cache.home, ["init", "-q"])?, "init")?;
        git::check(git::run(&cache.home, ["add", "Library"])?, "add")?;
        assert!(cache.prepare().is_err());
        Ok(())
    }
}
