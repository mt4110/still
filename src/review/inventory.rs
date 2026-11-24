use super::message::Message;
use anyhow::{bail, Context, Result};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) mod live;

#[derive(Default)]
pub struct ScanProgress {
    files: AtomicU64,
    directories: AtomicU64,
    logical_bytes: AtomicU64,
    current_directory: Mutex<Option<PathBuf>>,
    inventory_done: AtomicBool,
}

#[cfg(any(
    test,
    all(feature = "desktop", any(target_os = "macos", target_os = "windows"))
))]
pub struct ProgressSnapshot {
    pub files: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    pub current_directory: Option<PathBuf>,
    pub inventory_done: bool,
}

impl ScanProgress {
    #[cfg(any(
        test,
        all(feature = "desktop", any(target_os = "macos", target_os = "windows"))
    ))]
    pub fn snapshot(&self) -> ProgressSnapshot {
        let inventory_done = self.inventory_done.load(Ordering::Acquire);
        ProgressSnapshot {
            files: self.files.load(Ordering::Relaxed),
            directories: self.directories.load(Ordering::Relaxed),
            logical_bytes: self.logical_bytes.load(Ordering::Relaxed),
            current_directory: self
                .current_directory
                .lock()
                .expect("scan progress")
                .clone(),
            inventory_done,
        }
    }
}

#[derive(Default)]
struct ProgressBatch {
    files: u64,
    directories: u64,
    logical_bytes: u64,
}

impl ProgressBatch {
    fn publish(&mut self, progress: Option<&ScanProgress>, directory: Option<&Path>) {
        let pending = std::mem::take(self);
        let Some(progress) = progress else { return };
        progress.files.fetch_add(pending.files, Ordering::Relaxed);
        progress
            .directories
            .fetch_add(pending.directories, Ordering::Relaxed);
        progress
            .logical_bytes
            .fetch_add(pending.logical_bytes, Ordering::Relaxed);
        if let Some(directory) = directory {
            if let Ok(mut current) = progress.current_directory.try_lock() {
                *current = Some(directory.to_owned());
            }
        }
    }
}

#[derive(Clone, Default, Serialize)]
pub struct Usage {
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub largest_file_bytes: u64,
    pub files: u64,
    pub skipped: u64,
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub files_modified_last_7_days: u64,
}

impl Usage {
    fn add(&mut self, other: &Self) {
        self.logical_bytes = self.logical_bytes.saturating_add(other.logical_bytes);
        self.allocated_bytes = match (self.allocated_bytes, other.allocated_bytes) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (Some(a), None) if other.files == 0 => Some(a),
            (None, Some(b)) if self.files == 0 => Some(b),
            _ => None,
        };
        self.largest_file_bytes = self.largest_file_bytes.max(other.largest_file_bytes);
        self.files += other.files;
        self.skipped += other.skipped;
        self.files_modified_last_7_days = self
            .files_modified_last_7_days
            .saturating_add(other.files_modified_last_7_days);
    }

    pub(super) fn file(metadata: &Metadata) -> Self {
        Self {
            logical_bytes: metadata.len(),
            allocated_bytes: allocated_bytes(metadata),
            largest_file_bytes: metadata.len(),
            files: 1,
            skipped: 0,
            modified_at: metadata.modified().ok().map(Into::into),
            files_modified_last_7_days: 0,
        }
    }

    pub fn size(&self) -> u64 {
        self.allocated_bytes.unwrap_or(self.logical_bytes)
    }
}

#[derive(Clone, Serialize)]
pub struct Row {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub is_dir: bool,
    pub usage: Usage,
}

#[derive(Serialize)]
pub struct Inventory {
    pub directories: Vec<Row>,
    pub matches: Vec<Row>,
    pub omitted_matches: usize,
    #[serde(skip)]
    pub cargo_marker_dirs: HashSet<PathBuf>,
}

#[derive(Default)]
struct Batch {
    raw: RawDirectories,
    matches: Vec<Row>,
    match_count: usize,
}

const MATCH_LIMIT: usize = 1000;

pub fn scan(root: &Path, threads: u16, query: Option<&str>) -> Result<Inventory> {
    scan_with_progress(root, threads, query, None)
}

pub fn scan_with_progress(
    root: &Path,
    threads: u16,
    query: Option<&str>,
    progress: Option<&ScanProgress>,
) -> Result<Inventory> {
    scan_impl(root, threads, query, progress, false).map(|(inventory, _)| inventory)
}

#[derive(Clone, Default)]
struct RawDirectories {
    directories: HashMap<PathBuf, Usage>,
    cargo_marker_dirs: HashSet<PathBuf>,
    skipped: u64,
    #[cfg(unix)]
    hardlinks: Vec<((u64, u64), PathBuf, u64)>,
}

fn scan_impl(
    root: &Path,
    threads: u16,
    query: Option<&str>,
    progress: Option<&ScanProgress>,
    retain_directories: bool,
) -> Result<(Inventory, Option<RawDirectories>)> {
    let root_metadata = fs::symlink_metadata(root)?;

    if !root_metadata.is_dir() || is_link(&root_metadata) {
        bail!("Scan root must be a directory: {root:?}");
    }
    if let Some(progress) = progress {
        progress.files.store(0, Ordering::Relaxed);
        progress.directories.store(1, Ordering::Relaxed);
        progress.logical_bytes.store(0, Ordering::Relaxed);
        progress.inventory_done.store(false, Ordering::Release);
        *progress.current_directory.lock().expect("scan progress") = Some(root.to_owned());
    }

    let mut merged = Batch::default();
    merged.raw.directories.insert(
        root.to_owned(),
        Usage {
            modified_at: root_metadata.modified().ok().map(Into::into),
            ..Usage::default()
        },
    );
    let scan_started = chrono::Utc::now();
    let recent_cutoff = scan_started - chrono::TimeDelta::days(7);
    let result = Mutex::new(merged);
    let query = query.filter(|q| !q.is_empty()).map(str::to_lowercase);
    let mut walker = WalkBuilder::new(root);
    walker
        .threads(usize::from(threads))
        .follow_links(false)
        .same_file_system(true)
        .standard_filters(false);
    walker.build_parallel().run(|| {
        // Merge once per worker instead of taking a shared lock for every file.
        let mut worker = Worker {
            results: &result,
            batch: Batch::default(),
            progress,
            pending_progress: ProgressBatch::default(),
            scan_started,
            recent_cutoff,
        };
        let query = query.as_deref();
        let root_metadata = &root_metadata;
        Box::new(move |entry| worker.visit(entry, root, root_metadata, query))
    });

    let Batch {
        raw,
        mut matches,
        match_count,
    } = result.into_inner().expect("walker results");
    let retained = retain_directories.then(|| raw.clone());
    let (directories, cargo_marker_dirs) = raw.aggregate(root);
    let mut directory_matches = 0;
    if let Some(query) = query.as_deref() {
        for directory in &directories {
            if matches_query(&directory.path, query) {
                directory_matches += 1;
                matches.push(directory.clone());
                trim_matches(&mut matches);
            }
        }
    }
    sort_rows(&mut matches, false);
    matches.truncate(MATCH_LIMIT);
    let inventory = Inventory {
        directories,
        omitted_matches: (match_count + directory_matches).saturating_sub(matches.len()),
        cargo_marker_dirs,
        matches,
    };
    if let Some(progress) = progress {
        progress.inventory_done.store(true, Ordering::Release);
    }
    Ok((inventory, retained))
}

impl RawDirectories {
    fn aggregate(self, root: &Path) -> (Vec<Row>, HashSet<PathBuf>) {
        let Self {
            mut directories,
            cargo_marker_dirs,
            skipped,
            #[cfg(unix)]
            hardlinks,
        } = self;
        directories.entry(root.to_owned()).or_default().skipped += skipped;
        #[cfg(unix)]
        {
            let mut unique = HashMap::new();
            for (inode, path, blocks) in hardlinks {
                remember_hardlink(&mut unique, inode, path, blocks);
            }
            for (path, blocks) in unique.into_values() {
                if let Some(parent) = path.parent() {
                    let usage = directories.entry(parent.to_owned()).or_default();
                    usage.allocated_bytes =
                        Some(usage.allocated_bytes.unwrap_or(0).saturating_add(blocks));
                }
            }
        }
        let mut directories: Vec<_> = directories
            .into_iter()
            .map(|(path, usage)| Row {
                path,
                is_dir: true,
                usage,
            })
            .collect();
        directories.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        accumulate_directories(&mut directories);
        (directories, cargo_marker_dirs)
    }
}

struct Worker<'a> {
    results: &'a Mutex<Batch>,
    batch: Batch,
    progress: Option<&'a ScanProgress>,
    pending_progress: ProgressBatch,
    scan_started: chrono::DateTime<chrono::Utc>,
    recent_cutoff: chrono::DateTime<chrono::Utc>,
}

impl Worker<'_> {
    fn visit(
        &mut self,
        entry: Result<ignore::DirEntry, ignore::Error>,
        root: &Path,
        root_metadata: &Metadata,
        query: Option<&str>,
    ) -> WalkState {
        let Ok(entry) = entry else {
            self.batch.raw.skipped += 1;
            return WalkState::Continue;
        };
        let path = entry.path();
        if path == root {
            return WalkState::Continue;
        }

        let parent = path.parent().expect("walker child has a parent");
        let Ok(metadata) = fs::symlink_metadata(path) else {
            self.batch
                .raw
                .directories
                .entry(parent.to_owned())
                .or_default()
                .skipped += 1;
            return WalkState::Skip;
        };
        if is_link(&metadata) || !same_device(root_metadata, &metadata) {
            self.batch
                .raw
                .directories
                .entry(parent.to_owned())
                .or_default()
                .skipped += 1;
            return WalkState::Skip;
        } else if metadata.is_dir() {
            self.batch
                .raw
                .directories
                .entry(path.to_owned())
                .or_default()
                .modified_at = metadata.modified().ok().map(Into::into);
            self.pending_progress.directories += 1;
        } else if metadata.is_file() {
            let stats = record_file(
                &mut self.batch.raw,
                path,
                parent,
                &metadata,
                self.recent_cutoff,
                self.scan_started,
            );
            self.pending_progress.files += 1;
            self.pending_progress.logical_bytes = self
                .pending_progress
                .logical_bytes
                .saturating_add(metadata.len());
            if query.is_some_and(|q| matches_query(path, q)) {
                self.batch.match_count += 1;
                self.batch.matches.push(Row {
                    path: path.to_owned(),
                    is_dir: false,
                    usage: stats,
                });
                trim_matches(&mut self.batch.matches);
            }
        } else {
            self.batch
                .raw
                .directories
                .entry(parent.to_owned())
                .or_default()
                .skipped += 1;
        }
        if self.pending_progress.files + self.pending_progress.directories >= 128 {
            self.pending_progress.publish(
                self.progress,
                Some(if metadata.is_dir() { path } else { parent }),
            );
        }
        WalkState::Continue
    }
}

fn record_file(
    raw: &mut RawDirectories,
    path: &Path,
    parent: &Path,
    metadata: &Metadata,
    recent_cutoff: chrono::DateTime<chrono::Utc>,
    scan_started: chrono::DateTime<chrono::Utc>,
) -> Usage {
    if matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".rustc_info.json" | "CACHEDIR.TAG")
    ) {
        raw.cargo_marker_dirs.insert(parent.to_owned());
    }
    let mut stats = Usage::file(metadata);
    stats.files_modified_last_7_days = u64::from(
        stats
            .modified_at
            .is_some_and(|time| time >= recent_cutoff && time <= scan_started),
    );
    let counted = stats.clone();
    #[cfg(unix)]
    let counted = {
        use std::os::unix::fs::MetadataExt;
        let mut counted = counted;
        if metadata.nlink() > 1 {
            let blocks = counted.allocated_bytes.replace(0).unwrap_or(0);
            raw.hardlinks
                .push(((metadata.dev(), metadata.ino()), path.to_owned(), blocks));
        }
        counted
    };
    raw.directories
        .entry(parent.to_owned())
        .or_default()
        .add(&counted);
    stats
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        self.pending_progress.publish(self.progress, None);
        let mut results = self.results.lock().expect("walker results");
        // Reuse the larger hash table rather than allocating another table of
        // comparable size while the worker-owned one is still resident.
        if self.batch.raw.directories.len() > results.raw.directories.len() {
            std::mem::swap(
                &mut self.batch.raw.directories,
                &mut results.raw.directories,
            );
        }
        for (path, usage) in self.batch.raw.directories.drain() {
            let merged = results.raw.directories.entry(path).or_default();
            // The worker visiting the directory owns its timestamp; another worker
            // may have accumulated child sizes without inspecting the directory itself.
            merged.modified_at = merged.modified_at.or(usage.modified_at);
            merged.add(&usage);
        }
        results.match_count += self.batch.match_count;
        results.raw.skipped += self.batch.raw.skipped;
        results
            .raw
            .cargo_marker_dirs
            .extend(self.batch.raw.cargo_marker_dirs.drain());
        results.matches.append(&mut self.batch.matches);
        #[cfg(unix)]
        results.raw.hardlinks.append(&mut self.batch.raw.hardlinks);
        trim_matches(&mut results.matches);
    }
}

#[cfg(unix)]
fn remember_hardlink(
    links: &mut HashMap<(u64, u64), (PathBuf, u64)>,
    inode: (u64, u64),
    path: PathBuf,
    blocks: u64,
) {
    use std::collections::hash_map::Entry;
    match links.entry(inode) {
        Entry::Occupied(mut entry) if path < entry.get().0 => {
            entry.insert((path, blocks));
        }
        Entry::Vacant(entry) => {
            entry.insert((path, blocks));
        }
        Entry::Occupied(_) => {}
    }
}

// Directory order is preorder: each parent precedes its contiguous subtree.
// Close finished subtrees into their parents as the next sibling is reached.
// This keeps one owned path per directory throughout aggregation.
fn accumulate_directories(rows: &mut [Row]) {
    let mut ancestors: Vec<usize> = Vec::new();
    for next in 0..=rows.len() {
        while let Some(&last) = ancestors.last() {
            if rows.get(next).and_then(|r| r.path.parent()) == Some(rows[last].path.as_path()) {
                break;
            }
            ancestors.pop();
            if let Some(&ancestor) = ancestors.last() {
                let (parents, child) = rows.split_at_mut(last);
                parents[ancestor].usage.add(&child[0].usage);
            }
        }
        if next < rows.len() {
            ancestors.push(next);
        }
    }
}

fn trim_matches(rows: &mut Vec<Row>) {
    if rows.len() >= MATCH_LIMIT * 2 {
        sort_rows(rows, false);
        rows.truncate(MATCH_LIMIT);
    }
}

fn matches_query(path: &Path, query: &str) -> bool {
    path.to_string_lossy().to_lowercase().contains(query)
}

pub fn sort_rows(rows: &mut [Row], largest_file: bool) {
    let size = |row: &Row| {
        if largest_file {
            row.usage.largest_file_bytes
        } else {
            row.usage.size()
        }
    };
    rows.sort_unstable_by(|a, b| size(b).cmp(&size(a)).then_with(|| a.path.cmp(&b.path)));
}

pub fn safe_dir(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    // Check each parent before resolving a child through that parent.
    let ancestors: Vec<_> = path.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        let metadata = fs::symlink_metadata(ancestor).with_context(|| {
            Message::new(
                format!("Cannot access {ancestor:?}"),
                format!("フォルダにアクセスできません: {}", display(ancestor)),
            )
        })?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(Message::new(
                format!("Expected a directory without symlinks or reparse points: {ancestor:?}"),
                format!(
                    "通常のフォルダとして確認できません。リンクや種類を確認してください: {}",
                    display(ancestor)
                ),
            )
            .into());
        }
    }
    Ok(fs::canonicalize(path)?)
}

pub(super) fn home_scan_dir(home: &Path, path: &Path) -> Result<PathBuf> {
    let outside_home = || {
        Message::new(
            "Choose a folder inside your home directory",
            "ホームフォルダ内のフォルダを選んでください。",
        )
    };
    #[cfg(unix)]
    {
        let absolute = std::path::absolute(path)?;
        let relative = absolute.strip_prefix(home).map_err(|_| outside_home())?;
        if relative
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(outside_home().into());
        }
    }
    let resolved = safe_dir(path)?;
    if !resolved.starts_with(home) {
        return Err(outside_home().into());
    }
    Ok(resolved)
}

pub(super) fn workspace_dir(path: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let dirs = directories::BaseDirs::new().context("Could not locate your home directory")?;
        home_scan_dir(&dirs.home_dir().canonicalize()?, path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let root = fs::canonicalize(path)?;
        if !root.is_dir() {
            bail!("Expected a directory for the scan root: {root:?}");
        }
        Ok(root)
    }
}

pub(super) fn safe_workspace_dir(path: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return workspace_dir(path);
    #[cfg(not(target_os = "macos"))]
    safe_dir(path)
}

#[cfg(all(test, unix))]
#[test]
fn home_boundary_rejects_outside_roots_parent_traversal_and_links() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let fixture_path = fixture.path().canonicalize()?;
    let home = fixture_path.join("home");
    let project = home.join("project");
    fs::create_dir_all(&project)?;
    assert_eq!(home_scan_dir(&home, &home)?, home);
    assert_eq!(home_scan_dir(&home, &project)?, project);
    for outside in [
        PathBuf::from("/"),
        PathBuf::from("/tmp"),
        PathBuf::from("/nix/store"),
        fixture_path.join("home-other"),
        home.join("../home-other"),
    ] {
        assert!(home_scan_dir(&home, &outside)
            .unwrap_err()
            .to_string()
            .contains("inside your home directory"));
    }
    let outside_dir = fixture_path.join("outside");
    fs::create_dir_all(outside_dir.join("child"))?;
    std::os::unix::fs::symlink(outside_dir, home.join("link"))?;
    assert!(home_scan_dir(&home, &home.join("link/child")).is_err());
    Ok(())
}

pub fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Junctions, mounted directories and cloud reparse points also stop traversal.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.is_symlink()
}

pub fn same_device(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

#[cfg(unix)]
fn allocated_bytes(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.blocks().saturating_mul(512))
}

#[cfg(not(unix))]
fn allocated_bytes(_: &Metadata) -> Option<u64> {
    None
}

pub fn display(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|ch| {
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ch.escape_default().to_string().chars().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn serialize_path<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&display(path))
}

#[cfg(test)]
#[test]
fn scan_progress_finishes_with_the_inventory_counts() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    fs::create_dir(root.join("files"))?;
    fs::create_dir(root.join("empty"))?;
    for index in 0..257 {
        fs::write(root.join("files").join(format!("{index}.bin")), [0; 17])?;
    }
    let progress = ScanProgress::default();
    let inventory = scan_with_progress(&root, 4, None, Some(&progress))?;
    let observed = progress.snapshot();
    let total = &inventory
        .directories
        .iter()
        .find(|row| row.path == root)
        .unwrap()
        .usage;
    assert!(observed.inventory_done);
    assert_eq!(observed.files, 257);
    assert_eq!(observed.files, total.files);
    assert_eq!(observed.logical_bytes, total.logical_bytes);
    assert_eq!(observed.directories, inventory.directories.len() as u64);
    assert!(observed
        .current_directory
        .as_ref()
        .is_some_and(|path| path.starts_with(&root)));

    scan_with_progress(&root.join("empty"), 2, None, Some(&progress))?;
    let empty = progress.snapshot();
    assert!(empty.inventory_done);
    assert_eq!(
        (empty.files, empty.directories, empty.logical_bytes),
        (0, 1, 0)
    );
    Ok(())
}

#[cfg(test)]
#[test]
fn scan_keeps_item_dates_separate_from_the_recent_file_count() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let folder = root.join("dated-folder");
    fs::create_dir(&folder)?;
    let recent = folder.join("dated-recent.txt");
    let old = folder.join("dated-old.txt");
    fs::write(&recent, b"recent")?;
    fs::write(&old, b"old")?;
    fs::File::options()
        .write(true)
        .open(&old)?
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(9 * 86_400))?;
    let modified = |path: &Path| -> Result<chrono::DateTime<chrono::Utc>> {
        Ok(fs::metadata(path)?.modified()?.into())
    };
    let inventory = scan(&root, 4, Some("dated"))?;
    for path in [&folder, &recent, &old] {
        let row = inventory
            .matches
            .iter()
            .find(|row| &row.path == path)
            .unwrap();
        assert_eq!(row.usage.modified_at, Some(modified(path)?));
    }
    for path in [&root, &folder] {
        let row = inventory
            .directories
            .iter()
            .find(|row| &row.path == path)
            .unwrap();
        assert_eq!(row.usage.modified_at, Some(modified(path)?));
        assert_eq!(row.usage.files_modified_last_7_days, 1);
    }
    Ok(())
}
