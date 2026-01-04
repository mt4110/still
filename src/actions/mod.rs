pub mod quarantine;
pub mod trash;
pub mod vault;

use crate::plan::{Plan, ActionType};
use crate::error::AppError;
use std::io::Write;
use std::path::PathBuf;

pub fn execute_plan(plan: &Plan, yes: bool, vault_dir: Option<PathBuf>) -> Result<(), AppError> {
    // 1. Initialize journal
    crate::journal::writer::init()?;
    
    // 2. Initialize vault
    let vault = if let Some(path) = vault_dir {
        quarantine::Vault::new_with_root(path)
    } else {
        quarantine::Vault::new()
    }.map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

    crate::journal::writer::log("apply_start", serde_json::json!({
        "total_items": plan.entries.len()
    }));

    // 3. Collect actions
    let actions: Vec<_> = plan.entries.iter()
        .filter(|e| e.selected_action != ActionType::Keep)
        .collect();

    if actions.is_empty() {
        println!("No actions selected in plan. Exiting.");
        return Ok(());
    }

    println!("Found {} actions to execute.", actions.len());
    
    // 4. Confirm if not yes
    if !yes && crate::policy::APPLY_REQUIRES_CONFIRMATION {
        print!("Are you sure you want to proceed? [y/N] ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim().to_lowercase() != "y" {
            println!("Aborted.");
            crate::journal::writer::log("apply_aborted", serde_json::json!({
                "reason": "User declined confirmation"
            }));
            return Ok(());
        }
    }

    // 5. Execute
    let mut success_count = 0;
    let mut fail_count = 0;

    crate::journal::writer::log("apply_start", serde_json::json!({
        "total_items": plan.entries.len()
    }));

    for entry in actions {
        println!("Processing: {:?} -> {:?}", entry.path, entry.selected_action);

        // JIT Safety Check: Symlinks and Special Files
        let jit_check = || -> Result<(), AppError> {
            let md = std::fs::symlink_metadata(&entry.path).map_err(|e| AppError::Io(e))?;
            if md.is_symlink() {
                return Err(AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, "Security: Symlink rejected")));
            }
            #[cfg(unix)]
            {
                // Simplified check: reject if not file and not dir
                // This covers special files like devices, pipes, sockets (S_IFIFO, S_IFCHR, S_IFBLK, S_IFSOCK)
                if !md.is_file() && !md.is_dir() {
                    return Err(AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, "Security: Special file rejected")));
                }
            }
            Ok(())
        };

        if let Err(e) = jit_check() {
            eprintln!("Failed to process {:?}: {}", entry.path, e);
            crate::journal::writer::log("failed", serde_json::json!({
                "src": entry.path,
                "action": entry.selected_action,
                "error": e.to_string(),
                "reason": "JIT Safety Violation"
            }));
            fail_count += 1;
            continue;
        }

        let res = match entry.selected_action {
            ActionType::Quarantine => {
                vault.quarantine(&entry.path)
                    .map(|dest| {
                        crate::journal::writer::log("quarantine", serde_json::json!({
                            "src": entry.path,
                            "dest": dest,
                            "size": entry.size,
                            "reason": entry.reason
                        }));
                    })
            },
            ActionType::Trash => {
                crate::actions::trash::trash_path(&entry.path)?;
                crate::journal::writer::log("trash", serde_json::json!({
                    "src": entry.path,
                    "size": entry.size,
                    "reason": entry.reason
                }));
                Ok(())
            },
            ActionType::Purge => {
                return Err(AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, "FATAL: Purge action blocked by defensive double-lock.")));
            },
            ActionType::Keep => Ok(()), // Should be filtered out
        };

        match res {
            Ok(_) => success_count += 1,
            Err(e) => {
                eprintln!("Failed to process {:?}: {}", entry.path, e);
                crate::journal::writer::log("failed", serde_json::json!({
                    "src": entry.path,
                    "action": entry.selected_action,
                    "error": e.to_string(),
                    "reason": entry.reason
                }));
                fail_count += 1;
            }
        }
    }

    println!("Execution complete. Success: {}, Failed: {}", success_count, fail_count);
    
    crate::journal::writer::log("apply_end", serde_json::json!({
        "success": success_count,
        "failed": fail_count
    }));
    
    Ok(())
}
