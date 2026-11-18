pub mod app_storage;
mod artifacts;
mod candidates;
#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
pub mod desktop;
pub mod docker;
mod filesystem;
mod git;
mod history;
mod inventory;
pub mod kube;
mod message;
mod profile;
#[cfg(all(feature = "desktop", target_os = "macos"))]
mod sdk_cleanup;
pub mod stale;
mod ui;
pub mod worktrees;

use crate::cli::{ListFormat, ReviewArgs};
use anyhow::{bail, Result};
use artifacts::Policy;
use history::History;
use inventory::display;
use profile::Profile;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

fn external_command(name: &str) -> Command {
    #[cfg(target_os = "macos")]
    let program = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name));
    #[cfg(not(target_os = "macos"))]
    let program = name;
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

pub fn run(args: &ReviewArgs) -> Result<()> {
    let input = match &args.profile {
        Some(name) => Profile::load(name)?,
        None => Profile {
            roots: args.roots.clone(),
            cleanup_roots: args.cleanup_roots.clone(),
            outputs: args.outputs.clone(),
            docker_context: args.docker_context.clone(),
            docker_cache: args.docker_cache,
            git_repos: args.git_repos.clone(),
            git_base: args.git_base.clone(),
            kube: args
                .kubeconfig
                .as_ref()
                .zip(args.kube_context.as_ref())
                .zip(args.kube_namespace.as_ref())
                .map(|((config, context), namespace)| {
                    kube::Source::new(config.clone(), context.clone(), namespace.clone())
                })
                .transpose()?,
        },
    };
    let Scope {
        roots,
        policy,
        sources,
    } = resolve(input, args.profile.is_some())?;
    if let Some(name) = &args.save_profile {
        profile_for(&roots, &policy, &sources).save(name)?;
    }
    let history = args
        .profile
        .as_deref()
        .or(args.save_profile.as_deref())
        .map(History::open)
        .transpose()?;
    let format = args
        .format
        .clone()
        .or_else(|| (!std::io::stdout().is_terminal()).then_some(ListFormat::Text));
    if let Some(format) = format {
        report(roots, args, &policy, format, history.as_ref(), &sources)
    } else {
        ui::run(
            roots,
            args.threads,
            args.search.clone(),
            policy,
            history,
            sources,
        )
    }
}

struct Scope {
    roots: Vec<PathBuf>,
    policy: Policy,
    sources: candidates::Sources,
}

fn resolve(input: Profile, loading_saved_profile: bool) -> Result<Scope> {
    let mut policy = Policy {
        docker_context: input.docker_context,
        roots: input
            .cleanup_roots
            .iter()
            .map(|p| inventory::safe_workspace_dir(p))
            .collect::<Result<_>>()?,
        outputs: Vec::new(),
    };
    for output in input.outputs {
        // A dedicated output may have been removed at the last cleanup. Retain its
        // declaration so that a later scan can find it if the script regenerates it.
        let output = match fs::symlink_metadata(&output) {
            Err(err) if loading_saved_profile && err.kind() == std::io::ErrorKind::NotFound => {
                output
            }
            _ => inventory::safe_workspace_dir(&output)?,
        };
        policy.outputs.push(output.clone());
        if policy.producer(&output) != Some(artifacts::Producer::DeclaredOutput) {
            bail!("Output must be a dedicated directory beneath a cleanup root: {output:?}");
        }
    }
    let default = [std::env::current_dir()?];
    let roots = match (input.roots.as_slice(), policy.roots.as_slice()) {
        ([], []) => default.as_slice(),
        ([], roots) | (roots, _) => roots,
    };
    let roots = roots
        .iter()
        .map(|path| inventory::workspace_dir(path))
        .collect::<Result<Vec<_>>>()?;
    let sources = candidates::Sources {
        git_repos: input
            .git_repos
            .iter()
            .map(|p| inventory::safe_workspace_dir(p))
            .collect::<Result<_>>()?,
        docker_cache: input.docker_cache,
        git_base: input.git_base,
        kube: input.kube,
    };
    Ok(Scope {
        roots,
        policy,
        sources,
    })
}

fn profile_for(roots: &[PathBuf], policy: &Policy, sources: &candidates::Sources) -> Profile {
    Profile {
        roots: roots.to_vec(),
        cleanup_roots: policy.roots.clone(),
        outputs: policy.outputs.clone(),
        docker_context: policy.docker_context.clone(),
        docker_cache: sources.docker_cache,
        git_repos: sources.git_repos.clone(),
        git_base: sources.git_base.clone(),
        kube: sources.kube.clone(),
    }
}

fn report(
    roots: Vec<PathBuf>,
    args: &ReviewArgs,
    policy: &Policy,
    format: ListFormat,
    history: Option<&History>,
    sources: &candidates::Sources,
) -> Result<()> {
    let mut reports = Vec::new();
    let external = sources.collect(policy);
    let mut external_rows = Vec::new();
    for root in roots {
        let mut scan = inventory::scan(&root, args.threads, args.search.as_deref())?;
        external_rows.extend(external.rows(&scan));
        inventory::sort_rows(&mut scan.directories, false);
        let previous = history
            .map(|history| history.previous(&root))
            .transpose()?
            .flatten();
        let candidates = artifacts::file_candidates(&scan, policy);
        match format {
            ListFormat::Json => {
                let mut report = serde_json::json!({
                "root": display(&root),
                    "inventory": scan,
                    "outputs_to_review": candidates.iter().map(|candidate| serde_json::json!({
                        "entry": candidate.row,
                        "reason": candidate.producer.reason(),
                        "selectable": candidate.blocked_reason.is_none(),
                        "blocked_reason": candidate.blocked_reason,
                    })).collect::<Vec<_>>()
                });
                if let Some(previous) = &previous {
                    let changes: Vec<_> = scan
                        .directories
                        .iter()
                        .filter_map(|row| {
                            let delta = previous.delta(row)?;
                            (delta.is_new() || delta.signed_bytes() != 0).then(|| {
                                serde_json::json!({
                                    "path": display(&row.path), "delta": delta,
                                })
                            })
                        })
                        .collect();
                    report["previous_scan"] = serde_json::json!({
                        "completed_at": previous.completed_at(), "changes": changes,
                    });
                }
                reports.push(report);
            }
            ListFormat::Text => {
                println!("{}", display(&root));
                if let Some(previous) = &previous {
                    println!(
                        "Compared with {}",
                        previous.completed_at().with_timezone(&chrono::Local)
                    );
                }
                let rows = if args.search.is_some() {
                    &scan.matches
                } else {
                    &scan.directories
                };
                for row in rows.iter().take(30) {
                    let delta = previous.as_ref().and_then(|previous| previous.delta(row));
                    println!(
                        "{:>12}  {}{}{}",
                        bytes(row.usage.size()),
                        display(&row.path),
                        if row.usage.skipped > 0 {
                            " [partial]"
                        } else {
                            ""
                        },
                        delta
                            .map(|delta| format!("  ({})", delta.label()))
                            .unwrap_or_default(),
                    );
                }
                for candidate in candidates {
                    let row = candidate.row;
                    let producer = candidate.producer.reason();
                    let detail = candidate.blocked_reason.as_ref().map_or_else(
                        || producer.to_owned(),
                        |reason| format!("{producer} · {reason}"),
                    );
                    println!(
                        "{:8}  {:>12}  {}  ({})",
                        if candidate.blocked_reason.is_none() {
                            "Review"
                        } else {
                            "Blocked"
                        },
                        bytes(row.usage.size()),
                        display(&row.path),
                        detail
                    );
                }
            }
        }
    }
    external_rows = candidates::deduplicate(external_rows);
    if matches!(format, ListFormat::Json) {
        let mut report = serde_json::json!({"roots": reports});
        if sources.configured(policy) {
            report["external_candidates"] = serde_json::to_value(&external_rows)?;
            report["candidate_notes"] = serde_json::to_value(&external.notes)?;
        }
        println!("{report}");
    } else {
        for row in external_rows {
            let estimate = row.estimated_bytes.map_or_else(|| "—".into(), bytes);
            println!(
                "{:6} {estimate:>12}  {}  ({})",
                row.kind, row.label, row.reason
            );
        }
        for note in external.notes {
            println!("{note}");
        }
    }
    Ok(())
}

pub fn bytes(size: u64) -> String {
    humansize::format_size(size, humansize::BINARY)
}
