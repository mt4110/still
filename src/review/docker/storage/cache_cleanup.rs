use super::super::{
    buildx_command, cache_id, cache_parent_ids, cache_size, cache_timestamp, command_on, connect,
    json_lines, Cache, Daemon, EngineCache, CACHE_SAFETY_FILTERS, ENGINE_CACHE_TEMPLATE,
};
use super::actions;
use crate::review::app_storage::command_output;
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use std::{
    collections::{HashMap, HashSet},
    process::Command,
    time::Duration,
};

const BUILDX_FORMAT: &str = r#"{"ID":{{json .ID}},"Reclaimable":{{json .Reclaimable}},"Shared":{{json .Shared}},"Mutable":{{json .Mutable}},"Size":{{json .Size}},"Type":{{json .Type}},"Parents":{{json .Parents}}}"#;
const MAX_REVIEW: usize = 100;
const INSPECT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CacheTarget {
    pub id: String,
    pub size: String,
    pub last_used: DateTime<Utc>,
    pub selected: bool,
}

pub(crate) struct CacheCleanup {
    pub context: String,
    pub days: u16,
    pub targets: Vec<CacheTarget>,
    pub eligible_count: usize,
    pub referenced_count: usize,
    daemon: Daemon,
    pin: actions::Pin,
    cutoff: DateTime<Utc>,
    buildx: tempfile::TempDir,
}

pub(crate) struct CacheCleanupResult {
    pub context: String,
    pub days: u16,
    pub selected: usize,
    pub remaining: Option<usize>,
    pub attempted: bool,
    pub result: Result<String>,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub history_error: Option<String>,
}

impl CacheCleanup {
    pub fn prepare(context: &str, days: u16) -> Result<Self> {
        Self::prepare_selection(context, days, None)
    }

    pub fn prepare_one(context: &str, days: u16, id: &str) -> Result<Self> {
        Self::prepare_selection(context, days, Some(id))
    }

    fn prepare_selection(context: &str, days: u16, only: Option<&str>) -> Result<Self> {
        ensure!(
            (1..=3650).contains(&days),
            "Choose a retention period between 1 and 3650 days"
        );
        let daemon = connect(context)?;
        let pin = actions::capture(&daemon.endpoint)?;
        let cutoff = Utc::now() - TimeDelta::days(i64::from(days));
        // Local, isolated Buildx configuration pins the default builder to this socket.
        let buildx = tempfile::tempdir()?;
        let (eligible, referenced_count) = eligible_targets(&daemon, &buildx, cutoff)?;
        let mut targets: Vec<_> = eligible
            .into_values()
            .filter(|item| only.is_none_or(|id| item.id == id))
            .collect();
        let referenced_count = if only.is_some() { 0 } else { referenced_count };
        targets.sort_by_cached_key(|item| {
            (
                std::cmp::Reverse(cache_size(&item.size)),
                item.last_used,
                item.id.clone(),
            )
        });
        let eligible_count = targets.len();
        targets.truncate(MAX_REVIEW);
        Ok(Self {
            context: context.to_owned(),
            days,
            targets,
            eligible_count,
            referenced_count,
            daemon,
            pin,
            cutoff,
            buildx,
        })
    }

    fn verify_daemon(&self) -> Result<()> {
        ensure!(
            connect(&self.context)? == self.daemon
                && actions::capture(&self.daemon.endpoint)? == self.pin,
            "Docker context or daemon changed after confirmation"
        );
        Ok(())
    }

    pub fn apply(self) -> CacheCleanupResult {
        let started = Utc::now();
        let free = || {
            directories::BaseDirs::new()
                .and_then(|d| crate::review::filesystem::available(d.home_dir()))
        };
        let before = free();
        let selected: Vec<_> = self
            .targets
            .iter()
            .filter(|target| target.selected)
            .collect();
        let mut attempted = false;
        let result = (|| {
            self.verify_daemon()?;
            let selector = exact_selector(selected.iter().map(|target| target.id.as_str()))?;
            let (current, _) = eligible_targets(&self.daemon, &self.buildx, self.cutoff)?;
            ensure!(
                selected
                    .iter()
                    .all(|target| current.get(&target.id) == Some(*target)),
                "Reviewed cache records changed. Review the current candidates again"
            );
            let matching = read_buildx(&self.daemon, &self.buildx, Some(&selector))?;
            ensure!(
                matching.len() == selected.len()
                    && selected
                        .iter()
                        .all(|target| matching.contains_key(&target.id)),
                "Buildx could not confirm the exact reviewed cache IDs"
            );
            self.verify_daemon()?;
            let mut command = cache_command(&self.daemon, &self.buildx, "prune", Some(&selector));
            command.args([
                "--force",
                "--filter",
                &format!("until={}h", u64::from(self.days) * 24),
            ]);
            attempted = true;
            command_output(&mut command, Duration::from_secs(300))
                .context("Docker cache deletion did not complete")
        })();
        let remaining = if attempted {
            (|| {
                self.verify_daemon()?;
                let current = read_buildx(&self.daemon, &self.buildx, None)?;
                Ok::<_, anyhow::Error>(
                    selected
                        .iter()
                        .filter(|item| current.contains_key(&item.id))
                        .count(),
                )
            })()
            .ok()
        } else {
            None
        };
        let receipt = CacheCleanupResult {
            context: self.context,
            days: self.days,
            selected: selected.len(),
            remaining,
            attempted,
            result,
            before,
            after: free(),
            history_error: None,
        };
        #[cfg(not(test))]
        let mut receipt = receipt;
        #[cfg(not(test))]
        {
            use crate::review::cleanup_history::{self, Operation, OperationTarget};
            let status = match (receipt.attempted, receipt.result.is_ok(), receipt.remaining) {
                (false, _, _) => "skipped",
                (true, true, Some(0)) => "completed",
                (true, true, Some(n)) if n < receipt.selected => "partially_remaining",
                (true, true, Some(_)) => "remaining",
                _ => "unverified",
            };
            receipt.history_error = cleanup_history::save_operation(
                started,
                receipt.before,
                receipt.after,
                Operation {
                    kind: "docker_builder_cache_cleanup".into(),
                    retention_days: Some(receipt.days),
                    targets: vec![OperationTarget {
                        identifier: "reviewed default builder cache".into(),
                        status: status.into(),
                        reason: Some(match receipt.remaining {
                            Some(n) => {
                                format!("{n} of {} selected records still listed", receipt.selected)
                            }
                            None if receipt.attempted => {
                                "Post-deletion inventory could not be verified".into()
                            }
                            None => "Pre-deletion verification failed".into(),
                        }),
                        remaining_image_bytes: None,
                    }],
                },
            )
            .err()
            .map(|error| format!("{error:#}"));
        }
        #[cfg(test)]
        let _ = started;
        receipt
    }
}

fn exact_selector<'a>(ids: impl Iterator<Item = &'a str>) -> Result<String> {
    let ids: Vec<_> = ids.collect();
    ensure!(
        (1..=MAX_REVIEW).contains(&ids.len()) && ids.iter().all(|id| cache_id(id)),
        "No valid reviewed cache selection"
    );
    Ok(format!("id~=^({})$", ids.join("|")))
}

fn cache_command(
    daemon: &Daemon,
    buildx: &tempfile::TempDir,
    operation: &str,
    selector: Option<&str>,
) -> Command {
    let mut cmd = buildx_command(daemon, buildx);
    cmd.args([operation, "--builder", "default"]);
    if let Some(selector) = selector {
        cmd.args(["--filter", selector]);
        for filter in CACHE_SAFETY_FILTERS {
            cmd.args(["--filter", filter]);
        }
    }
    cmd
}

fn read_buildx(
    daemon: &Daemon,
    buildx: &tempfile::TempDir,
    selector: Option<&str>,
) -> Result<HashMap<String, Cache>> {
    let mut cmd = cache_command(daemon, buildx, "du", selector);
    cmd.args(["--format", BUILDX_FORMAT]);
    let raw = command_output(&mut cmd, INSPECT_TIMEOUT)
        .context("Could not inspect the selected daemon's default builder with Docker Buildx")?;
    let mut records = HashMap::new();
    for record in json_lines::<Cache>(raw.as_bytes())? {
        ensure!(
            cache_id(&record.id) && records.insert(record.id.clone(), record).is_none(),
            "Buildx returned an invalid or duplicate cache ID"
        );
    }
    Ok(records)
}

fn eligible_targets(
    daemon: &Daemon,
    buildx: &tempfile::TempDir,
    cutoff: DateTime<Utc>,
) -> Result<(HashMap<String, CacheTarget>, usize)> {
    let raw = command_output(
        command_on(&daemon.endpoint).args([
            "system",
            "df",
            "--verbose",
            "--format",
            ENGINE_CACHE_TEMPLATE,
        ]),
        INSPECT_TIMEOUT,
    )
    .context("Could not inspect exact cache last-use times from Docker Engine")?;
    let buildx_records = read_buildx(daemon, buildx, None)?;
    let parents = cache_parent_ids(buildx_records.values())?;
    let mut referenced_count = 0;
    let mut seen = HashSet::new();
    let mut targets = HashMap::new();
    for engine in json_lines::<EngineCache>(raw.as_bytes())? {
        ensure!(
            cache_id(&engine.id) && seen.insert(engine.id.clone()),
            "Docker Engine returned an invalid or duplicate cache ID"
        );
        if let Some(cache) = buildx_records.get(&engine.id) {
            if let Some(target) = eligible_target(engine, cache, cutoff) {
                if parents.contains(target.id.as_str()) {
                    referenced_count += 1;
                } else {
                    targets.insert(target.id.clone(), target);
                }
            }
        }
    }
    Ok((targets, referenced_count))
}

fn eligible_target(
    engine: EngineCache,
    cache: &Cache,
    cutoff: DateTime<Utc>,
) -> Option<CacheTarget> {
    // Buildx du can ignore the until selector; use the absolute Engine timestamp.
    if !cache.reclaimable
        || cache.shared
        || cache.mutable
        || cache.kind != "regular"
        || engine.in_use != "false"
        || engine.shared != "false"
        || engine.cache_type != "regular"
    {
        return None;
    }
    let last_used = cache_timestamp(&engine.last_used_at).filter(|time| *time < cutoff)?;
    Some(CacheTarget {
        id: engine.id,
        size: cache.size.clone(),
        last_used,
        selected: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buildx_reclaimable_alone_does_not_make_a_record_safe_to_delete() {
        let cache = Cache {
            id: "oldrecord123".into(),
            reclaimable: true,
            parents: None,
            shared: false,
            mutable: false,
            size: "1MB".into(),
            kind: "regular".into(),
        };
        let engine = |last_used: &str, shared: &str| EngineCache {
            id: cache.id.clone(),
            last_used_at: last_used.into(),
            in_use: "false".into(),
            shared: shared.into(),
            cache_type: "regular".into(),
        };
        let cutoff = cache_timestamp("2026-11-17T05:35:19Z").unwrap();
        let old = "2026-11-16 05:35:19.123 +0000 UTC";
        assert!(eligible_target(engine(old, "false"), &cache, cutoff).is_some());
        for (time, shared) in [
            ("0001-01-01T00:00:00Z", "false"),
            ("2026-11-17T05:35:19Z", "false"),
            ("2026-11-24T05:35:19Z", "false"),
            (old, "true"),
        ] {
            assert!(eligible_target(engine(time, shared), &cache, cutoff).is_none());
        }
    }

    #[test]
    fn selector_never_expands_to_unreviewed_or_malformed_ids() -> Result<()> {
        let selector = exact_selector(["oldrecord123", "oldrecord456"].into_iter())?;
        assert_eq!(selector, "id~=^(oldrecord123|oldrecord456)$");
        assert!(exact_selector(std::iter::empty()).is_err());
        assert!(exact_selector(["oldrecord123|.*"].into_iter()).is_err());
        Ok(())
    }
}
