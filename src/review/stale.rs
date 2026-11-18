use super::{bytes, inventory};
use crate::cli::{ListFormat, StaleArgs};
use anyhow::{bail, Context, Result};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub(super) struct Candidate {
    pub path: String,
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
    pub omitted: usize,
    pub skipped: usize,
    pub unknown_age: usize,
}

type Match = (Reverse<u64>, SystemTime, PathBuf, Dates);

#[derive(Eq, Ord, PartialEq, PartialOrd)]
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
    largest: BinaryHeap<Match>,
    total: usize,
    skipped: usize,
    unknown_age: usize,
}

pub(super) fn scan(root: Option<&Path>, older_than_days: u16, min_mib: u64) -> Result<Report> {
    let home = directories::BaseDirs::new().context("Could not locate your home directory")?;
    let home = home.home_dir().canonicalize()?;
    let cutoff = SystemTime::now() - Duration::from_secs(u64::from(older_than_days) * 86_400);
    scan_under(&home, root.unwrap_or(&home), cutoff, min_mib)
}

fn scan_under(home: &Path, root: &Path, cutoff: SystemTime, min_mib: u64) -> Result<Report> {
    let root = inventory::home_scan_dir(home, root)?;
    if root.strip_prefix(home)?.iter().any(excluded) {
        bail!("Choose a folder under your home directory outside hidden or Library directories");
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
        .filter_entry(move |entry| entry.path() == scan_root || !excluded(entry.file_name()));
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
                let row = (Reverse(metadata.len()), activity, entry.into_path(), dates);
                let mut result = matches.lock().unwrap();
                result.total += 1;
                result.largest.push(row);
                if result.largest.len() > 100 {
                    result.largest.pop();
                }
            }
            WalkState::Continue
        })
    });
    let Matches {
        largest,
        total,
        skipped,
        unknown_age,
    } = matches.into_inner().unwrap();
    let candidates: Vec<_> = largest
        .into_sorted_vec()
        .into_iter()
        .enumerate()
        .map(
            |(index, (Reverse(size), activity, path, dates))| Candidate {
                path: inventory::display(&path),
                modified_at: dates.modified.map(Into::into),
                accessed_at: dates.accessed.map(Into::into),
                last_activity_at: activity.into(),
                logical_bytes: size,
                open_state: if index < 20 {
                    open_state(path)
                } else {
                    "not_checked"
                },
            },
        )
        .collect();
    Ok(Report {
        root: inventory::display(&root),
        omitted: total - candidates.len(),
        candidates,
        skipped,
        unknown_age,
    })
}

pub fn run(args: &StaleArgs) -> Result<()> {
    let report = scan(args.root.as_deref(), args.older_than_days, args.min_mib)?;
    match args.format {
        ListFormat::Json => println!("{}", serde_json::to_string(&report)?),
        ListFormat::Text => {
            println!(
                "{} · {} candidates by latest recorded activity (review only)",
                report.root,
                report.candidates.len() + report.omitted
            );
            println!("        Size  Activity    Modified    Last access  Open check          Path");
            for row in &report.candidates {
                println!(
                    "{:>12}  {}  {:10}  {:11}  {:18}  {}",
                    bytes(row.logical_bytes),
                    date(Some(row.last_activity_at)),
                    date(row.modified_at),
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
        let report = scan_under(&home, &home, cutoff, 1)?;
        let sizes: Vec<_> = report.candidates.iter().map(|r| r.logical_bytes).collect();
        assert_eq!(sizes, [3 * 1024 * 1024, 2 * 1024 * 1024]);
        assert!(scan_under(&home, tmp.path(), cutoff, 1).is_err());
        assert!(scan_under(&home, &home.join("Library"), cutoff, 1).is_err());
        assert_eq!(meaningful_date(Ok(UNIX_EPOCH)), None);
        assert_eq!(
            meaningful_date(Ok(UNIX_EPOCH + Duration::from_secs(1))),
            None
        );
        Ok(())
    }
}
