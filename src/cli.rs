use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = crate::lang::text().about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = crate::lang::text().scan)]
    Scan(ScanArgs),
    #[command(about = crate::lang::text().tui)]
    Tui(TuiArgs),
    #[command(about = crate::lang::text().apply)]
    Apply(ApplyArgs),
    #[command(about = crate::lang::text().vault)]
    Vault(VaultArgs),
    #[command(about = crate::lang::text().init)]
    Init,
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    #[arg(long, value_enum, default_value_t = Scope::LibrarySafe, help = crate::lang::text().scan_scope)]
    pub scope: Scope,

    #[arg(short, long, default_value = "./plan.json", help = crate::lang::text().scan_out)]
    pub out: PathBuf,

    #[arg(long, help = crate::lang::text().scan_rules)]
    pub rules: Option<PathBuf>,

    #[arg(long, help = crate::lang::text().scan_json)]
    pub json: bool,

    #[arg(long, help = crate::lang::text().scan_list_scopes)]
    pub list_scopes: bool,

    #[arg(long, value_enum, default_value_t = ListFormat::Text, help = crate::lang::text().scan_format)]
    pub format: ListFormat,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum ListFormat {
    #[default]
    Text,
    Json,
}

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Scope {
    #[value(alias = "library")]
    LibrarySafe,
    LibraryUnsafe,
    Custom,
}

#[derive(Args, Debug)]
pub struct TuiArgs {
    #[arg(default_value = "./plan.json", help = crate::lang::text().tui_plan)]
    pub plan: PathBuf,
}

#[derive(Args, Debug)]
pub struct ApplyArgs {
    #[arg(default_value = "./plan.json", help = crate::lang::text().apply_plan)]
    pub plan: PathBuf,

    #[arg(long, short, help = crate::lang::text().apply_yes)]
    pub yes: bool,

    #[arg(long, help = crate::lang::text().apply_dry_run)]
    pub dry_run: bool,

    #[arg(long, help = crate::lang::text().apply_allow_outside)]
    pub allow_outside_scope: bool,

    #[arg(long, help = crate::lang::text().apply_vault_dir)]
    pub vault_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct VaultArgs {
    #[arg(long, global = true, help = crate::lang::text().vault_dir)]
    pub vault_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: VaultCommands,
}

#[derive(Subcommand, Debug)]
pub enum VaultCommands {
    #[command(about = "List vault items")]
    // Fallback or add to lang if strictly needed, user didn't ask for full coverage but good practice.
    List {
        #[arg(long, help = crate::lang::text().vault_list_expired)]
        expired: bool,
    },
    #[command(about = "Restore item")]
    Restore {
        #[arg(help = crate::lang::text().vault_restore_target)]
        target: String,
    },
    #[command(about = "Purge item")]
    Purge {
        #[arg(help = crate::lang::text().vault_purge_target)]
        target: Option<String>,

        #[arg(long, help = crate::lang::text().vault_purge_expired)]
        expired: bool,
    },
}
