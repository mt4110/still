//! Append-only, read-only Docker summary observations. Category totals are
//! kept separate: shared layers and VM storage make a summed total misleading.
use super::{actions, command_on, size_bytes};
use crate::review::app_storage::command_output;
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::Path;
use std::time::Duration;

const LINE_LIMIT: usize = 64 * 1024;
const CATEGORIES: [&str; 4] = ["Images", "Containers", "Local Volumes", "Build Cache"];

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::review) struct Observation {
    pub context: String,
    pub measured: DateTime<Utc>,
    pub prior: Option<DateTime<Utc>>,
    pub values: BTreeMap<String, u64>,
    pub changes: BTreeMap<String, i128>,
    identity: String,
    versions: String,
}

#[derive(Deserialize, Serialize)]
struct LatestCache {
    log_len: u64,
    observation: Observation,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SummaryRow {
    #[serde(rename = "Type")]
    category: String,
    size: String,
}

fn parse_summary(output: &str) -> Result<BTreeMap<String, u64>> {
    let mut rows = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let row: SummaryRow = serde_json::from_str(line)?;
        ensure!(
            CATEGORIES.contains(&row.category.as_str()),
            "Unknown Docker category"
        );
        let value = size_bytes(&row.size).context("Docker size unavailable")?;
        ensure!(
            rows.insert(row.category, value).is_none(),
            "Duplicate Docker category"
        );
    }
    ensure!(rows.len() == CATEGORIES.len(), "Incomplete Docker summary");
    Ok(rows)
}

fn versions(endpoint: &str) -> Result<String> {
    let value = command_output(
        command_on(endpoint).args([
            "version",
            "--format",
            "{{json .Client.Version}} {{json .Server.Version}} {{json .Server.APIVersion}}",
        ]),
        Duration::from_secs(10),
    )?;
    let parts = value
        .split_whitespace()
        .map(serde_json::from_str::<String>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        parts.len() == 3 && parts.iter().all(|part| !part.is_empty()),
        "Docker version unavailable"
    );
    let info = command_output(
        command_on(endpoint).args([
            "info",
            "--format",
            "{{json .Driver}}\n{{json .DockerRootDir}}",
        ]),
        Duration::from_secs(10),
    )?;
    let info = info
        .lines()
        .map(serde_json::from_str::<String>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        info.len() == 2 && info.iter().all(|part| !part.is_empty()),
        "Docker storage configuration unavailable"
    );
    // Keep configuration paths local but out of the observation history.
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("{parts:?}{info:?}").as_bytes())
    ))
}

pub(super) fn measure_and_record(endpoint: &str, context: &str, pin: &actions::Pin) -> Result<()> {
    let first_version = versions(endpoint)?;
    ensure!(
        &actions::capture(endpoint)? == pin,
        "Docker daemon changed before summary"
    );
    let measured = Utc::now();
    let output = command_output(
        command_on(endpoint).args(["system", "df", "--format", "{{json .}}"]),
        Duration::from_secs(30),
    )?;
    let values = parse_summary(&output)?;
    ensure!(
        &actions::capture(endpoint)? == pin && versions(endpoint)? == first_version,
        "Docker daemon or version changed during summary"
    );
    // A digest is sufficient for equality and avoids persisting the daemon ID or socket path.
    let identity = format!("{:x}", Sha256::digest(format!("{pin:?}").as_bytes()));
    let dirs = crate::storage_paths::project_dirs().context("Could not locate still data")?;
    let event = Observation {
        context: context.into(),
        measured,
        prior: None,
        values,
        changes: BTreeMap::new(),
        identity,
        versions: first_version,
    };
    append(dirs.data_local_dir(), event)
}

fn read_latest(file: &mut File, identity: Option<&str>) -> Result<Option<Observation>> {
    let mut latest = None;
    let mut reader = BufReader::new(file);
    loop {
        let mut line = Vec::new();
        let count = reader
            .by_ref()
            .take(LINE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        ensure!(
            count <= LINE_LIMIT && line.last() == Some(&b'\n'),
            "Invalid Docker observation record"
        );
        let item: Observation = serde_json::from_slice(&line)?;
        if identity.is_none_or(|id| item.identity == id)
            && latest
                .as_ref()
                .is_none_or(|current: &Observation| current.measured < item.measured)
        {
            latest = Some(item);
        }
    }
    Ok(latest)
}

fn append(directory: &Path, mut event: Observation) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::create_dir_all(directory)?;
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("docker-observations.jsonl"))?;
    ensure!(
        file.metadata()?.is_file(),
        "Docker history is not a regular file"
    );
    let mut lock = crate::review::filesystem::FileLock::try_acquire(file)
        .map_err(|error| anyhow::anyhow!("Docker history is busy: {error}"))?;
    let file = lock.file_mut();
    let previous = read_latest(file, Some(&event.identity))?;
    file.rewind()?;
    let last = read_latest(file, None)?;
    if let Some(previous) = &previous {
        if previous.measured < event.measured
            && previous.versions == event.versions
            && previous.values.keys().eq(event.values.keys())
        {
            event.prior = Some(previous.measured);
            event.changes = event
                .values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        i128::from(*value) - i128::from(previous.values[key]),
                    )
                })
                .collect();
        }
    }
    let mut line = serde_json::to_vec(&event)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_data()?;
    let log_len = file.metadata()?.len();
    // The append-only log is authoritative; the bounded latest cache keeps
    // rendering the home screen independent of the length of that log.
    let latest = last
        .filter(|value| value.measured > event.measured)
        .unwrap_or(event);
    let mut cache = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(
        cache.as_file_mut(),
        &LatestCache {
            log_len,
            observation: latest,
        },
    )?;
    cache.as_file().sync_all()?;
    cache.persist(directory.join("docker-observation-latest.json"))?;
    Ok(())
}

pub(in crate::review) fn latest() -> Result<Option<Observation>> {
    let dirs = crate::storage_paths::project_dirs().context("Could not locate still data")?;
    latest_in(dirs.data_local_dir())
}

fn latest_in(directory: &Path) -> Result<Option<Observation>> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = directory.join("docker-observation-latest.json");
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !directory.join("docker-observations.jsonl").exists(),
                "Docker observation cache is missing"
            );
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= LINE_LIMIT as u64,
        "Invalid Docker observation cache"
    );
    let mut data = String::new();
    file.read_to_string(&mut data)?;
    let cache: LatestCache = serde_json::from_str(&data)?;
    let log = fs::metadata(directory.join("docker-observations.jsonl"))?;
    ensure!(
        log.len() == cache.log_len,
        "Docker observation cache is out of date"
    );
    Ok(Some(cache.observation))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, version: &str, value: u64) -> Observation {
        Observation {
            context: "test".into(),
            measured: Utc::now(),
            prior: None,
            values: CATEGORIES
                .iter()
                .map(|name| ((*name).into(), value))
                .collect(),
            changes: BTreeMap::new(),
            identity: id.into(),
            versions: version.into(),
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_sizes() {
        let good = "{\"Type\":\"Images\",\"Size\":\"1GB\"}\n{\"Type\":\"Containers\",\"Size\":\"1MB\"}\n{\"Type\":\"Local Volumes\",\"Size\":\"0B\"}\n{\"Type\":\"Build Cache\",\"Size\":\"2MB\"}\n";
        assert_eq!(parse_summary(good).unwrap().len(), 4);
        assert!(parse_summary(good.replace("2MB", "N/A").as_str()).is_err());
        assert!(parse_summary(good.replace("Build Cache", "Other").as_str()).is_err());
        assert!(parse_summary(good.replace("Build Cache", "Images").as_str()).is_err());
        assert!(
            parse_summary(good.lines().take(3).collect::<Vec<_>>().join("\n").as_str()).is_err()
        );
    }

    #[test]
    fn records_only_comparable_daemon_and_version_without_discarding_history() -> Result<()> {
        let dir = tempfile::tempdir()?;
        append(dir.path(), event("daemon-a", "v1", 10))?;
        append(dir.path(), event("daemon-b", "v1", 99))?;
        append(dir.path(), event("daemon-a", "v1", 15))?;
        append(dir.path(), event("daemon-a", "v2", 25))?;
        let mut file = File::open(dir.path().join("docker-observations.jsonl"))?;
        let events = BufReader::new(&mut file)
            .lines()
            .map(|line| -> Result<Observation> { Ok(serde_json::from_str(&line?)?) })
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(events.len(), 4);
        assert!(events[0].prior.is_none());
        assert!(events[1].prior.is_none());
        assert!(events[2].prior.is_some());
        assert_eq!(events[2].changes["Images"], 5);
        assert!(events[3].prior.is_none());
        Ok(())
    }

    #[test]
    fn late_completion_does_not_replace_newer_observation_or_baseline() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let base = Utc::now();
        let mut newer = event("daemon", "v1", 20);
        newer.measured = base + chrono::TimeDelta::seconds(2);
        append(dir.path(), newer)?;
        let mut older = event("daemon", "v1", 10);
        older.measured = base + chrono::TimeDelta::seconds(1);
        append(dir.path(), older)?;
        let latest = latest_in(dir.path())?.unwrap();
        assert_eq!(latest.values["Images"], 20);
        let mut subsequent = event("daemon", "v1", 30);
        subsequent.measured = base + chrono::TimeDelta::seconds(3);
        append(dir.path(), subsequent)?;
        let latest = latest_in(dir.path())?.unwrap();
        assert_eq!(latest.changes["Images"], 10);
        assert_eq!(latest.prior, Some(base + chrono::TimeDelta::seconds(2)));
        Ok(())
    }

    #[test]
    fn stale_or_missing_cache_is_not_displayed_as_latest() -> Result<()> {
        let dir = tempfile::tempdir()?;
        assert!(latest_in(dir.path())?.is_none());
        append(dir.path(), event("daemon", "v1", 20))?;
        let mut log = OpenOptions::new()
            .append(true)
            .open(dir.path().join("docker-observations.jsonl"))?;
        let mut line = serde_json::to_vec(&event("daemon", "v1", 30))?;
        line.push(b'\n');
        log.write_all(&line)?;
        assert!(latest_in(dir.path()).is_err());
        let other = tempfile::tempdir()?;
        std::fs::write(other.path().join("docker-observations.jsonl"), &line)?;
        assert!(latest_in(other.path()).is_err());
        Ok(())
    }

    #[test]
    fn busy_history_fails_without_waiting() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.path().join("docker-observations.jsonl"))?;
        file.try_lock()?;
        let started = std::time::Instant::now();
        assert!(append(dir.path(), event("daemon", "v1", 20)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        Ok(())
    }
}
