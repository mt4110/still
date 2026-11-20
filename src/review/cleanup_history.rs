use crate::cli::{CleanupHistoryArgs, ListFormat};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
#[cfg(any(
    test,
    all(feature = "desktop", any(target_os = "macos", target_os = "windows"))
))]
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Item {
    pub path: PathBuf,
    pub producer: String,
    pub logical_bytes: u64,
    /// Allocated size from the earlier workspace scan, not a deletion-time measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_allocated_bytes: Option<u64>,
    pub status: String,
    pub reason: Option<String>,
    pub original_path_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Record {
    pub version: u8,
    pub root: PathBuf,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub available_before: Option<u64>,
    pub available_after: Option<u64>,
    /// Workspace keep rules observed at the start; absent means not observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_paths_at_start: Option<Vec<PathBuf>>,
    pub items: Vec<Item>,
    /// Non-filesystem operations use their own typed targets rather than fake paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Operation {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u16>,
    pub targets: Vec<OperationTarget>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OperationTarget {
    pub identifier: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_image_bytes: Option<u64>,
}

#[cfg(all(not(test), feature = "desktop", target_os = "macos"))]
pub(super) fn save_operation(
    started_at: DateTime<Utc>,
    before: Option<u64>,
    after: Option<u64>,
    operation: Operation,
) -> Result<()> {
    let home = directories::BaseDirs::new()
        .context("Could not locate home volume")?
        .home_dir()
        .to_owned();
    save(&Record {
        version: 1,
        root: home,
        started_at,
        completed_at: Utc::now(),
        available_before: before,
        available_after: after,
        kept_paths_at_start: None,
        items: Vec::new(),
        operation: Some(operation),
    })
}

fn directory() -> Result<PathBuf> {
    let dirs = crate::storage_paths::project_dirs()
        .context("Could not determine the still data directory")?;
    Ok(dirs.data_local_dir().join("cleanup-history"))
}

#[cfg(all(
    not(test),
    feature = "desktop",
    any(target_os = "macos", target_os = "windows")
))]
pub(super) fn save(record: &Record) -> Result<()> {
    save_in(&directory()?, record)
}

#[cfg(any(
    test,
    all(feature = "desktop", any(target_os = "macos", target_os = "windows"))
))]
fn save_in(directory: &Path, record: &Record) -> Result<()> {
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    let name = format!(
        "{}-{}.json",
        record.completed_at.format("%Y%m%dT%H%M%S%.fZ"),
        uuid::Uuid::new_v4()
    );
    file.persist_noclobber(directory.join(name))
        .context("Could not save cleanup history")?;
    Ok(())
}

fn newest_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|ext| ext == "json")
        {
            paths.push(entry.path());
        }
    }
    paths.sort_unstable_by(|a, b| b.cmp(a));
    Ok(paths)
}

fn load_from(directory: &Path, limit: usize) -> Result<Vec<Record>> {
    newest_paths(directory)?
        .into_iter()
        .take(limit)
        .map(|path| {
            let file = fs::File::open(&path)?;
            let record: Record = serde_json::from_reader(BufReader::new(file))
                .with_context(|| format!("Could not read cleanup history {path:?}"))?;
            anyhow::ensure!(
                record.version == 1,
                "Unsupported cleanup history version in {path:?}"
            );
            Ok(record)
        })
        .collect()
}

/// Read just the volume measurements. Older history files can contain thousands
/// of individual outcomes, which the overview does not need to allocate.
#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(super) fn last_measurement(root: &Path) -> Result<Option<(u64, u64)>> {
    last_measurement_in(&directory()?, root)
}

#[cfg(any(test, all(feature = "desktop", target_os = "macos")))]
fn last_measurement_in(directory: &Path, root: &Path) -> Result<Option<(u64, u64)>> {
    #[derive(Deserialize)]
    struct Measurement {
        version: u8,
        root: PathBuf,
        available_before: Option<u64>,
        available_after: Option<u64>,
        #[serde(default)]
        operation: Option<serde_json::Value>,
    }
    for path in newest_paths(directory)? {
        let measurement: Measurement =
            serde_json::from_reader(BufReader::new(fs::File::open(path)?))?;
        if measurement.root == root && measurement.operation.is_none() {
            anyhow::ensure!(
                measurement.version == 1,
                "Unsupported cleanup history version"
            );
            return Ok(measurement
                .available_before
                .zip(measurement.available_after));
        }
    }
    Ok(None)
}

pub fn run(args: &CleanupHistoryArgs) -> Result<()> {
    let records = load_from(&directory()?, usize::from(args.limit))?;
    match args.format {
        ListFormat::Json => println!("{}", serde_json::to_string_pretty(&records)?),
        ListFormat::Text => {
            if records.is_empty() {
                println!("No recorded cleanup operations.");
            }
            for record in records {
                println!("{} · {}", record.completed_at, record.root.display());
                println!(
                    "  Free bytes: {:?} → {:?} (whole-volume observation)",
                    record.available_before, record.available_after
                );
                if let Some(paths) = &record.kept_paths_at_start {
                    println!("  Keep rules at start: {}", paths.len());
                    for path in paths {
                        println!("    {}", path.display());
                    }
                }
                for item in record.items {
                    println!(
                        "  {} · {} · {} logical bytes · {}",
                        item.status,
                        item.path.display(),
                        item.logical_bytes,
                        item.producer
                    );
                    if let Some(reason) = item.reason {
                        println!("    {reason}");
                    }
                    println!("    Original path exists: {:?}", item.original_path_exists);
                    if item.original_path_exists == Some(true) && item.status == "removed" {
                        println!("    The path may have been recreated after removal.");
                    }
                    if let Some(bytes) = item.scanned_allocated_bytes {
                        println!("    Allocated bytes from earlier scan: {bytes}");
                    }
                    if let Some(time) = item.original_path_checked_at {
                        println!("    Original path checked at: {time}");
                    }
                }
                if let Some(operation) = record.operation {
                    println!("  Operation: {}", operation.kind);
                    if let Some(days) = operation.retention_days {
                        println!("  Retention: {days} days");
                    }
                    for target in operation.targets {
                        println!("  {} · {}", target.status, target.identifier);
                        if let Some(reason) = target.reason {
                            println!("    {reason}");
                        }
                        if let Some(bytes) = target.remaining_image_bytes {
                            println!("    Remaining image bytes: {bytes}");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_round_trip_and_limit_results() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let record = Record {
            version: 1,
            root: "/tmp/project".into(),
            started_at: Utc::now(),
            completed_at: Utc::now(),
            available_before: Some(10),
            available_after: Some(20),
            kept_paths_at_start: Some(vec!["/tmp/project/keep".into()]),
            items: vec![Item {
                path: "/tmp/project/target".into(),
                producer: "Cargo".into(),
                logical_bytes: 42,
                scanned_allocated_bytes: Some(32),
                status: "incomplete".into(),
                reason: Some("could not verify".into()),
                original_path_exists: None,
                original_path_checked_at: Some(Utc::now()),
            }],
            operation: None,
        };
        save_in(dir.path(), &record)?;
        assert_eq!(
            last_measurement_in(dir.path(), &record.root)?,
            Some((10, 20))
        );
        assert!(last_measurement_in(dir.path(), Path::new("/other"))?.is_none());
        let records = load_from(dir.path(), 1)?;
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].items[0].reason.as_deref(),
            Some("could not verify")
        );
        assert_eq!(records[0].items[0].original_path_exists, None);
        assert_eq!(records[0].items[0].scanned_allocated_bytes, Some(32));
        assert_eq!(load_from(dir.path(), 0)?.len(), 0);
        Ok(())
    }

    #[test]
    fn reads_history_written_before_optional_observation_fields() -> Result<()> {
        let record: Record = serde_json::from_str(
            r#"{
            "version":1,"root":"/tmp/project",
            "started_at":"2026-01-01T00:00:00Z",
            "completed_at":"2026-01-01T00:01:00Z",
            "available_before":1,"available_after":2,
            "items":[{"path":"/tmp/project/target","producer":"Cargo",
              "logical_bytes":42,"status":"removed","reason":null,
              "original_path_exists":false}]
        }"#,
        )?;
        assert_eq!(record.items[0].scanned_allocated_bytes, None);
        assert_eq!(record.items[0].original_path_checked_at, None);
        assert!(record.kept_paths_at_start.is_none());
        assert!(record.operation.is_none());
        Ok(())
    }

    #[test]
    fn operation_history_round_trips_without_confusing_workspace_measurements() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let home = PathBuf::from("/tmp/fixture-home");
        let record = Record {
            version: 1,
            root: home.clone(),
            started_at: Utc::now(),
            completed_at: Utc::now(),
            available_before: Some(10),
            available_after: Some(20),
            kept_paths_at_start: None,
            items: Vec::new(),
            operation: Some(Operation {
                kind: "sdk_uninstall".into(),
                retention_days: None,
                targets: vec![OperationTarget {
                    identifier: "runtime".into(),
                    status: "partial".into(),
                    reason: Some("Runtime image remains".into()),
                    remaining_image_bytes: Some(100),
                }],
            }),
        };
        save_in(dir.path(), &record)?;
        let loaded = load_from(dir.path(), 1)?;
        let target = &loaded[0].operation.as_ref().unwrap().targets[0];
        assert_eq!(target.status, "partial");
        assert_eq!(target.remaining_image_bytes, Some(100));
        assert!(last_measurement_in(dir.path(), &home)?.is_none());
        Ok(())
    }
}
