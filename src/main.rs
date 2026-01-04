mod actions;
mod cli;
mod config;
mod error;
mod journal;
mod lang;
mod plan;
mod policy;
mod scan;
mod scope;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Critical Self-Check: Ensure embedded rules are valid at startup.
    // This protects against binary corruption or bad build artifacts.
    let embedded_rules = crate::policy::DEFAULT_RULES_TOML;
    if let Err(e) = toml::from_str::<config::rules::RuleSet>(embedded_rules) {
        eprintln!("CRITICAL ERROR: Embedded default rules are corrupt. Exiting for safety.");
        eprintln!("Details: {}", e);
        std::process::exit(1);
    }

    match &cli.command {
        Some(Commands::Scan(args)) => {
            // Handle --list-scopes
            if args.list_scopes {
                let scopes = crate::scope::all_scopes();
                match args.format {
                    cli::ListFormat::Json => {
                        let out = serde_json::json!({
                            "schema_version": "1.0",
                            "default_scope": "library-safe",
                            "scopes": scopes
                        });
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    }
                    cli::ListFormat::Text => {
                        println!("Scopes (source of truth: this binary)\n");
                        for s in scopes {
                            let safety_label = format!("[{}]", s.safety);
                            let default_label = if s.default { " (default)" } else { "" };
                            let alias_label = if !s.aliases.is_empty() { 
                                format!(" (alias -> {})", s.name) // Simplified visual for alias
                            } else { "".to_string() };
                            
                            println!("* {}{}{} {}", s.name, default_label, alias_label, safety_label);
                            println!("  - {}", s.description);
                            println!("  - roots:");
                            for r in &s.roots {
                                println!("    - {}", r);
                            }
                            println!("  - apply_compatible_by_default: {}", if s.apply_compatible_by_default { "yes" } else { "no" });
                            if let Some(w) = &s.warning {
                                println!("  - warning: {}", w);
                            }
                            println!();
                        }
                    }
                }
                return Ok(());
            }

            println!("Scanning...");
            let start = std::time::SystemTime::now();

            let target_paths = match args.scope {
                cli::Scope::LibrarySafe => {
                    let dirs = directories::UserDirs::new().ok_or(anyhow::anyhow!("Could not find user home"))?;
                    let library_path = dirs.home_dir().join("Library");
                    // We specifically want Caches, Logs for MVP (Containers excluded in v0.1.0)
                    crate::policy::DEFAULT_ALLOWED_SCOPES_SUBDIRS.iter()
                        .map(|subdir| library_path.join(subdir))
                        .collect()
                }
                cli::Scope::LibraryUnsafe => {
                    // Fetch official warning from Source of Truth
                    let scopes = crate::scope::all_scopes();
                    let unsafe_spec = scopes.iter().find(|s| s.name == "library-unsafe");
                    
                    if let Some(spec) = unsafe_spec {
                        eprintln!("WARNING: scope={}", spec.name);
                        if let Some(w) = &spec.warning {
                            eprintln!("{}", w);
                        }
                    } else {
                        // Fallback purely for safety if spec missing (should not happen)
                        eprintln!("WARNING: scope=library-unsafe scans the entire ~/Library. Use with caution.");
                    }
                    
                    let dirs = directories::UserDirs::new().ok_or(anyhow::anyhow!("Could not find user home"))?;
                    vec![dirs.home_dir().join("Library")]
                }
                cli::Scope::Custom => {
                    // For MVP custom not implemented via argument yet except just Library
                    println!("Custom scope not fully supported in MVP, scanning current dir for demo");
                    vec![std::env::current_dir()?]
                }
            };

            let mut all_entries = Vec::new();

            // Load rules
            // Load rules
            let rules_path = if let Some(path) = &args.rules {
                Some(path.clone())
            } else if let Some(proj_dirs) = directories::ProjectDirs::from("com", "antigravity", "still") {
                Some(proj_dirs.config_dir().join("rules.toml"))
            } else {
                None
            };

            let rules = if let Some(path) = rules_path {
                if path.exists() {
                    println!("Loading rules from {:?}", path);
                    match config::rules::load(&path) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("Error loading rules: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else if args.rules.is_some() {
                     // If explicit path given but not found, error out
                    eprintln!("Error: Rules file not found at {:?}", path);
                    std::process::exit(1);
                } else {
                    // Start of fallback to embedded rules
                    println!("No rules file found. Loading embedded default rules.");
                    let content = crate::policy::DEFAULT_RULES_TOML;
                    match toml::from_str::<config::rules::RuleSet>(content) {
                        Ok(rs) => rs.rules,
                        Err(e) => {
                             eprintln!("Critical: Failed to parse embedded rules: {}", e);
                             std::process::exit(1);
                        }
                    }
                }
            } else {
                // If we are here, it means we didn't search for a file (should not happen with logic above)
                vec![]
            };

            for path in target_paths {
                if !path.exists() {
                    println!("Skipping non-existent path: {:?}", path);
                    continue;
                }
                println!("Scanning: {:?}", path);
                let scanner = scan::walk::Scanner::new(path, rules.clone());
                match scanner.scan() {
                    Ok(mut entries) => all_entries.append(&mut entries),
                    Err(e) => eprintln!("Error scanning: {}", e),
                }
            }

            println!("Found {} entries", all_entries.len());

            let plan = plan::Plan {
                schema_version: "2.0".to_string(),
                generated_by: "still".to_string(),
                created_at: start,
                entries: all_entries,
            };

            if args.json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            }

            plan::io::save_plan(&plan, &args.out)?;
            println!("Plan saved to {:?}", args.out);
        }
        Some(Commands::Tui(args)) => {
            println!("Starting TUI with plan: {:?}", args.plan);
            let mut app = tui::app::App::new(args.plan.clone())?;
            app.run()?;
        }
        Some(Commands::Apply(args)) => {
            if args.dry_run {
                println!("(Dry Run) Applying plan from: {:?}", args.plan);
            } else {
                println!("Applying plan from: {:?}", args.plan);
            }
            let plan = plan::io::load_plan(&args.plan)?;

            // Construct Validation Context
            // For MVP, we enforce the same scopes as the default scan.
            // Ideally plan would store this, but strict default is safer.
            let mut allowed_scopes = Vec::new();
            if let Some(dirs) = directories::UserDirs::new() {
                let library_path = dirs.home_dir().join("Library");
                for subdir in crate::policy::DEFAULT_ALLOWED_SCOPES_SUBDIRS {
                    allowed_scopes.push(library_path.join(subdir));
                }
            }
            // Also allow the plan file's directory? No, that's not where content is.
            // What if user used custom scope (current dir)?
            // If so, they must use --allow-outside-scope. Default Deny.

            let ctx = plan::validator::ValidationContext {
                allowed_scopes,
                allow_outside_scope: args.allow_outside_scope,
            };

            // Safety Validator
            if let Err(e) = plan::validator::validate(&plan, &ctx) {
                eprintln!("{}", e);
                std::process::exit(2);
            }

            // Pre-flight Summary
            print_plan_summary(&plan);

            if args.dry_run {
                println!("Dry run complete. No changes made.");
            } else {
                actions::execute_plan(&plan, args.yes, args.vault_dir.clone())?;
            }
        }
        Some(Commands::Vault(args)) => {
            use cli::VaultCommands;
            // Logic moved to actions::vault
            
            match &args.command {
                VaultCommands::List { expired } => {
                    actions::vault::list(args.vault_dir.clone(), *expired)?;
                }
                VaultCommands::Restore { target } => {
                    actions::vault::restore(args.vault_dir.clone(), target.clone())?;
                }
                VaultCommands::Purge { target, expired } => {
                    actions::vault::purge(args.vault_dir.clone(), target.clone(), *expired)?;
                }
            }
        }
        None => {
            // Default behavior if no subcommand? Maybe print help.
            // For now, let's just print help.
            println!("No command specified. Run with --help for usage.");
        }
        Some(Commands::Init) => {
            if let Some(proj_dirs) = directories::ProjectDirs::from("com", "antigravity", "still") {
                let config_dir = proj_dirs.config_dir();
                if !config_dir.exists() {
                     std::fs::create_dir_all(config_dir)?;
                }
                let rules_path = config_dir.join("rules.toml");
                if rules_path.exists() {
                    eprintln!("Config already exists at {:?}. Skipping init.", rules_path);
                    return Ok(());
                }
                
                std::fs::write(&rules_path, crate::policy::DEFAULT_RULES_TOML)?;
                println!("Initialized default rules at {:?}", rules_path);
            } else {
                eprintln!("Could not determine config directory.");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

fn print_plan_summary(plan: &crate::plan::Plan) {
    use crate::plan::ActionType;
    let mut counts = std::collections::HashMap::new();
    let mut sizes = std::collections::HashMap::new();

    for entry in &plan.entries {
        if entry.selected_action == ActionType::Keep { continue; }
        *counts.entry(entry.selected_action).or_insert(0) += 1;
        *sizes.entry(entry.selected_action).or_insert(0) += entry.size;
    }

    println!("--- Plan Summary ---");
    // Explicitly iterate expected actions for consistent order
    // Purge is strictly for Vault maintenance (still vault purge) and never for apply.
    for action in [ActionType::Quarantine, ActionType::Trash] {
        if let Some(count) = counts.get(&action) {
            let size = sizes.get(&action).unwrap_or(&0);
            println!("{:?}: {} items ({})", action, count, humansize::format_size(*size, humansize::DECIMAL));
        }
    }
    // Also print total scanned? Or just actions?
    // "Action (Count / Total Size)" implies breakdown.
    // Maybe total found:
    println!("Total scanned entries: {}", plan.entries.len());
    println!("--------------------");
}
