use super::{bytes, inventory};
use crate::cli::{ListFormat, StaleArgs, StaleSort};
use anyhow::{bail, Context, Result};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub(super) struct Candidate {
    pub path: String,
    #[cfg(all(feature = "desktop", target_os = "macos"))]
    #[serde(skip)]
    pub native_path: PathBuf,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub accessed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
    pub logical_bytes: u64,
    pub open_state: &'static str,
}

#[derive(Serialize)]
pub(super) struct Report {
    pub root: String,
    pub candidates: Vec<Candidate>,
    #[serde(skip)]
    alternate_orders: [Vec<Candidate>; 3],
    #[serde(skip)]
    order: StaleSort,
    pub omitted: usize,
    pub skipped: usize,
    pub unknown_age: usize,
}

impl Report {
    pub(super) fn set_order(&mut self, order: StaleSort) {
        if order != self.order {
            self.alternate_orders[self.order as usize] = std::mem::take(&mut self.candidates);
            self.candidates = std::mem::take(&mut self.alternate_orders[order as usize]);
            self.order = order;
        }
    }
}

const ORDERS: [StaleSort; 3] = [StaleSort::Size, StaleSort::Modified, StaleSort::Created];

// The heap evicts its greatest entry. Unknown dates sort after all known dates.
type Match = (
    (bool, Option<SystemTime>),
    Reverse<u64>,
    SystemTime,
    PathBuf,
    Dates,
);

impl StaleSort {
    fn key(self, dates: &Dates) -> (bool, Option<SystemTime>) {
        let date = match self {
            Self::Size => return (false, None),
            Self::Modified => dates.modified,
            Self::Created => dates.created,
        };
        (date.is_none(), date)
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Dates {
    modified: Option<SystemTime>,
    accessed: Option<SystemTime>,
    created: Option<SystemTime>,
}

impl Dates {
    fn last_activity(&self) -> Option<SystemTime> {
        [self.modified, self.accessed, self.created]
            .into_iter()
            .flatten()
            .max()
    }
}

fn meaningful_date(value: std::io::Result<SystemTime>) -> Option<SystemTime> {
    // Reproducible archives can normalize timestamps to the epoch or one
    // second after it. Neither value is evidence of recent or old use.
    value
        .ok()
        .filter(|time| *time >= UNIX_EPOCH + Duration::from_secs(86_400))
}

#[derive(Default)]
struct Matches {
    ranked: [BinaryHeap<Match>; 3],
    total: usize,
    skipped: usize,
    unknown_age: usize,
}

pub(super) fn scan(
    root: Option<&Path>,
    older_than_days: u16,
    min_mib: u64,
    sort: StaleSort,
) -> Result<Report> {
    scan_scope(root, older_than_days, min_mib, sort, false)
}

#[cfg(feature = "desktop")]
pub(super) fn scan_personal(
    root: Option<&Path>,
    days: u16,
    min_mib: u64,
    sort: StaleSort,
) -> Result<Report> {
    scan_scope(root, days, min_mib, sort, true)
}

fn scan_scope(
    root: Option<&Path>,
    older_than_days: u16,
    min_mib: u64,
    sort: StaleSort,
    personal: bool,
) -> Result<Report> {
    let home = directories::BaseDirs::new().context("Could not locate your home directory")?;
    let home = home.home_dir().canonicalize()?;
    let cutoff = SystemTime::now() - Duration::from_secs(u64::from(older_than_days) * 86_400);
    scan_under(
        &home,
        root.unwrap_or(&home),
        cutoff,
        min_mib,
        sort,
        personal,
    )
}

fn scan_under(
    home: &Path,
    root: &Path,
    cutoff: SystemTime,
    min_mib: u64,
    sort: StaleSort,
    personal: bool,
) -> Result<Report> {
    let root = inventory::home_scan_dir(home, root)?;
    if root.strip_prefix(home)?.iter().any(excluded) {
        bail!("Choose a folder under your home directory outside hidden or Library directories");
    }
    if personal
        && root
            .ancestors()
            .take_while(|p| *p != home)
            .any(build_output)
    {
        bail!("Choose a personal folder. Inspect generated output in Workspace.");
    }
    let minimum = min_mib
        .checked_mul(1024 * 1024)
        .context("Minimum size is too large")?;
    let matches = Mutex::new(Matches::default());
    let mut walker = WalkBuilder::new(&root);
    let scan_root = root.clone();
    walker
        .threads(std::thread::available_parallelism().map_or(1, |count| count.get().min(4)))
        .standard_filters(false)
        .follow_links(false)
        .same_file_system(true)
        .filter_entry(move |entry| {
            entry.path() == scan_root
                || (!excluded(entry.file_name())
                    && !(personal
                        && entry.file_type().is_some_and(|kind| kind.is_dir())
                        && build_output(entry.path())))
        });
    walker.build_parallel().run(|| {
        let matches = &matches;
        Box::new(move |item| {
            let Ok(entry) = item else {
                matches.lock().unwrap().skipped += 1;
                return WalkState::Continue;
            };
            #[cfg(windows)]
            if entry.file_type().is_some_and(|kind| kind.is_dir())
                && fs::symlink_metadata(entry.path()).is_ok_and(|meta| inventory::is_link(&meta))
            {
                return WalkState::Skip;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                return WalkState::Continue;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                matches.lock().unwrap().skipped += 1;
                return WalkState::Continue;
            };
            if !metadata.is_file()
                || inventory::is_link(&metadata)
                || metadata.len() < minimum
                || super::artifacts::protected(entry.path())
            {
                return WalkState::Continue;
            }
            let dates = Dates {
                modified: meaningful_date(metadata.modified()),
                accessed: meaningful_date(metadata.accessed()),
                created: meaningful_date(metadata.created()),
            };
            let Some(activity) = dates.last_activity() else {
                matches.lock().unwrap().unknown_age += 1;
                return WalkState::Continue;
            };
            if activity <= cutoff {
                let mut result = matches.lock().unwrap();
                result.total += 1;
                for (heap, order) in result.ranked.iter_mut().zip(ORDERS) {
                    let key = order.key(&dates);
                    let row = (
                        key,
                        Reverse(metadata.len()),
                        activity,
                        entry.path().to_owned(),
                        dates,
                    );
                    if heap.len() < 100 {
                        heap.push(row);
                    } else if let Some(mut worst) = heap.peek_mut() {
                        if row < *worst {
                            *worst = row;
                        }
                    }
                }
            }
            WalkState::Continue
        })
    });
    let Matches {
        ranked,
        total,
        skipped,
        unknown_age,
    } = matches.into_inner().unwrap();
    let ranked = ranked.map(BinaryHeap::into_sorted_vec);
    let open_states: HashMap<_, _> = ranked[sort as usize]
        .iter()
        .take(20)
        .map(|(_, _, _, path, _)| (path.clone(), open_state(path.clone())))
        .collect();
    let mut alternate_orders = ranked.map(|rows| {
        rows.into_iter()
            .map(|(_, Reverse(size), activity, path, dates)| Candidate {
                path: inventory::display(&path),
                created_at: dates.created.map(Into::into),
                modified_at: dates.modified.map(Into::into),
                accessed_at: dates.accessed.map(Into::into),
                last_activity_at: activity.into(),
                logical_bytes: size,
                open_state: open_states.get(&path).copied().unwrap_or("not_checked"),
                #[cfg(all(feature = "desktop", target_os = "macos"))]
                native_path: path,
            })
            .collect()
    });
    let mut report = Report {
        root: inventory::display(&root),
        omitted: total.saturating_sub(100),
        candidates: std::mem::take(&mut alternate_orders[StaleSort::Size as usize]),
        alternate_orders,
        order: StaleSort::Size,
        skipped,
        unknown_age,
    };
    report.set_order(sort);
    Ok(report)
}

pub fn run(args: &StaleArgs) -> Result<()> {
    let report = scan(
        args.root.as_deref(),
        args.older_than_days,
        args.min_mib,
        args.sort,
    )?;
    match args.format {
        ListFormat::Json => println!("{}", serde_json::to_string(&report)?),
        ListFormat::Text => {
            println!(
                "{} · {} candidates by latest recorded activity (review only)",
                report.root,
                report.candidates.len() + report.omitted
            );
            println!("        Size  Activity    Modified    Created     Last access  Open check          Path");
            for row in &report.candidates {
                println!(
                    "{:>12}  {}  {:10}  {:10}  {:11}  {:18}  {}",
                    bytes(row.logical_bytes),
                    date(Some(row.last_activity_at)),
                    date(row.modified_at),
                    date(row.created_at),
                    date(row.accessed_at),
                    row.open_state,
                    row.path
                );
            }
            println!(
                "{} shown; {} with unknown dates excluded; {} unread entries",
                report.candidates.len(),
                report.unknown_age,
                report.skipped
            );
        }
    }
    Ok(())
}

pub(super) fn date(time: Option<chrono::DateTime<chrono::Utc>>) -> String {
    time.map_or_else(
        || "—".into(),
        |time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        },
    )
}

// Generic names such as target and dist also occur in personal folders.
fn build_output(dir: &Path) -> bool {
    let Some(parent) = dir.parent() else {
        return false;
    };
    match dir.file_name().and_then(OsStr::to_str) {
        Some("target") => {
            parent.join("Cargo.toml").is_file()
                || (dir.join("CACHEDIR.TAG").is_file() && dir.join(".rustc_info.json").is_file())
        }
        Some("dist" | "build") => {
            parent.join("package.json").is_file()
                || parent.join("CMakeLists.txt").is_file()
                || parent.join("build.gradle").is_file()
                || parent.join("build.gradle.kts").is_file()
        }
        Some("__pycache__") => true,
        _ => false,
    }
}

fn excluded(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
        || name.eq_ignore_ascii_case("Library")
        || name == "node_modules"
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_state(path: PathBuf) -> &'static str {
    use std::process::{Command, Stdio};
    let program = if cfg!(target_os = "macos") {
        "/usr/sbin/lsof"
    } else {
        "lsof"
    };
    match Command::new(program)
        .args(["-nP", "-t", "--"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if !output.stdout.is_empty() => "currently_open",
        Ok(output) if output.status.code() == Some(1) && output.stderr.is_empty() => {
            "not_seen_open"
        }
        _ => "unknown",
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn open_state(_: PathBuf) -> &'static str {
    "not_checked"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_scan_skips_confirmed_project_outputs_but_keeps_other_targets() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?;
        for path in ["docs/target", "crate/target", "web/dist"] {
            fs::create_dir_all(home.join(path))?;
            fs::write(home.join(path).join("old.bin"), b"data")?;
        }
        fs::write(home.join("crate/Cargo.toml"), b"")?;
        fs::write(home.join("web/package.json"), b"{}")?;
        let cutoff = SystemTime::now() + Duration::from_secs(60);
        let report = scan_under(&home, &home, cutoff, 0, StaleSort::Size, true)?;
        let paths: Vec<_> = report.candidates.iter().map(|c| c.path.as_str()).collect();
        assert!(paths
            .iter()
            .any(|p| p.ends_with("docs/target/old.bin")
                || p.contains("docs") && p.ends_with("old.bin")));
        assert!(!paths
            .iter()
            .any(|p| p.ends_with("old.bin") && !p.contains("docs")));
        assert!(scan_under(
            &home,
            &home.join("crate/target"),
            cutoff,
            0,
            StaleSort::Size,
            true
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn date_sort_includes_matches_outside_the_largest_hundred() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?;
        for index in 0..101 {
            let file = fs::File::create(home.join(format!("file-{index}")))?;
            file.set_len(index + 1)?;
            file.set_modified(UNIX_EPOCH + Duration::from_secs(86_400 * (index + 2)))?;
        }
        let mut report = scan_under(
            &home,
            &home,
            SystemTime::now() + Duration::from_secs(60),
            0,
            StaleSort::Size,
            false,
        )?;
        assert_eq!(report.omitted, 1);
        assert_eq!(report.candidates[0].logical_bytes, 101);
        report.set_order(StaleSort::Modified);
        assert_eq!(report.candidates[0].logical_bytes, 1);
        report.set_order(StaleSort::Created);
        assert_eq!(report.candidates.len(), 100);
        let dates: Vec<_> = report
            .candidates
            .iter()
            .filter_map(|row| row.created_at)
            .collect();
        assert!(dates.windows(2).all(|pair| pair[0] <= pair[1]));
        report.set_order(StaleSort::Size);
        assert_eq!(report.candidates[0].logical_bytes, 101);
        Ok(())
    }

    #[test]
    fn scans_only_old_visible_files_in_scope_and_keeps_largest_first() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?.join("home");
        for dir in ["visible", "Library", ".hidden", "node_modules"] {
            fs::create_dir_all(home.join(dir))?;
        }
        let old = SystemTime::now() - Duration::from_secs(180 * 86_400);
        let cutoff = SystemTime::now() + Duration::from_secs(600);
        let new = cutoff + Duration::from_secs(600);
        for (name, size, timestamp) in [
            ("visible/small.bin", 2, old),
            ("largest.bin", 3, old),
            ("new.bin", 4, new),
            ("recently-opened.bin", 4, old),
            ("Library/old.bin", 4, old),
            (".hidden/old.bin", 4, old),
            ("node_modules/old.bin", 4, old),
            ("secret.pem", 4, old),
        ] {
            let file = fs::File::create(home.join(name))?;
            file.set_len(size * 1024 * 1024)?;
            file.set_modified(timestamp)?;
            if name == "recently-opened.bin" {
                file.set_times(fs::FileTimes::new().set_accessed(new))?;
            }
        }
        let report = scan_under(&home, &home, cutoff, 1, StaleSort::Size, false)?;
        let sizes: Vec<_> = report.candidates.iter().map(|r| r.logical_bytes).collect();
        assert_eq!(sizes, [3 * 1024 * 1024, 2 * 1024 * 1024]);
        assert!(scan_under(&home, tmp.path(), cutoff, 1, StaleSort::Size, false).is_err());
        assert!(scan_under(
            &home,
            &home.join("Library"),
            cutoff,
            1,
            StaleSort::Size,
            false
        )
        .is_err());
        assert_eq!(meaningful_date(Ok(UNIX_EPOCH)), None);
        assert_eq!(
            meaningful_date(Ok(UNIX_EPOCH + Duration::from_secs(1))),
            None
        );
        Ok(())
    }
}
