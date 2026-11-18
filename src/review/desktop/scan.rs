use super::super::{artifacts, bytes, candidates, history, inventory, resolve, Profile, Scope};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const THREADS: u16 = 4;
pub(super) type Baseline = Option<Arc<history::Snapshot>>;

pub(super) struct Scan {
    pub root: PathBuf,
    pub policy: Arc<artifacts::Policy>,
    pub inventory: inventory::Inventory,
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
    ) -> Result<Self> {
        let Scope {
            roots,
            policy,
            sources,
        } = resolve(profile, true)?;
        let root = roots
            .get(index)
            .context("Workspace is no longer available")?
            .clone();
        let inventory = inventory::scan(&root, THREADS, None)?;
        let findings = sources.collect(&policy);
        let external = candidates::deduplicate(findings.rows(&inventory));
        let mut notes = findings.notes;
        let mut recorded = false;
        let previous = if let Some(name) = profile_name {
            match history::History::open(name) {
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
        Ok(Self {
            root,
            policy: Arc::new(policy),
            inventory,
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

pub(super) struct Confirmation {
    policy: Arc<artifacts::Policy>,
    items: Vec<artifacts::Prepared>,
}

impl Confirmation {
    pub fn prepare(policy: Arc<artifacts::Policy>, paths: Vec<PathBuf>) -> Result<Self> {
        if paths.is_empty() {
            bail!("Choose a generated folder to review.");
        }
        let items = run_in_pairs(&paths, |path| {
            policy.prepare(path).with_context(|| display(path))
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
        Ok(Self { policy, items })
    }

    pub fn items(&self) -> &[artifacts::Prepared] {
        &self.items
    }

    pub fn remove(self) -> Vec<String> {
        run_in_pairs(&self.items, |item| {
            let label = display(&item.path);
            match self.policy.remove(item) {
                Ok(()) => format!("Deleted {label}"),
                Err(err) => format!("{label}: {err:#}"),
            }
        })
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
            roots: vec![root],
            outputs: outputs.to_vec(),
            docker_context: None,
        });
        let confirmation = Confirmation::prepare(policy, outputs.to_vec())?;
        fs::write(outputs[1].join("artifact.bin"), b"changed after approval")?;
        let results = confirmation.remove();
        assert!(results[0].starts_with("Deleted "));
        assert!(results[1].contains("changed after review"));
        assert!(results[2].starts_with("Deleted "));
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
            roots: vec![root],
            outputs: vec![output.clone()],
            docker_context: None,
        });
        let preview = Confirmation::prepare(policy.clone(), vec![output.clone()])?;
        assert_eq!(preview.items()[0].logical_bytes(), 6);
        drop(preview);
        assert_eq!(fs::read(&file)?, b"before");

        let preview = Confirmation::prepare(policy, vec![output])?;
        fs::write(&file, b"after")?;
        assert!(preview.remove()[0].contains("changed after review"));
        assert_eq!(fs::read(&file)?, b"after");
        Ok(())
    }
}
