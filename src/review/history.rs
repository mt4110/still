use super::{bytes, inventory, profile};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const VERSION: u8 = 1;

pub struct History {
    directory: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct SnapshotHeader {
    version: u8,
    root: NativePath,
    completed_at: DateTime<Utc>,
}

impl SnapshotHeader {
    fn verify(&self, root: &Path) -> Result<()> {
        if self.version != VERSION || self.root.as_os_str() != root.as_os_str() {
            bail!("History snapshot does not match this scan root or format version");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct SnapshotOutput<'a> {
    header: SnapshotHeader,
    directories: DirectoryObservations<'a>,
}

/// Serialize rows as they are visited, keeping the v1 snapshot format without
/// allocating another vector of paths for the entire workspace.
struct DirectoryObservations<'a> {
    root: &'a Path,
    rows: &'a [inventory::Row],
}

impl Serialize for DirectoryObservations<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeSeq};
        let mut seq = serializer.serialize_seq(Some(self.rows.len()))?;
        for row in self.rows {
            let relative = row.path.strip_prefix(self.root).map_err(S::Error::custom)?;
            seq.serialize_element(&(
                NativePath::from(relative.as_os_str()),
                Observed::from(&row.usage),
            ))?;
        }
        seq.end()
    }
}

#[derive(Deserialize, Serialize)]
struct ScanFile {
    header: SnapshotHeader,
    directories: Vec<(NativePath, Observed)>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum NativePath {
    Text(String),
    // OsString serialization preserves Unix bytes and Windows wide units.
    Raw(OsString),
}

impl NativePath {
    fn as_os_str(&self) -> &OsStr {
        match self {
            Self::Text(path) => OsStr::new(path),
            Self::Raw(path) => path,
        }
    }
}

impl From<&OsStr> for NativePath {
    fn from(path: &OsStr) -> Self {
        match path.to_str() {
            Some(path) => Self::Text(path.to_owned()),
            None => Self::Raw(path.to_owned()),
        }
    }
}

impl From<NativePath> for OsString {
    fn from(path: NativePath) -> Self {
        match path {
            NativePath::Text(path) => path.into(),
            NativePath::Raw(path) => path,
        }
    }
}

pub struct Snapshot {
    header: SnapshotHeader,
    directories: HashMap<OsString, Observed>,
}

#[derive(Deserialize, Serialize)]
struct Observed {
    logical_bytes: u64,
    allocated_bytes: Option<u64>,
    partial: bool,
}

impl From<&inventory::Usage> for Observed {
    fn from(usage: &inventory::Usage) -> Self {
        Self {
            logical_bytes: usage.logical_bytes,
            allocated_bytes: usage.allocated_bytes,
            partial: usage.skipped != 0,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    New,
    Up,
    Down,
    Unchanged,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Basis {
    Allocated,
    Logical,
}

#[derive(Clone, Copy, Serialize)]
pub struct Delta {
    direction: Direction,
    bytes: u64,
    partial: bool,
    basis: Basis,
}

impl Delta {
    pub fn signed_bytes(self) -> i128 {
        match self.direction {
            Direction::Down => -i128::from(self.bytes),
            _ => i128::from(self.bytes),
        }
    }

    #[cfg(all(feature = "desktop", target_os = "macos"))]
    pub fn is_partial(self) -> bool {
        self.partial
    }

    pub fn is_new(self) -> bool {
        matches!(self.direction, Direction::New)
    }

    pub fn label(self) -> String {
        let size = bytes(self.bytes);
        let label = match self.direction {
            Direction::New => format!("new {size}"),
            Direction::Up => format!("+{size}"),
            Direction::Down => format!("-{size}"),
            Direction::Unchanged => "0 B".into(),
        };
        if self.partial {
            return format!("{label}*");
        }
        label
    }
}

impl Snapshot {
    pub fn completed_at(&self) -> DateTime<Utc> {
        self.header.completed_at
    }

    pub fn delta(&self, row: &inventory::Row) -> Option<Delta> {
        if !row.is_dir {
            return None;
        }
        let relative = row.path.strip_prefix(self.header.root.as_os_str()).ok()?;
        let current = Observed::from(&row.usage);
        let Some(previous) = self.directories.get(relative.as_os_str()) else {
            let (basis, bytes) = current
                .allocated_bytes
                .map_or((Basis::Logical, current.logical_bytes), |size| {
                    (Basis::Allocated, size)
                });
            let prev_parent = relative
                .ancestors()
                .skip(1)
                .find_map(|ancestor| self.directories.get(ancestor.as_os_str()));
            return Some(Delta {
                direction: Direction::New,
                bytes,
                partial: current.partial || prev_parent.is_some_and(|parent| parent.partial),
                basis,
            });
        };
        let (basis, before, after) = match (previous.allocated_bytes, current.allocated_bytes) {
            (Some(before), Some(after)) => (Basis::Allocated, before, after),
            _ => (
                Basis::Logical,
                previous.logical_bytes,
                current.logical_bytes,
            ),
        };
        Some(Delta {
            direction: match after.cmp(&before) {
                std::cmp::Ordering::Greater => Direction::Up,
                std::cmp::Ordering::Less => Direction::Down,
                std::cmp::Ordering::Equal => Direction::Unchanged,
            },
            bytes: before.abs_diff(after),
            partial: current.partial || previous.partial,
            basis,
        })
    }
}

impl History {
    pub fn open(name: &str) -> Result<Self> {
        profile::validate_name(name)?;
        Self::in_directory(&format!("profile-{name}"))
    }

    #[cfg(all(feature = "desktop", target_os = "macos"))]
    pub fn workspace() -> Result<Self> {
        Self::in_directory("workspace")
    }

    fn in_directory(name: &str) -> Result<Self> {
        let dirs = crate::storage_paths::project_dirs()
            .context("Could not determine the still data directory")?;
        Ok(Self {
            directory: dirs.data_local_dir().join("review-history").join(name),
        })
    }

    pub fn previous(&self, root: &Path) -> Result<Option<Snapshot>> {
        let Some(record) = read::<ScanFile>(&self.snapshot_path(root))? else {
            return Ok(None);
        };
        record.header.verify(root)?;
        Ok(Some(Snapshot {
            header: record.header,
            directories: record
                .directories
                .into_iter()
                .map(|(path, observed)| (path.into(), observed))
                .collect(),
        }))
    }

    pub fn record(&self, root: &Path, inventory: &inventory::Inventory) -> Result<()> {
        let snapshot = SnapshotOutput {
            header: SnapshotHeader {
                version: VERSION,
                root: NativePath::from(root.as_os_str()),
                completed_at: Utc::now(),
            },
            directories: DirectoryObservations {
                root,
                rows: &inventory.directories,
            },
        };
        fs::create_dir_all(&self.directory)?;
        let path = self.snapshot_path(root);
        let mut temp = NamedTempFile::new_in(&self.directory)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            serde_json::to_writer(&mut writer, &snapshot)?;
            writer.flush()?;
        }
        temp.as_file().sync_all()?;

        #[derive(Deserialize)]
        struct HeaderOnly {
            header: SnapshotHeader,
        }
        // Serialize writers for this root while keeping readers atomic and other roots independent.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.try_lock()
            .context("Another still session is saving this scan history")?;
        if let Some(current) = read::<HeaderOnly>(&path)? {
            current.header.verify(root)?;
            if current.header.completed_at > snapshot.header.completed_at {
                return Ok(());
            }
        }
        temp.persist(&path)
            .with_context(|| format!("Could not save scan history at {path:?}"))?;
        Ok(())
    }

    fn snapshot_path(&self, root: &Path) -> PathBuf {
        // A stable FNV-1a key keeps filenames short. The stored root is checked before use.
        let hash = root
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        self.directory.join(format!("{hash:016x}.json"))
    }
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("Could not open scan history at {path:?}"))
        }
    };
    serde_json::from_reader(BufReader::new(file))
        .map(Some)
        .with_context(|| format!("Could not read scan history at {path:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::inventory::{Inventory, Row, Usage};

    fn row(path: PathBuf, logical: u64, allocated: Option<u64>, skipped: u64) -> Row {
        Row {
            path,
            is_dir: true,
            usage: Usage {
                logical_bytes: logical,
                allocated_bytes: allocated,
                skipped,
                ..Usage::default()
            },
        }
    }

    #[test]
    fn updates_only_the_scanned_root_and_preserves_loaded_base() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let record = |root: &Path, size| {
            history.record(
                root,
                &Inventory {
                    directories: vec![row(root.to_owned(), size, Some(size), 1)],
                    matches: Vec::new(),
                    omitted_matches: 0,
                    cargo_marker_dirs: Default::default(),
                },
            )
        };
        assert!(history.previous(&first)?.is_none());
        record(&first, 500)?;
        record(&second, 1_000)?;
        let baseline = history.previous(&first)?.context("first baseline")?;
        record(&first, 600)?;

        let current = row(first.clone(), 100, Some(400), 0);
        let change = baseline.delta(&current).context("first delta")?;
        assert_eq!(change.signed_bytes(), -100);
        assert!(change.partial);
        assert_eq!(
            history
                .previous(&first)?
                .unwrap()
                .delta(&current)
                .unwrap()
                .signed_bytes(),
            -200
        );
        assert_eq!(
            history
                .previous(&second)?
                .unwrap()
                .delta(&row(second, 1500, None, 0))
                .unwrap()
                .signed_bytes(),
            500
        );

        let child = first.join("new worktree");
        let new = baseline.delta(&row(child, 50, Some(512), 0)).unwrap();
        assert!(new.is_new());
        assert_eq!(new.signed_bytes(), 512);
        assert!(new.partial);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_non_unicode_directory_paths() -> Result<()> {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let odd_path = temp.path().join(OsString::from_vec(vec![b'x', 0xff]));
        history.record(
            temp.path(),
            &Inventory {
                directories: vec![row(odd_path.clone(), 3, None, 0)],
                matches: Vec::new(),
                omitted_matches: 0,
                cargo_marker_dirs: Default::default(),
            },
        )?;
        let baseline = history.previous(temp.path())?.unwrap();
        let change = baseline.delta(&row(odd_path, 5, None, 0)).unwrap();
        assert!(!change.is_new());
        assert_eq!(change.signed_bytes(), 2);
        Ok(())
    }
}
