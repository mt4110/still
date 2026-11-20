use super::super::{
    artifacts, bytes, candidates, cleanup_history, history, inventory, resolve, Profile, Scope,
};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub(super) const THREADS: u16 = 4;
pub(super) type Baseline = Option<Arc<history::Snapshot>>;

pub(super) struct Scan {
    #[cfg(target_os = "macos")]
    pub kept: Option<super::keep::KeepRules>,
    pub root: PathBuf,
    pub policy: Arc<artifacts::Policy>,
    pub directories: super::folder_index::Directories,
    pub previous: Baseline,
    pub recorded: bool,
    pub files: Vec<artifacts::FileCandidate>,
    pub external: Vec<candidates::DisplayRow>,
    pub notes: Vec<String>,
}

impl Scan {
    pub fn run(
        profile: Profile,
        index: usize,
        profile_name: Option<&str>,
        baseline: Option<Baseline>,
        record: bool,
        read_only: bool,
        progress: &inventory::ScanProgress,
    ) -> Result<Self> {
        let Scope {
            roots,
            policy,
            sources,
        } = if read_only {
            Scope {
                roots: profile
                    .roots
                    .iter()
                    .map(|p| inventory::safe_dir(p))
                    .collect::<Result<_>>()?,
                policy: artifacts::Policy {
                    roots: vec![],
                    outputs: vec![],
                    docker_context: None,
                },
                sources: candidates::Sources {
                    git_repos: vec![],
                    git_base: None,
                    docker_cache: false,
                    kube: None,
                },
            }
        } else {
            resolve(profile, true)?
        };
        let root = roots
            .get(index)
            .context("Workspace is no longer available")?
            .clone();
        let inventory = inventory::scan_with_progress(&root, THREADS, None, Some(progress))?;
        let findings = sources.collect(&policy);
        let external = candidates::deduplicate(findings.rows(&inventory));
        let mut notes = findings.notes;
        let mut recorded = false;
        let history = profile_name.map(history::History::open);
        #[cfg(target_os = "macos")]
        let history = history.or_else(|| Some(history::History::workspace()));
        let previous = if let Some(history) = history {
            match history {
                Ok(history) => {
                    let previous = baseline.unwrap_or_else(|| match history.previous(&root) {
                        Ok(snapshot) => snapshot.map(Arc::new),
                        Err(err) => {
                            notes.push(format!("Could not read scan history: {err:#}"));
                            None
                        }
                    });
                    if record {
                        match history.record(&root, &inventory) {
                            Ok(()) => recorded = true,
                            Err(err) => notes.push(format!("Could not save scan history: {err:#}")),
                        }
                    }
                    previous
                }
                Err(err) => {
                    notes.push(format!("Scan history unavailable: {err:#}"));
                    None
                }
            }
        } else {
            None
        };
        let files = artifacts::file_candidates(&inventory, &policy);
        #[cfg(target_os = "macos")]
        let (files, kept) = {
            let mut files = files;
            let kept = if read_only {
                None
            } else {
                match super::keep::KeepRules::load() {
                    Ok(rules) => {
                        for c in &mut files {
                            if c.blocked_reason.is_none() {
                                c.blocked_reason = rules.reason(&c.row.path);
                            }
                        }
                        Some(rules)
                    }
                    Err(error) => {
                        let reason = super::Message::new(
                            format!("Could not read keep settings: {error}"),
                            format!("「残す」設定を読み取れません · {error}"),
                        );
                        notes.push(reason.to_string());
                        for c in &mut files {
                            c.blocked_reason.get_or_insert_with(|| reason.clone());
                        }
                        None
                    }
                }
            };
            (files, kept)
        };
        Ok(Self {
            #[cfg(target_os = "macos")]
            kept,
            directories: super::folder_index::Directories::from_inventory(&root, inventory),
            root,
            policy: Arc::new(policy),
            previous,
            recorded,
            files,
            external,
            notes,
        })
    }

    pub fn delta(&self, row: &inventory::Row) -> Option<history::Delta> {
        self.previous.as_ref()?.delta(row)
    }
}

pub(super) struct CleanupReceipt {
    pub outcomes: Vec<(String, Result<(), super::Message>)>,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub held: usize,
    pub history_error: Option<String>,
}

pub(super) struct Confirmation {
    policy: Arc<artifacts::Policy>,
    items: Vec<artifacts::Prepared>,
    totals: Vec<ReviewSize>,
}

pub(super) struct ReviewSize {
    pub files: usize,
    pub logical_bytes: u64,
    pub scanned_allocated: Option<u64>,
}

#[derive(Default)]
pub(super) struct ReviewProgress {
    files: AtomicU64,
    bytes: AtomicU64,
}

impl ReviewProgress {
    fn checked(&self, size: u64) {
        self.bytes.fetch_add(size, Ordering::Relaxed);
        self.files.fetch_add(1, Ordering::Relaxed);
    }

    pub fn label(&self, japanese: bool) -> String {
        let files = self.files.load(Ordering::Relaxed);
        let logical = bytes(self.bytes.load(Ordering::Relaxed));
        if japanese {
            format!("内容照合済み {files} ファイル · 論理容量 {logical}")
        } else {
            format!("Verified contents of {files} files · {logical} logical")
        }
    }
}

impl Confirmation {
    #[cfg(test)]
    pub fn prepare(policy: Arc<artifacts::Policy>, paths: Vec<PathBuf>) -> Result<Self> {
        Self::prepare_reporting(policy, paths, &ReviewProgress::default())
    }

    pub(super) fn prepare_reporting(
        policy: Arc<artifacts::Policy>,
        paths: Vec<PathBuf>,
        progress: &ReviewProgress,
    ) -> Result<Self> {
        if paths.is_empty() {
            bail!("Choose a generated folder to review.");
        }
        #[cfg(target_os = "macos")]
        {
            let rules = super::keep::KeepRules::load()?;
            for path in &paths {
                rules.check(path)?;
            }
        }
        let items = run_in_pairs(&paths, |path| {
            policy
                .prepare_reporting(path, |size| progress.checked(size))
                .with_context(|| display(path))
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        let mut ordered: Vec<_> = items.iter().map(|item| &item.path).collect();
        ordered.sort_unstable();
        if let Some(pair) = ordered.windows(2).find(|pair| pair[1].starts_with(pair[0])) {
            bail!(
                "Select one folder when candidates overlap: {}",
                display(pair[1])
            );
        }
        // Size and count were accumulated while each file was stamped.
        let totals = items
            .iter()
            .map(|item| ReviewSize {
                files: item.files(),
                logical_bytes: item.logical_bytes(),
                scanned_allocated: None,
            })
            .collect();
        Ok(Self {
            policy,
            items,
            totals,
        })
    }

    pub fn items(&self) -> &[artifacts::Prepared] {
        &self.items
    }

    pub fn match_scan_sizes(&mut self, scan: &Scan) {
        let sizes: std::collections::HashMap<_, _> = scan
            .files
            .iter()
            .filter_map(|c| {
                c.row
                    .usage
                    .allocated_bytes
                    .map(|size| (c.row.path.as_path(), size))
            })
            .collect();
        for (item, summary) in self.items.iter().zip(&mut self.totals) {
            summary.scanned_allocated = sizes.get(item.path.as_path()).copied();
        }
    }

    pub fn summaries(&self) -> impl Iterator<Item = (&artifacts::Prepared, &ReviewSize)> {
        self.items.iter().zip(&self.totals)
    }

    fn remove_reviewed(&self, item: &artifacts::Prepared) -> Result<()> {
        #[cfg(target_os = "macos")]
        super::keep::KeepRules::load()?.check(&item.path)?;
        self.policy.remove(item)
    }

    pub fn remove(self, root: &Path) -> CleanupReceipt {
        let started_at = chrono::Utc::now();
        let before = super::super::filesystem::available(root);
        let outcomes = run_in_pairs(&self.items, |item| {
            let result = self
                .remove_reviewed(item)
                .map_err(|err| super::Message::from_error(&err));
            (display(&item.path), result)
        });
        let after = super::super::filesystem::available(root);
        let held = outcomes
            .iter()
            .filter(|(_, result)| result.is_err())
            .count();
        let items = self
            .items
            .iter()
            .zip(&outcomes)
            .map(|(item, (_, result))| {
                let original_path_exists = match std::fs::symlink_metadata(&item.path) {
                    Ok(_) => Some(true),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(false),
                    Err(_) => None,
                };
                cleanup_history::Item {
                    path: item.path.clone(),
                    producer: item.producer.reason().to_owned(),
                    logical_bytes: item.logical_bytes(),
                    status: if result.is_ok() {
                        "removed"
                    } else {
                        "incomplete"
                    }
                    .into(),
                    reason: result.as_ref().err().map(ToString::to_string),
                    original_path_exists,
                }
            })
            .collect();
        let record = cleanup_history::Record {
            version: 1,
            root: root.to_owned(),
            started_at,
            completed_at: chrono::Utc::now(),
            available_before: before,
            available_after: after,
            items,
        };
        #[cfg(not(test))]
        let history_error = cleanup_history::save(&record)
            .err()
            .map(|error| format!("{error:#}"));
        #[cfg(test)]
        let history_error = {
            let _ = record;
            None
        };
        CleanupReceipt {
            outcomes,
            before,
            after,
            held,
            history_error,
        }
    }
}

// The calling thread already runs off the UI thread. Add at most one more I/O
// worker, and keep receipts in selection order even if the second half finishes first.
fn run_in_pairs<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() < 2 {
        return items.iter().map(work).collect();
    }
    let (left, right) = items.split_at(items.len().div_ceil(2));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| right.iter().map(&work).collect::<Vec<_>>());
        let mut results: Vec<_> = left.iter().map(&work).collect();
        results.extend(worker.join().expect("cleanup worker"));
        results
    })
}

pub(super) fn display(path: &Path) -> String {
    let label = inventory::display(path);
    if cfg!(windows) {
        if let Some(unc) = label.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(drive) = label.strip_prefix(r"\\?\") {
            return drive.to_owned();
        }
    }
    label
}

pub(super) fn usage(usage: &inventory::Usage) -> String {
    let basis = if usage.allocated_bytes.is_some() {
        "allocated"
    } else {
        "logical"
    };
    format!("{} {basis}", bytes(usage.size()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bulk_removal_rechecks_each_output_and_keeps_result_order() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let outputs = ["a", "b", "c"].map(|name| root.join(name));
        for output in &outputs {
            fs::create_dir(output)?;
            fs::write(output.join("artifact.bin"), b"generated")?;
        }
        let policy = Arc::new(artifacts::Policy {
            roots: vec![root.clone()],
            outputs: outputs.to_vec(),
            docker_context: None,
        });
        let confirmation = Confirmation::prepare(policy, outputs.to_vec())?;
        fs::write(outputs[1].join("artifact.bin"), b"changed after approval")?;
        let receipt = confirmation.remove(&root);
        assert_eq!(receipt.held, 1);
        let results = receipt.outcomes;
        assert!(results[0].1.is_ok());
        assert!(results[1]
            .1
            .as_ref()
            .unwrap_err()
            .text(false)
            .contains("changed after review"));
        assert!(results[2].1.is_ok());
        assert!(!outputs[0].exists() && !outputs[2].exists());
        assert_eq!(
            fs::read(outputs[1].join("artifact.bin"))?,
            b"changed after approval"
        );
        Ok(())
    }

    #[test]
    fn preview_leaves_files_intact_and_stale_review_blocks_deletion() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = inventory::safe_dir(&temp.path().canonicalize()?)?;
        let output = root.join("generated");
        fs::create_dir(&output)?;
        let file = output.join("artifact.bin");
        fs::write(&file, b"before")?;
        let policy = Arc::new(artifacts::Policy {
            roots: vec![root.clone()],
            outputs: vec![output.clone()],
            docker_context: None,
        });
        let preview = Confirmation::prepare(policy.clone(), vec![output.clone()])?;
        assert_eq!(preview.items()[0].logical_bytes(), 6);
        drop(preview);
        assert_eq!(fs::read(&file)?, b"before");

        let preview = Confirmation::prepare(policy, vec![output])?;
        fs::write(&file, b"after")?;
        assert!(preview.remove(&root).outcomes[0]
            .1
            .as_ref()
            .unwrap_err()
            .text(false)
            .contains("changed after review"));
        assert_eq!(fs::read(&file)?, b"after");
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod measurements {
    use super::*;
    use std::fs;
    use std::time::Instant;

    /// Run this by itself in release mode. It only opens the supplied generated
    /// fixture for reading. Setup and OS file-cache behavior are outside the timer.
    #[test]
    #[ignore = "requires a generated workspace fixture"]
    fn measure_workspace_review() -> Result<()> {
        let root = PathBuf::from(
            std::env::var_os("STILL_MEASURE_WORKSPACE").context("fixture root required")?,
        )
        .canonicalize()?;
        let output = root.join("project/target");
        let policy = Arc::new(artifacts::Policy {
            roots: vec![root.clone()],
            outputs: vec![output.clone()],
            docker_context: None,
        });
        let resident_kib = || -> Result<u64> {
            let res = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()?;
            Ok(std::str::from_utf8(&res.stdout)?.trim().parse()?)
        };
        let heap_kib = || {
            let mut stats = std::mem::MaybeUninit::uninit();
            // Apple's malloc.h documents a null zone as the sum of all zones.
            unsafe {
                libc::malloc_zone_statistics(std::ptr::null_mut(), stats.as_mut_ptr());
                stats.assume_init().size_in_use / 1024
            }
        };
        let before = resident_kib()?;
        let heap_before = heap_kib();
        let scan = Scan::run(
            Profile {
                roots: vec![root.clone()],
                cleanup_roots: vec![root.clone()],
                outputs: vec![output.clone()],
                ..Default::default()
            },
            0,
            None,
            Some(None),
            false,
            false,
            &Default::default(),
        )?;
        let idle = resident_kib()?;
        let heap_after = heap_kib();
        for _ in 0..3 {
            let start = Instant::now();
            let confirmation = Confirmation::prepare(policy.clone(), vec![output.clone()])?;
            let (_, size) = confirmation.summaries().next().unwrap();
            eprintln!("review_ms={} files={} logical_bytes={} rss_before_kib={before} rss_after_scan_kib={idle} live_heap_delta_kib={}", start.elapsed().as_millis(), size.files, size.logical_bytes, heap_after.saturating_sub(heap_before));
            std::hint::black_box(&scan);
        }
        assert!(fs::metadata(output)?.is_dir());
        Ok(())
    }
}
