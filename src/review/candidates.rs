use super::message::Message;
use super::{artifacts::Policy, docker, inventory, kube, worktrees};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) struct Sources {
    pub git_repos: Vec<PathBuf>,
    pub git_base: Option<String>,
    pub docker_cache: bool,
    pub kube: Option<kube::Source>,
}

#[derive(Default)]
pub(super) struct Findings {
    git: Vec<worktrees::ReviewCandidate>,
    docker: Vec<docker::ReviewCandidate>,
    kube: Vec<kube::Candidate>,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct DisplayRow {
    pub kind: &'static str,
    pub label: String,
    pub reason: Message,
    pub estimated_bytes: Option<u64>,
    pub estimate_basis: &'static str,
    pub eligible: bool,
}

impl Sources {
    pub fn configured(&self, policy: &Policy) -> bool {
        !self.git_repos.is_empty() || policy.docker_context.is_some() || self.kube.is_some()
    }

    pub fn collect(&self, policy: &Policy) -> Findings {
        let mut findings = Findings::default();
        for repo in &self.git_repos {
            let base = self.git_base.as_deref().unwrap_or("refs/heads/main");
            match worktrees::review_candidates(repo, &policy.roots, base) {
                Ok(mut rows) => findings.git.append(&mut rows),
                Err(err) => findings
                    .notes
                    .push(format!("Git {}: {err}", inventory::display(repo))),
            }
        }
        if let Some(context) = &policy.docker_context {
            match docker::review_candidates(context, self.docker_cache) {
                Ok(result) => {
                    findings.docker = result.items;
                    if let Some(error) = result.cache_error {
                        findings.notes.push(format!("Docker BuildKit: {error}"));
                    }
                    if let Some(error) = result.volume_error {
                        findings.notes.push(format!("Docker volumes: {error}"));
                    }
                }
                Err(err) => findings.notes.push(format!("Docker {context}: {err}")),
            }
        }
        if let Some(source) = &self.kube {
            match kube::candidates(source) {
                Ok(rows) => findings.kube = rows,
                Err(err) => findings.notes.push(format!(
                    "Kubernetes {} / {}: {err:#}",
                    source.context, source.namespace
                )),
            }
        }
        findings
    }
}

impl Findings {
    pub fn rows(&self, scan: &inventory::Inventory) -> Vec<DisplayRow> {
        let git = self.git.iter().map(|item| {
            let usage = if item.removable {
                scan.directories
                    .binary_search_by(|row| row.path.cmp(&item.path))
                    .ok()
                    .map(|index| &scan.directories[index].usage)
            } else {
                None
            };
            let bytes = usage.and_then(|usage| usage.allocated_bytes);
            DisplayRow {
                kind: "Git",
                label: inventory::display(&item.path),
                reason: item.reason.with_prefix(&inventory::display(
                    item.repo.file_name().map(Path::new).unwrap_or(&item.repo),
                )),
                estimated_bytes: bytes,
                estimate_basis: if bytes.is_some() {
                    "Observed allocation; shared blocks may stay allocated"
                } else {
                    "Host reclaim estimate unavailable"
                },
                eligible: item.removable,
            }
        });
        let docker = self.docker.iter().map(|item| DisplayRow {
            kind: "Docker",
            label: item.key.clone(),
            reason: item.reason.clone(),
            estimated_bytes: item.estimated_bytes,
            estimate_basis: item.estimate_note,
            eligible: item.eligible,
        });
        let kube = self.kube.iter().map(|item| DisplayRow {
            kind: "Kubernetes",
            label: item.label.clone(),
            reason: item.reason.clone(),
            estimated_bytes: None,
            estimate_basis: "Cluster reclaim estimate unavailable",
            eligible: item.eligible,
        });
        git.chain(docker).chain(kube).collect()
    }
}

pub(super) fn deduplicate(rows: Vec<DisplayRow>) -> Vec<DisplayRow> {
    let mut items = BTreeMap::new();
    for row in rows {
        let key = (row.kind, row.label.clone());
        match items.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(row);
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if entry.get().estimated_bytes.is_none() && row.estimated_bytes.is_some() =>
            {
                entry.insert(row);
            }
            _ => {}
        }
    }
    items.into_values().collect()
}
