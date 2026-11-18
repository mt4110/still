use super::message::Message;
use anyhow::{bail, Context, Result};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Default, Serialize)]
pub struct Usage {
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub largest_file_bytes: u64,
    pub files: u64,
    pub skipped: u64,
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
    }

    fn file(metadata: &Metadata) -> Self {
        Self {
            logical_bytes: metadata.len(),
            allocated_bytes: allocated_bytes(metadata),
            largest_file_bytes: metadata.len(),
            files: 1,
            skipped: 0,
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
    directories: HashMap<PathBuf, Usage>,
    matches: Vec<Row>,
    match_count: usize,
    skipped: u64,
    cargo_marker_dirs: HashSet<PathBuf>,
    #[cfg(unix)]
    hardlinks: HashMap<(u64, u64), (PathBuf, u64)>,
}

const MATCH_LIMIT: usize = 1000;

pub fn scan(root: &Path, threads: u16, query: Option<&str>) -> Result<Inventory> {
    let root_metadata = fs::symlink_metadata(root)?;
    if !root_metadata.is_dir() || is_link(&root_metadata) {
        bail!("Scan root must be a directory: {root:?}");
    }

    let mut merged = Batch::default();
    merged.directories.insert(root.to_owned(), Usage::default());
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
        };
        let query = query.as_deref();
        let root_metadata = &root_metadata;
        Box::new(move |entry| worker.visit(entry, root, root_metadata, query))
    });

    let Batch {
        mut directories,
        mut matches,
        match_count,
        skipped,
        cargo_marker_dirs,
        #[cfg(unix)]
        hardlinks,
    } = result.into_inner().expect("walker results");
    directories.entry(root.to_owned()).or_default().skipped += skipped;
    #[cfg(unix)]
    for (path, blocks) in hardlinks.into_values() {
        if let Some(parent) = path.parent() {
            let usage = directories.entry(parent.to_owned()).or_default();
            usage.allocated_bytes = Some(usage.allocated_bytes.unwrap_or(0).saturating_add(blocks));
        }
    }

    // Parents precede descendants in path order. Fold each subtree into its parent once.
    let mut paths: Vec<_> = directories.keys().cloned().collect();
    paths.sort_unstable();
    for path in paths.iter().rev() {
        if path != root {
            let usage = directories[path].clone();
            if let Some(parent) = path.parent() {
                directories
                    .entry(parent.to_owned())
                    .or_default()
                    .add(&usage);
            }
        }
    }

    if let Some(query) = query.as_deref() {
        for path in &paths {
            if matches_query(path, query) {
                match_count_dir(&mut matches, path, &directories[path]);
            }
        }
    }
    sort_rows(&mut matches, false);
    let directory_matches = query.as_ref().map_or(0, |query| {
        paths.iter().filter(|p| matches_query(p, query)).count()
    });
    matches.truncate(MATCH_LIMIT);

    Ok(Inventory {
        directories: paths
            .into_iter()
            .map(|path| Row {
                usage: directories.remove(&path).expect("known directory"),
                path,
                is_dir: true,
            })
            .collect(),
        omitted_matches: (match_count + directory_matches).saturating_sub(matches.len()),
        cargo_marker_dirs,
        matches,
    })
}

struct Worker<'a> {
    results: &'a Mutex<Batch>,
    batch: Batch,
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
            self.batch.skipped += 1;
            return WalkState::Continue;
        };
        let path = entry.path();
        if path == root {
            return WalkState::Continue;
        }

        let parent = path.parent().expect("walker child has a parent");
        let usage = self.batch.directories.entry(parent.to_owned()).or_default();
        let Ok(metadata) = fs::symlink_metadata(path) else {
            usage.skipped += 1;
            return WalkState::Skip;
        };
        if is_link(&metadata) || !same_device(root_metadata, &metadata) {
            usage.skipped += 1;
            return WalkState::Skip;
        } else if metadata.is_dir() {
            self.batch.directories.entry(path.to_owned()).or_default();
        } else if metadata.is_file() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".rustc_info.json" | "CACHEDIR.TAG")
            ) {
                self.batch.cargo_marker_dirs.insert(parent.to_owned());
            }
            let stats = Usage::file(&metadata);
            let counted = stats.clone();
            #[cfg(unix)]
            let counted = {
                use std::os::unix::fs::MetadataExt;
                let mut counted = counted;
                if metadata.nlink() > 1 {
                    let blocks = counted.allocated_bytes.replace(0).unwrap_or(0);
                    remember_hardlink(
                        &mut self.batch.hardlinks,
                        (metadata.dev(), metadata.ino()),
                        path.to_owned(),
                        blocks,
                    );
                }
                counted
            };
            usage.add(&counted);
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
            usage.skipped += 1;
        }
        WalkState::Continue
    }
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        let mut results = self.results.lock().expect("walker results");
        for (path, usage) in self.batch.directories.drain() {
            results.directories.entry(path).or_default().add(&usage);
        }
        results.match_count += self.batch.match_count;
        results.skipped += self.batch.skipped;
        results
            .cargo_marker_dirs
            .extend(self.batch.cargo_marker_dirs.drain());
        results.matches.append(&mut self.batch.matches);
        #[cfg(unix)]
        for (identity, (path, blocks)) in self.batch.hardlinks.drain() {
            remember_hardlink(&mut results.hardlinks, identity, path, blocks);
        }
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

fn match_count_dir(matches: &mut Vec<Row>, path: &Path, usage: &Usage) {
    matches.push(Row {
        path: path.to_owned(),
        is_dir: true,
        usage: usage.clone(),
    });
    trim_matches(matches);
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
