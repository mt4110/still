/// Quantitative Policy Constants for `still`.
///
/// This file centralizes all "tuning knobs" and quantitative policies.
/// Any number that affects behavior (TTL, Fallback thresholds, Safety limits)
/// MUST be defined here with a clear rationale comment.

// --- Vault Policies ---

/// Default Time-To-Live (days) for quarantined items.
/// Used when initializing Vault metadata if no specific rule overrides it.
pub const DEFAULT_VAULT_TTL_DAYS: i64 = 30;

/// The exact phrase required to confirm a `still vault purge` action.
pub const PURGE_RITUAL_PHRASE: &str = "PURGE STILL";

// --- Scanning & Validation Policies ---

/// Default approved subdirectories in ~/Library to scan.
/// Stricter scopes can be defined in rules, but this is the hardcoded safety net.
pub const DEFAULT_ALLOWED_SCOPES_SUBDIRS: &[&str] = &[
    "Caches",
    "Logs",
];

// --- Fallback Policies (Emergency Use Only) ---

/// Fallback threshold for "Old Files" when NO rules match.
/// Rationale: If rules.toml is missing or broken, we default to a safe, conservative value (30 days).
/// This prevents the tool from doing nothing or doing too much in a broken config state.
pub const RULES_FALLBACK_OLD_DAYS: u64 = 30;

/// Fallback threshold for "Large Files" when NO rules match.
/// Rationale: 200MB is large enough to be annoying but safe enough not to flag everything.
pub const RULES_FALLBACK_LARGE_BYTES: u64 = 200 * 1024 * 1024; // 200MB

// --- Default Rules (Embedded) ---

/// The default `rules.toml` content embedded in the binary.
/// This ensures `still` works safely out-of-the-box even if distributed as a single executable.
pub const DEFAULT_RULES_TOML: &str = include_str!("../rules/default/library.toml");

// --- TUI Policies ---

/// TUI refresh rate in milliseconds.
pub const TUI_REFRESH_MS: u64 = 250;

/// Maximum number of rows to render in TUI to prevent lag on huge lists.
/// The list is virtualized, but this is a safety cap for the view buffer.
pub const TUI_MAX_RENDER_ROWS: usize = 5000;

/// Maximum length of the filter query string.
pub const TUI_FILTER_QUERY_MAX_LEN: usize = 128;

// --- Application Policies ---

// --- Application Policies ---

/// Application Language: "EN" or "JP"
pub const APP_LANG: &str = "JP";

/// Plan Schema Version. Single source of truth.
pub const PLAN_SCHEMA_VERSION: &str = "2.0";

/// Default confirmation behavior for apply (y/N).
/// Currently hardcoded to require explicit 'y', but defined here for policy visibility.
pub const APPLY_REQUIRES_CONFIRMATION: bool = true;
