mod artifacts;
pub mod docker;
mod filesystem;
mod git;
mod inventory;
mod ui;
pub mod worktrees;

use crate::cli::{ListFormat, ReviewArgs};
use anyhow::{bail, Result};
use artifacts::Policy;
use inventory::{display, Inventory};
use std::io::IsTerminal;
use std::path::PathBuf;

pub fn run(args: &ReviewArgs) -> Result<()> {
    let policy = Policy {
        docker_context: args.docker_context.clone(),
        roots: args
            .cleanup_roots
            .iter()
            .map(|p| inventory::safe_dir(p))
            .collect::<Result<_>>()?,
        outputs: args
            .outputs
            .iter()
            .map(|p| inventory::safe_dir(p))
            .collect::<Result<_>>()?,
    };
    for output in &policy.outputs {
        if policy.producer(output) != Some(artifacts::Producer::DeclaredOutput) {
            bail!("--output must be a dedicated directory beneath --cleanup-root: {output:?}");
        }
    }
    let default = [std::env::current_dir()?];
    let roots = match (args.roots.as_slice(), policy.roots.as_slice()) {
        ([], []) => default.as_slice(),
        ([], roots) | (roots, _) => roots,
    };
    let roots = roots
        .iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    let format = args
        .format
        .clone()
        .or_else(|| (!std::io::stdout().is_terminal()).then_some(ListFormat::Text));
    if let Some(format) = format {
        report(roots, args, &policy, format)
    } else {
        ui::run(roots, args.threads, args.search.clone(), policy)
    }
}

fn report(
    roots: Vec<PathBuf>,
    args: &ReviewArgs,
    policy: &Policy,
    format: ListFormat,
) -> Result<()> {
    let mut reports = Vec::new();
    for root in roots {
        let mut scan = inventory::scan(&root, args.threads, args.search.as_deref())?;
        inventory::sort_rows(&mut scan.directories, false);
        let candidates: Vec<_> = scan
            .directories
            .iter()
            .filter_map(|row| {
                policy
                    .producer(&row.path)
                    .map(|producer| (row, producer.reason()))
            })
            .collect();
        match format {
            ListFormat::Json => reports.push(serde_json::json!({
                "root": display(&root),
                "inventory": scan,
                "outputs_to_review": candidates.iter().map(|(row, reason)| serde_json::json!({
                    "entry": row,
                    "reason": reason
                })).collect::<Vec<_>>()
            })),
            ListFormat::Text => {
                println!("{}", display(&root));
                let rows = if args.search.is_some() {
                    &scan.matches
                } else {
                    &scan.directories
                };
                for row in rows.iter().take(30) {
                    println!(
                        "{:>12}  {}{}",
                        bytes(row.usage.size()),
                        display(&row.path),
                        if row.usage.skipped > 0 {
                            " [partial]"
                        } else {
                            ""
                        }
                    );
                }
                for (row, reason) in candidates {
                    println!(
                        "Review  {:>12}  {}  ({reason})",
                        bytes(row.usage.size()),
                        display(&row.path)
                    );
                }
            }
        }
    }
    if matches!(format, ListFormat::Json) {
        println!("{}", serde_json::json!({"roots": reports}));
    }
    Ok(())
}

pub fn bytes(size: u64) -> String {
    humansize::format_size(size, humansize::BINARY)
}

fn candidate_rows(inventory: &Inventory, policy: &Policy) -> Vec<inventory::Row> {
    inventory
        .directories
        .iter()
        .filter(|row| policy.producer(&row.path).is_some())
        .cloned()
        .collect()
}
