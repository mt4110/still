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
    #[command(about = "Open the desktop workspace explorer")]
    Gui(DesktopArgs),
    #[command(about = "Review disposable Git worktrees integrated into the main branch")]
    Worktrees(WorktreeArgs),
    #[command(about = "Review unused Docker resources on an explicitly selected local context")]
    Docker(DockerArgs),
    #[command(
        about = "Review finished Jobs and Pods on an explicitly selected local Kubernetes or K3s context"
    )]
    Kube(KubeArgs),
    #[command(about = "Find large, unmodified files under your home directory for manual review")]
    Stale(StaleArgs),
    #[command(about = "Review Mac app storage, SDK versions, caches, and protected data")]
    AppStorage(AppStorageArgs),
    #[cfg(target_os = "macos")]
    #[command(about = "Check one selected application's signed App Group declaration")]
    GroupLink(GroupLinkArgs),
    #[command(about = "Read locally recorded desktop cleanup results")]
    CleanupHistory(CleanupHistoryArgs),
    #[command(about = "Read recorded workspace scan totals over time")]
    ReviewHistory(ReviewHistoryArgs),
    #[cfg(target_os = "macos")]
    #[command(about = "Review Finder metadata left in a cleanup stage")]
    StageCleanup(StageCleanupArgs),
    #[command(about = crate::lang::text().tui)]
    Tui(TuiArgs),
    #[command(about = crate::lang::text().apply)]
    Apply(ApplyArgs),
    #[command(about = crate::lang::text().vault)]
    Vault(VaultArgs),
    #[command(about = crate::lang::text().init)]
    Init,
}

#[cfg(target_os = "macos")]
#[derive(Args, Debug)]
pub struct StageCleanupArgs {
    /// Review exactly one .still-stage-UUID folder.
    pub stage: PathBuf,
    /// Explicitly authorized workspace containing the stage.
    #[arg(long)]
    pub cleanup_root: PathBuf,
    /// Permanently remove the reviewed metadata and empty stage directories.
    #[arg(long, conflicts_with = "restore")]
    pub apply: bool,
    /// Move the remaining payload back to the recorded sibling path, without overwriting it.
    #[arg(long)]
    pub restore: bool,
}

#[derive(Args, Debug)]
pub struct CleanupHistoryArgs {
    /// Maximum number of recent operations to display.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
    pub limit: u16,
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct ReviewHistoryArgs {
    /// Workspace root from the recorded scan (absolute path).
    #[arg(long)]
    pub root: PathBuf,
    /// Read a named review profile's history instead of the desktop workspace history.
    #[arg(long)]
    pub profile: Option<String>,
    /// Maximum number of recent observations to display.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
    pub limit: u16,
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct AppStorageArgs {
    /// Also ask Xcode for its installed simulator runtime inventory.
    #[arg(long)]
    pub xcode_runtimes: bool,
    /// Also inventory immediate folders in selected Library locations (read-only).
    #[arg(long)]
    pub library: bool,
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

#[cfg(target_os = "macos")]
#[derive(Args, Debug)]
pub struct GroupLinkArgs {
    /// Explicitly selected .app bundle to inspect; its code is not launched.
    #[arg(long)]
    pub app: PathBuf,
    /// Exact App Group identifier to compare.
    #[arg(long)]
    pub group: String,
}

#[derive(clap::ValueEnum, Clone, Copy, Default, Debug, Eq, PartialEq)]
pub enum StaleSort {
    #[default]
    Size,
    Modified,
    Created,
}

#[derive(Args, Debug)]
pub struct StaleArgs {
    /// Directory to inspect (defaults to your home directory).
    #[arg(long)]
    pub root: Option<PathBuf>,
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u16).range(1..=3650))]
    pub older_than_days: u16,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u64).range(1..))]
    pub min_mib: u64,
    /// Rank matches by largest size or oldest modification or creation date.
    #[arg(long, value_enum, default_value_t = StaleSort::Size)]
    pub sort: StaleSort,
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct KubeArgs {
    #[arg(long)]
    pub kubeconfig: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

#[derive(Args, Debug)]
pub struct DesktopArgs {
    /// Open a saved weekly review profile.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
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
#[command(group(clap::ArgGroup::new("review_roots").args(["roots", "cleanup_roots"]).multiple(true)))]
pub struct ReviewArgs {
    /// Load saved review roots and cleanup settings.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["save_profile", "roots", "cleanup_roots", "outputs", "docker_context", "docker_cache", "git_repos", "git_base", "kubeconfig", "kube_context", "kube_namespace"])]
    pub profile: Option<String>,

    /// Save the specified roots and cleanup settings as a named profile.
    #[arg(long, value_name = "NAME", requires = "review_roots")]
    pub save_profile: Option<String>,

    /// Directory to explore. Defaults to the cleanup roots or the current directory.
    #[arg(long = "root")]
    pub roots: Vec<PathBuf>,

    /// Directory in which verified generated output may be selected for deletion.
    #[arg(long = "cleanup-root")]
    pub cleanup_roots: Vec<PathBuf>,

    /// Treat an exact path as a dedicated, reproducible script output directory.
    #[arg(long = "output", requires = "cleanup_roots")]
    pub outputs: Vec<PathBuf>,

    /// Show Docker candidates and protect bind mounts on this local context.
    #[arg(long, requires = "cleanup_roots")]
    pub docker_context: Option<String>,

    /// Include the selected Docker context's local BuildKit cache in the review.
    #[arg(long, requires = "docker_context")]
    pub docker_cache: bool,

    /// Primary Git checkout whose linked worktrees should appear in the review.
    #[arg(long = "git-repo", requires = "cleanup_roots")]
    pub git_repos: Vec<PathBuf>,

    /// Integration ref for the selected Git repositories (default: refs/heads/main).
    #[arg(long, requires = "git_repos")]
    pub git_base: Option<String>,

    /// Read-only Kubernetes source, with an explicit context and namespace.
    #[arg(long, requires_all = ["kube_context", "kube_namespace"])]
    pub kubeconfig: Option<PathBuf>,
    #[arg(long, requires = "kubeconfig")]
    pub kube_context: Option<String>,
    #[arg(long, requires = "kubeconfig")]
    pub kube_namespace: Option<String>,

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

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{error::ErrorKind, Parser};

    #[test]
    fn saving_review_profile_requires_an_explicit_scope() {
        let error =
            Cli::try_parse_from(["still", "review", "--save-profile", "weekly"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);

        assert!(Cli::try_parse_from([
            "still",
            "review",
            "--save-profile",
            "weekly",
            "--root",
            "source",
            "--cleanup-root",
            "builds",
            "--format",
            "json",
        ])
        .is_ok());
    }

    #[test]
    fn review_profile_scope_cannot_be_overridden() {
        let error = Cli::try_parse_from([
            "still",
            "review",
            "--profile",
            "weekly",
            "--cleanup-root",
            "somewhere-else",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }
}
