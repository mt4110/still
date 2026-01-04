use anyhow::Result;
use std::path::PathBuf;
use std::io::Write;
use crate::actions::quarantine::Vault;

fn get_vault(root: Option<PathBuf>) -> Result<Vault> {
    if let Some(r) = root {
        Vault::new_with_root(r)
    } else {
        Vault::new()
    }
}

pub fn list(vault_dir: Option<PathBuf>, expired_only: bool) -> Result<()> {
    let vault = get_vault(vault_dir)?;
    let items = vault.list()?;
    if items.is_empty() {
        println!("Vault is empty.");
    } else {
        println!("Vault items (newest first):");
        let now = chrono::Utc::now();
        for (id, meta) in items {
            let is_expired = if let Some(expires_at) = meta.expires_at {
                expires_at < now
            } else {
                false
            };

            if expired_only && !is_expired {
                continue;
            }
            
            let expiry_text = if let Some(_expires_at) = meta.expires_at {
                if is_expired { "[EXPIRED]" } else { "" }
            } else {
                ""
            };
            
            println!("- {id}  {}  {} {}", meta.quarantined_at.to_rfc3339(), meta.original_path.display(), expiry_text);
        }
    }
    Ok(())
}

pub fn restore(vault_dir: Option<PathBuf>, target: String) -> Result<()> {
    let vault = get_vault(vault_dir)?;
    let restored = vault.restore(&target)?;
    println!("Restored to: {}", restored.display());
    Ok(())
}

pub fn purge(vault_dir: Option<PathBuf>, target: Option<String>, expired: bool) -> Result<()> {
    let vault = get_vault(vault_dir)?;

    if target.is_none() && !expired {
        eprintln!("Error: Must specify either <target> or --expired");
        std::process::exit(1);
    }
    
    println!("WARNING: This action is permanent and cannot be undone.");
    println!("To confirm, please type exactly: {}", crate::policy::PURGE_RITUAL_PHRASE);
    
    print!("> ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim() != crate::policy::PURGE_RITUAL_PHRASE {
        println!("Confirmation failed. Aborted.");
        return Ok(());
    }

    // Initialize audit log
    crate::journal::writer::init()?;
    
    // Policy constant unified to DEFAULT_VAULT_TTL_DAYS (i64)
    let ttl_days = crate::policy::DEFAULT_VAULT_TTL_DAYS;

    if expired {
        println!("Scanning for items older than {} days...", ttl_days);
        crate::journal::writer::log("vault_purge_start", serde_json::json!({
            "mode": "expired", 
            "ttl_days": ttl_days
        }));
        
        // Use vault.list() + manual check to ensure we control the policy matches the log
        let items = vault.list()?;
        let now = chrono::Utc::now();
        let mut purged_count = 0;
        
        for (id, meta) in items {
            if let Some(expiry) = meta.expires_at {
                if expiry < now {
                    if let Err(e) = vault.purge(&id) {
                        eprintln!("Failed to purge {}: {}", id, e);
                        crate::journal::writer::log("vault_purge_error", serde_json::json!({ "id": id, "error": e.to_string() }));
                    } else {
                        println!("Purged expired: {} ({})", id, meta.original_path.display());
                        crate::journal::writer::log("vault_purge_item", serde_json::json!({ "id": id, "original_path": meta.original_path }));
                        purged_count += 1;
                    }
                }
            }
        }
        
        crate::journal::writer::log("vault_purge_end", serde_json::json!({ "purged_count": purged_count }));
        println!("Purged {} expired items.", purged_count);
    }
    
    if let Some(t) = target {
        crate::journal::writer::log("vault_purge_start", serde_json::json!({ "mode": "target", "target": t }));
        match vault.purge(&t) {
            Ok(_) => {
                println!("Purged vault item: {t}");
                crate::journal::writer::log("vault_purge_item", serde_json::json!({ "id": t }));
                crate::journal::writer::log("vault_purge_end", serde_json::json!({ "purged_count": 1 }));
            },
            Err(e) => {
                eprintln!("Failed to purge {}: {}", t, e);
                crate::journal::writer::log("vault_purge_error", serde_json::json!({ "id": t, "error": e.to_string() }));
            }
        }
    }
    Ok(())
}
