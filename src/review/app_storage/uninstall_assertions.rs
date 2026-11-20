//! User-reported checks only. These records never authorize cleanup.
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 4 * 1024 * 1024;

fn stamp(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::review) enum Scope {
    SystemApplications,
    UserApplications,
    BothApplications,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Confirmed,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::review) struct Record {
    state: State,
    pub scope: Scope,
    pub confirmed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Record {
    pub(in crate::review) fn confirmed(&self) -> bool {
        self.state == State::Confirmed
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Document {
    version: u8,
    records: BTreeMap<String, Record>,
}

#[derive(Default)]
pub(in crate::review) struct Records {
    records: BTreeMap<String, Record>,
}

fn path() -> Result<std::path::PathBuf> {
    Ok(crate::storage_paths::project_dirs()
        .context("Could not locate still data")?
        .data_local_dir()
        .join("user-uninstall-confirmations.json"))
}

fn valid_id(id: &str) -> bool {
    id.len() <= 255
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

impl Records {
    pub(in crate::review) fn load() -> Result<Self> {
        Self::read(&path()?)
    }
    fn read(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        let before = file.metadata()?;
        ensure!(
            before.is_file() && before.len() <= MAX_BYTES,
            "Invalid or oversized uninstall confirmation file"
        );
        let mut bytes = Vec::new();
        (&mut file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Oversized uninstall confirmation file"
        );
        let after = file.metadata()?;
        let current = fs::symlink_metadata(path)?;
        ensure!(
            after.is_file()
                && current.is_file()
                && stamp(&before) == stamp(&after)
                && stamp(&after) == stamp(&current),
            "Uninstall confirmation file changed while reading"
        );
        let document: Document = serde_json::from_slice(&bytes)?;
        ensure!(
            document.version == 1 && document.records.keys().all(|id| valid_id(id)),
            "Invalid uninstall confirmation file"
        );
        Ok(Self {
            records: document.records,
        })
    }
    pub(in crate::review) fn get(&self, id: &str) -> Option<&Record> {
        self.records.get(id)
    }
    pub(in crate::review) fn entries(&self) -> impl Iterator<Item = (&str, &Record)> {
        self.records
            .iter()
            .map(|(id, record)| (id.as_str(), record))
    }
    pub(in crate::review) fn confirm(id: &str, scope: Scope) -> Result<Self> {
        Self::update(&path()?, id, Some(scope))
    }
    pub(in crate::review) fn revoke(id: &str) -> Result<Self> {
        Self::update(&path()?, id, None)
    }
    fn update(file: &Path, id: &str, scope: Option<Scope>) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        ensure!(valid_id(id), "Invalid bundle ID");
        let parent = file.parent().context("No still data directory")?;
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent.join("user-uninstall-confirmations.lock"))?;
        ensure!(lock.metadata()?.is_file(), "Invalid confirmation lock");
        lock.try_lock()
            .map_err(|error| anyhow::anyhow!("Confirmation records are busy: {error}"))?;
        let mut current = Self::read(file)?;
        let now = Utc::now();
        if let Some(scope) = scope {
            current.records.insert(
                id.into(),
                Record {
                    state: State::Confirmed,
                    scope,
                    confirmed_at: now,
                    updated_at: now,
                },
            );
        } else {
            let record = current
                .records
                .get_mut(id)
                .context("No user confirmation to revoke")?;
            ensure!(
                record.confirmed(),
                "The user confirmation is already revoked"
            );
            record.state = State::Revoked;
            record.updated_at = now;
        }
        let temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            temp.as_file(),
            &Document {
                version: 1,
                records: current.records.clone(),
            },
        )?;
        ensure!(
            temp.as_file().metadata()?.len() <= MAX_BYTES,
            "Uninstall confirmation file would exceed the supported size"
        );
        temp.as_file().sync_all()?;
        temp.persist(file)?;
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_scopes_confirm_reconfirm_and_revoke_without_other_ids_changing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("confirmations.json");
        let first = Records::update(&path, "com.example.one", Some(Scope::SystemApplications))?;
        assert!(first.get("com.example.one").unwrap().confirmed());
        Records::update(&path, "com.example.two", Some(Scope::UserApplications))?;
        let revised = Records::update(&path, "com.example.one", Some(Scope::BothApplications))?;
        assert_eq!(
            revised.get("com.example.one").unwrap().scope,
            Scope::BothApplications
        );
        let revoked = Records::update(&path, "com.example.one", None)?;
        assert!(!revoked.get("com.example.one").unwrap().confirmed());
        assert!(revoked.get("com.example.two").unwrap().confirmed());
        assert!(!Records::read(&path)?
            .get("com.example.one")
            .unwrap()
            .confirmed());
        assert!(Records::update(&path, "../bad", Some(Scope::BothApplications)).is_err());
        assert!(Records::update(&path, "com.example.missing", None).is_err());
        Ok(())
    }

    #[test]
    fn busy_or_invalid_records_never_overwrite_existing_data() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("confirmations.json");
        Records::update(&path, "com.example.one", Some(Scope::BothApplications))?;
        let original = fs::read(&path)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("user-uninstall-confirmations.lock"))?;
        lock.try_lock()?;
        assert!(
            Records::update(&path, "com.example.two", Some(Scope::SystemApplications)).is_err()
        );
        assert_eq!(fs::read(&path)?, original);
        drop(lock);
        let broken = dir.path().join("broken.json");
        fs::write(&broken, b"not-json")?;
        assert!(
            Records::update(&broken, "com.example.two", Some(Scope::UserApplications)).is_err()
        );
        assert_eq!(fs::read(&broken)?, b"not-json");
        Ok(())
    }

    #[test]
    fn rejects_symlinks_fifos_and_oversized_input_without_waiting() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("target");
        fs::write(&target, b"{}")?;
        let link = dir.path().join("link");
        symlink(&target, &link)?;
        assert!(Records::read(&link).is_err());
        let pipe = dir.path().join("pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes())?;
        // SAFETY: name is a live, NUL-terminated temporary path and mode is valid.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let before = std::time::Instant::now();
        assert!(Records::read(&pipe).is_err());
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        let large = dir.path().join("large");
        fs::write(&large, vec![b' '; MAX_BYTES as usize + 1])?;
        assert!(Records::read(&large).is_err());
        Ok(())
    }

    #[test]
    fn update_that_crosses_size_limit_preserves_existing_records() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("confirmations.json");
        let record = Record {
            state: State::Confirmed,
            scope: Scope::BothApplications,
            confirmed_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let id = |n: usize| format!("com.example.{n:06}.{}", "x".repeat(220));
        let empty = Document {
            version: 1,
            records: BTreeMap::new(),
        };
        let empty_len = serde_json::to_vec(&empty)?.len();
        let sample = Document {
            version: 1,
            records: BTreeMap::from([(id(0), record.clone())]),
        };
        let entry_cost = serde_json::to_vec(&sample)?.len() - empty_len + 1;
        let count = (MAX_BYTES as usize - empty_len + 1) / entry_cost;
        let records = (0..count).map(|n| (id(n), record.clone())).collect();
        let original = serde_json::to_vec(&Document {
            version: 1,
            records,
        })?;
        assert!(original.len() <= MAX_BYTES as usize);
        fs::write(&path, &original)?;
        let oversized_id = format!("com.example.{}", "y".repeat(243));
        assert!(valid_id(&oversized_id));
        let error = Records::update(&path, &oversized_id, Some(Scope::SystemApplications))
            .err()
            .context("expected size rejection")?;
        assert!(format!("{error:#}").contains("exceed"));
        assert_eq!(fs::read(&path)?, original);
        Ok(())
    }
}
