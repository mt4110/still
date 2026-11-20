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
        }
        snapshot.allocated.insert(row.label.clone(), bytes);
    }
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
        Ok(())
    }
}
