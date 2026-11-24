//! Only comparable, complete observations get a size delta. Each explicit
//! desktop Library scan establishes the baseline for the next one.
use super::Report;
#[cfg(feature = "desktop")]
use anyhow::Context;
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u8,
    completed: DateTime<Utc>,
    // Library-relative labels contain the area as well as the directory name.
    allocated: BTreeMap<String, u64>,
    // Added without changing v1: older snapshots simply have no saved changes.
    #[serde(default)]
    growth: Vec<Growth>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::review) struct Growth {
    pub label: String,
    pub bytes: u64,
    pub previous: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(in crate::review) struct RecentGrowth {
    pub completed: DateTime<Utc>,
    pub rows: Vec<Growth>,
}

#[cfg(feature = "desktop")]
pub(in crate::review) fn recent_growth() -> Result<Option<RecentGrowth>> {
    let dirs = crate::storage_paths::project_dirs().context("Could not locate still data")?;
    recent_growth_in(dirs.data_local_dir())
}

fn recent_growth_in(directory: &Path) -> Result<Option<RecentGrowth>> {
    let file = match File::open(directory.join("library-observation.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let snapshot: Snapshot = serde_json::from_reader(BufReader::new(file))?;
    ensure!(snapshot.version == 1, "Unknown Library history format");
    Ok(Some(RecentGrowth {
        completed: snapshot.completed,
        rows: snapshot.growth,
    }))
}

#[cfg(feature = "desktop")]
pub(super) fn record(report: &mut Report) -> Result<()> {
    let dirs = crate::storage_paths::project_dirs().context("Could not locate still data")?;
    record_in(dirs.data_local_dir(), report)
}

fn record_in(directory: &Path, report: &mut Report) -> Result<()> {
    let path = directory.join("library-observation.json");
    let previous: Option<Snapshot> = match File::open(&path) {
        Ok(file) => {
            let previous: Snapshot = serde_json::from_reader(BufReader::new(file))?;
            ensure!(previous.version == 1, "Unknown Library history format");
            Some(previous)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut snapshot = Snapshot {
        version: 1,
        completed: Utc::now(),
        allocated: BTreeMap::new(),
        growth: Vec::new(),
    };
    for row in &mut report.rows {
        let Some(evidence) = &mut row.library else {
            continue;
        };
        let Some(bytes) = row.size_bytes else {
            continue;
        };
        // Labels displayed with replacement characters cannot identify paths uniquely.
        if row.partial || row.label.contains('\u{fffd}') {
            continue;
        }
        if let Some((prior, size)) = previous
            .as_ref()
            .and_then(|p| p.allocated.get(&row.label).map(|size| (p.completed, size)))
        {
            evidence.previous = Some((prior, i128::from(bytes) - i128::from(*size)));
            if bytes > *size {
                snapshot.growth.push(Growth {
                    label: row.label.clone(),
                    bytes: bytes - *size,
                    previous: prior,
                });
            }
        }
        snapshot.allocated.insert(row.label.clone(), bytes);
    }
    snapshot
        .growth
        .sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
    snapshot.growth.truncate(3);
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        serde_json::to_writer(&mut writer, &snapshot)?;
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist(&path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::app_storage::{
        library::{Activity, Area, Evidence},
        Row,
    };
    use std::ffi::OsStr;

    fn scan(bytes: u64, partial: bool) -> Report {
        let evidence = Evidence::new(Area::Cache, OsStr::new("test.app"), Activity::default());
        Report {
            rows: vec![Row {
                label: "Library/Caches/test.app".to_owned(),
                path: None,
                modified_at: None,
                #[cfg(feature = "desktop")]
                native_path: None,
                size_bytes: Some(bytes),
                size_basis: "allocated",
                partial,
                reason: evidence.describe(),
                library: Some(evidence),
            }],
            notes: vec![],
        }
    }

    #[test]
    fn compares_complete_scans_and_resets_after_a_partial_scan() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut first = scan(64, false);
        record_in(dir.path(), &mut first)?;
        assert!(first.rows[0].library.as_ref().unwrap().previous.is_none());
        assert!(recent_growth_in(dir.path())?.unwrap().rows.is_empty());
        let mut smaller = scan(16, false);
        record_in(dir.path(), &mut smaller)?;
        assert_eq!(
            smaller.rows[0]
                .library
                .as_ref()
                .unwrap()
                .previous
                .unwrap()
                .1,
            -48
        );
        let mut partial = scan(2, true);
        record_in(dir.path(), &mut partial)?;
        assert!(partial.rows[0].library.as_ref().unwrap().previous.is_none());
        assert!(recent_growth_in(dir.path())?.unwrap().rows.is_empty());
        let mut baseline = scan(16, false);
        record_in(dir.path(), &mut baseline)?;
        assert!(baseline.rows[0]
            .library
            .as_ref()
            .unwrap()
            .previous
            .is_none());
        let mut fresh = scan(32, false);
        record_in(dir.path(), &mut fresh)?;
        assert_eq!(
            fresh.rows[0].library.as_ref().unwrap().previous.unwrap().1,
            16
        );
        let latest = recent_growth_in(dir.path())?.unwrap();
        assert_eq!(latest.rows.len(), 1);
        assert_eq!(latest.rows[0].label, "Library/Caches/test.app");
        assert_eq!(latest.rows[0].bytes, 16);
        assert!(latest.rows[0].previous <= latest.completed);
        Ok(())
    }

    #[test]
    fn older_snapshots_are_read_without_inventing_a_difference() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("library-observation.json"),
            r#"{"version":1,"completed":"2026-01-01T00:00:00Z","allocated":{"Library/Caches/test.app":16}}"#,
        )?;
        assert!(recent_growth_in(dir.path())?.unwrap().rows.is_empty());
        let mut report = scan(32, false);
        record_in(dir.path(), &mut report)?;
        let latest = recent_growth_in(dir.path())?.unwrap();
        assert_eq!(latest.rows.len(), 1);
        assert_eq!(latest.rows[0].bytes, 16);
        Ok(())
    }

    #[test]
    fn saved_growth_is_limited_to_largest_comparable_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut initial = Report::default();
        let mut next = Report::default();
        for (index, increase) in [10, 30, 20, 40].into_iter().enumerate() {
            let mut old = scan(100, false).rows.pop().unwrap();
            let mut new = scan(100 + increase, false).rows.pop().unwrap();
            old.label = format!("Library/Caches/item{index}");
            new.label = old.label.clone();
            initial.rows.push(old);
            next.rows.push(new);
        }
        record_in(dir.path(), &mut initial)?;
        record_in(dir.path(), &mut next)?;
        let latest = recent_growth_in(dir.path())?.unwrap();
        assert_eq!(
            latest.rows.iter().map(|row| row.bytes).collect::<Vec<_>>(),
            [40, 30, 20]
        );
        Ok(())
    }
}
