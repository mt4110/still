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
        self.localized_label(false)
    }

    pub fn localized_label(self, japanese: bool) -> String {
        let size = bytes(self.bytes);
        let label = match self.direction {
            Direction::New if japanese => format!("新規 {size}"),
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
                return self.save_inventory_summary(root, inventory, snapshot.header.completed_at)
                    .context("A newer latest scan was retained, but this scan's long-term summary could not be saved");
            }
        }
        temp.persist(&path)
            .with_context(|| format!("Could not save scan history at {path:?}"))?;
        // Keep one compact root total per explicit observation, rather than
        // archiving every directory in each full snapshot. The existing latest
        // snapshot remains usable even if this separate history write fails.
        self.save_inventory_summary(root, inventory, snapshot.header.completed_at)
            .context("Latest scan saved, but its long-term summary could not be saved")?;
        Ok(())
    }

    fn save_inventory_summary(
        &self,
        root: &Path,
        inventory: &inventory::Inventory,
        completed_at: DateTime<Utc>,
    ) -> Result<()> {
        let root_row = inventory
            .directories
            .iter()
            .find(|row| row.path == root)
            .context("Scan did not contain a root total")?;
        let summary = ScanSummary {
            header: SnapshotHeader {
                version: VERSION,
                root: NativePath::from(root.as_os_str()),
                completed_at,
            },
            total: Observed::from(&root_row.usage),
        };
        self.save_summary(root, &summary)
    }

    fn series_dir(&self, root: &Path) -> PathBuf {
        let snapshot = self.snapshot_path(root);
        self.directory
            .join("series")
            .join(snapshot.file_stem().expect("snapshot name"))
    }

    fn save_summary(&self, root: &Path, summary: &ScanSummary) -> Result<()> {
        let dir = self.series_dir(root);
        fs::create_dir_all(&dir)?;
        let mut temp = NamedTempFile::new_in(&dir)?;
        serde_json::to_writer(&mut temp, summary)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        let filename = format!(
            "{}-{}.json",
            summary.header.completed_at.format("%Y%m%dT%H%M%S%9fZ"),
            uuid::Uuid::new_v4()
        );
        temp.persist_noclobber(dir.join(filename))?;
        Ok(())
    }

    fn summaries(&self, root: &Path, limit: usize) -> Result<Vec<ScanSummary>> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let dir = self.series_dir(root);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        // Keep only the newest requested paths even after years of scans.
        let mut paths = BinaryHeap::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "json")
            {
                let path = entry.path();
                if paths.len() < limit {
                    paths.push(Reverse(path));
                } else if paths.peek().is_some_and(|oldest| path > oldest.0) {
                    paths.pop();
                    paths.push(Reverse(path));
                }
            }
        }
        paths
            .into_sorted_vec()
            .into_iter()
            .map(|path| path.0)
            .map(|path| {
                let summary: ScanSummary = read(&path)?.context("Scan summary disappeared")?;
                summary.header.verify(root)?;
                Ok(summary)
            })
            .collect()
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

#[derive(Deserialize, Serialize)]
struct ScanSummary {
    header: SnapshotHeader,
    total: Observed,
}

#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) struct HistoryPoint {
    pub completed_at: DateTime<Utc>,
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub partial: bool,
}

#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) fn load_timeline(
    root: &Path,
    profile: Option<&str>,
    limit: usize,
) -> Result<Vec<HistoryPoint>> {
    let store = match profile {
        Some(name) => History::open(name)?,
        None => History::workspace()?,
    };
    store.timeline(root, limit)
}

#[cfg(all(feature = "desktop", target_os = "macos"))]
impl History {
    fn timeline(&self, root: &Path, limit: usize) -> Result<Vec<HistoryPoint>> {
        Ok(self
            .summaries(root, limit)?
            .into_iter()
            .map(|entry| HistoryPoint {
                completed_at: entry.header.completed_at,
                logical_bytes: entry.total.logical_bytes,
                allocated_bytes: entry.total.allocated_bytes,
                partial: entry.total.partial,
            })
            .collect())
    }
}

pub(crate) fn run(args: &crate::cli::ReviewHistoryArgs) -> Result<()> {
    use crate::cli::ListFormat;
    anyhow::ensure!(args.root.is_absolute(), "Root must be an absolute path");
    // A removed workspace can still be inspected by its original absolute path.
    let root = match args.root.canonicalize() {
        Ok(root) => root,
        Err(err) if err.kind() == io::ErrorKind::NotFound => args.root.clone(),
        Err(err) => return Err(err.into()),
    };
    let history = match &args.profile {
        Some(name) => History::open(name)?,
        None => History::workspace()?,
    };
    let records = history.summaries(&root, usize::from(args.limit))?;
    match args.format {
        ListFormat::Json => println!("{}", serde_json::to_string_pretty(&records)?),
        ListFormat::Text => {
            if records.is_empty() {
                println!("No recorded scan totals for this root. Earlier snapshots are not reconstructed.");
            }
            for record in records {
                println!(
                    "{} · logical {} bytes · allocated {} · {}",
                    record.header.completed_at,
                    record.total.logical_bytes,
                    record
                        .total
                        .allocated_bytes
                        .map_or_else(|| "unavailable".to_owned(), |n| format!("{n} bytes")),
                    if record.total.partial {
                        "partial scan"
                    } else {
                        "complete scan"
                    }
                );
            }
        }
    }
    Ok(())
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

    #[test]
    fn new_growth_label_respects_language_and_partial_status() {
        let delta = Delta {
            direction: Direction::New,
            bytes: 1024,
            partial: true,
            basis: Basis::Logical,
        };
        assert!(delta.localized_label(true).starts_with("新規 "));
        assert!(delta.localized_label(true).ends_with('*'));
        assert!(delta.label().starts_with("new "));
    }

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
    fn compact_series_keeps_independent_roots_partial_status_and_limit() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        assert!(history.summaries(&first, 20)?.is_empty());
        let inventory = |root: &Path, size, allocated, skipped| Inventory {
            directories: vec![row(root.to_owned(), size, allocated, skipped)],
            matches: Vec::new(),
            omitted_matches: 0,
            cargo_marker_dirs: Default::default(),
        };
        history.record(&first, &inventory(&first, 50, Some(100), 0))?;
        history.record(&second, &inventory(&second, 1000, Some(2048), 0))?;
        history.record(&first, &inventory(&first, 70, None, 2))?;
        let first_records = history.summaries(&first, 20)?;
        assert_eq!(first_records.len(), 2);
        assert_eq!(first_records[0].total.logical_bytes, 70);
        assert!(first_records[0].total.allocated_bytes.is_none());
        assert!(first_records[0].total.partial);
        assert_eq!(first_records[1].total.allocated_bytes, Some(100));
        assert!(!first_records[1].total.partial);
        assert_eq!(history.summaries(&first, 1)?.len(), 1);
        assert_eq!(history.summaries(&second, 20)?[0].total.logical_bytes, 1000);
        #[cfg(all(feature = "desktop", target_os = "macos"))]
        {
            let points = history.timeline(&first, 100)?;
            assert_eq!(points.len(), 2);
            assert_eq!(points[0].logical_bytes, 70);
            assert_eq!(points[0].allocated_bytes, None);
            assert!(points[0].partial);
            assert_eq!(points[1].allocated_bytes, Some(100));
            assert!(!points[1].partial);
            assert!(history
                .timeline(&temp.path().join("missing"), 100)?
                .is_empty());
        }
        assert_eq!(
            history
                .previous(&first)?
                .unwrap()
                .delta(&row(first.clone(), 75, None, 0))
                .unwrap()
                .signed_bytes(),
            5
        );
        Ok(())
    }

    #[test]
    fn series_uses_fixed_precision_chronological_order_and_rejects_wrong_root() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let root = temp.path().join("root");
        let summary = |when: &str, name: &Path, bytes| ScanSummary {
            header: SnapshotHeader {
                version: VERSION,
                root: NativePath::from(name.as_os_str()),
                completed_at: when.parse().unwrap(),
            },
            total: Observed {
                logical_bytes: bytes,
                allocated_bytes: None,
                partial: false,
            },
        };
        history.save_summary(&root, &summary("2026-01-01T00:00:00.123Z", &root, 1))?;
        history.save_summary(&root, &summary("2026-01-01T00:00:00.123001Z", &root, 2))?;
        assert_eq!(history.summaries(&root, 2)?[0].total.logical_bytes, 2);
        history.save_summary(&root, &summary("2026-01-01T00:00:00.123Z", &root, 1))?;
        assert_eq!(history.summaries(&root, 10)?.len(), 3);
        history.save_summary(
            &root,
            &summary("2027-01-01T00:00:00Z", &temp.path().join("other"), 3),
        )?;
        assert!(history.summaries(&root, 1).is_err());
        #[cfg(all(feature = "desktop", target_os = "macos"))]
        assert!(history.timeline(&root, 1).is_err());
        Ok(())
    }

    #[test]
    fn older_observation_keeps_latest_snapshot_and_records_its_total() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let root = temp.path().join("root");
        let inventory = |size| Inventory {
            directories: vec![row(root.clone(), size, Some(size), 0)],
            matches: Vec::new(),
            omitted_matches: 0,
            cargo_marker_dirs: Default::default(),
        };
        history.record(&root, &inventory(50))?;
        let path = history.snapshot_path(&root);
        let mut latest: serde_json::Value = serde_json::from_reader(File::open(&path)?)?;
        latest["header"]["completed_at"] = serde_json::json!("2099-01-01T00:00:00Z");
        fs::write(path, serde_json::to_vec(&latest)?)?;
        history.record(&root, &inventory(80))?;
        assert_eq!(history.summaries(&root, 10)?.len(), 2);
        assert_eq!(
            history
                .previous(&root)?
                .unwrap()
                .delta(&row(root.clone(), 55, Some(55), 0))
                .unwrap()
                .signed_bytes(),
            5
        );
        Ok(())
    }

    #[test]
    fn summary_failure_reports_error_but_keeps_latest_snapshot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let root = temp.path().join("root");
        fs::create_dir_all(&history.directory)?;
        fs::write(history.directory.join("series"), b"not a directory")?;
        let inventory = Inventory {
            directories: vec![row(root.clone(), 50, Some(100), 0)],
            matches: Vec::new(),
            omitted_matches: 0,
            cargo_marker_dirs: Default::default(),
        };
        let err = history.record(&root, &inventory).unwrap_err();
        assert!(format!("{err:#}").contains("long-term summary"));
        assert!(history.previous(&root)?.is_some());
        Ok(())
    }

    #[test]
    fn old_snapshot_remains_readable_without_inventing_series() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let history = History {
            directory: temp.path().join("snapshots"),
        };
        let root = temp.path().join("root");
        let rows = vec![row(root.clone(), 50, Some(100), 0)];
        let snapshot = SnapshotOutput {
            header: SnapshotHeader {
                version: VERSION,
                root: NativePath::from(root.as_os_str()),
                completed_at: Utc::now(),
            },
            directories: DirectoryObservations {
                root: &root,
                rows: &rows,
            },
        };
        fs::create_dir_all(&history.directory)?;
        serde_json::to_writer(File::create(history.snapshot_path(&root))?, &snapshot)?;
        assert!(history.previous(&root)?.is_some());
        assert!(history.summaries(&root, 20)?.is_empty());
        Ok(())
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
                directories: vec![
                    row(temp.path().to_owned(), 3, None, 0),
                    row(odd_path.clone(), 3, None, 0),
                ],
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
