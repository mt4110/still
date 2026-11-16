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
    #[command(about = "Explore storage and review generated output for cleanup")]
    Review(ReviewArgs),
    #[command(about = "Review disposable Git worktrees integrated into the main branch")]
    Worktrees(WorktreeArgs),
    #[command(about = "Review unused Docker resources on an explicitly selected local context")]
    Docker(DockerArgs),
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
pub struct DockerArgs {
    /// Docker context for a local daemon.
    #[arg(long)]
    pub context: String,

    /// Include stale cache from the context's built-in Buildx builder.
    #[arg(long)]
    pub include_cache: bool,

    /// Minimum age for stopped containers and BuildKit cache.
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u16).range(1..=365))]
    pub older_than_days: u16,

    /// Select the exact resource key shown in the report.
    #[arg(long)]
    pub remove: Option<String>,

    #[arg(long, value_enum, default_value_t = ListFormat::Text, conflicts_with = "remove")]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct WorktreeArgs {
    /// Primary repository checkout.
    #[arg(long)]
    pub repo: PathBuf,

    /// Directory containing disposable linked worktrees.
    #[arg(long)]
    pub root: PathBuf,

    /// Full Git ref used as the integration baseline.
    #[arg(long, default_value = "refs/heads/main")]
    pub base: String,

    /// Look up a merged pull request on the GitHub origin (requires gh).
    #[arg(long = "github-pr")]
    pub github_prs: Vec<u64>,

    /// Select the exact worktree path for an interactive removal confirmation.
    #[arg(long)]
    pub remove: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = ListFormat::Text, conflicts_with = "remove")]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Directory to explore. Defaults to the cleanup roots or the current directory.
    #[arg(long = "root")]
    pub roots: Vec<PathBuf>,

    /// Directory in which verified generated output may be selected for deletion.
    #[arg(long = "cleanup-root")]
    pub cleanup_roots: Vec<PathBuf>,

    /// Treat an exact path as a dedicated, reproducible script output directory.
    #[arg(long = "output", requires = "cleanup_roots")]
    pub outputs: Vec<PathBuf>,

    /// Protect bind mounts referenced by containers on this local Docker context.
    #[arg(long, requires = "cleanup_roots")]
    pub docker_context: Option<String>,

    /// Search recursively for a case-insensitive substring of a path.
    #[arg(long)]
    pub search: Option<String>,

    /// Walker threads per root.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=32))]
    pub threads: u16,

    /// Print a read-only report instead of opening the explorer.
    #[arg(long, value_enum)]
    pub format: Option<ListFormat>,
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
