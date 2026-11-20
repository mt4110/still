use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::AppError;
use crate::plan::{ActionType, Entry, EntryType, PlanOrigin};
use crate::scan::classify::determine_entry_type;

/// Scan strategy notes
/// -------------------
///
/// The first prototype enumerated *every* file under ~/Library/Caches etc.
/// That explodes into tens/hundreds of thousands of rows, which makes the
/// plan/TUI unusable.
///
/// For the MVP, we aggregate at **depth=1** (direct children of the base path)
/// and compute:
/// - total size (sum of descendant file sizes)
/// - latest modified time (max)
///
/// This yields a compact, human-reviewable list like:
///   ~/Library/Caches/com.google.Chrome
///   ~/Library/Caches/com.apple.dt.Xcode
///
/// Later we can add modes (raw / depth=N / rule-driven).
use crate::config::rules::{expand_path, Rule};
use globset::{Glob, GlobSetBuilder};

// Detect common DOS 8.3 spellings, including glob patterns that could match
// them. This is deliberately conservative and is not a complete 8.3 detector.
#[cfg(windows)]
fn looks_like_short_name(value: &str) -> bool {
    value.as_bytes().windows(2).any(|pair| {
        pair[0] == b'~' && (pair[1].is_ascii_digit() || matches!(pair[1], b'[' | b'?' | b'*'))
    })
}

#[cfg(windows)]
fn reject_ambiguous_short_names(base: &Path, rules: &[Rule]) -> Result<(), AppError> {
    let root_has_short_name = looks_like_short_name(&base.to_string_lossy());
    let rule_has_short_name = rules.iter().any(|rule| {
        rule.scopes.iter().any(|scope| {
            expand_path(scope).is_ok_and(|path| looks_like_short_name(&path.to_string_lossy()))
        }) || rule
            .includes
            .iter()
            .chain(rule.excludes.iter())
            .flatten()
            .any(|pattern| looks_like_short_name(pattern))
    });
    if root_has_short_name || rule_has_short_name {
        return Err(AppError::Scan("A scan path, rule scope, or glob resembles a Windows 8.3 short name. Use the long spelling so exclusions can be checked safely.".into()));
    }
    Ok(())
}

/// Compare scopes lexically. On Windows, a canonicalized local path may have a
/// verbatim drive prefix while the same path from current_dir does not. Only
/// bridge those spellings when every component has ordinary Win32 semantics;
/// do not resolve links or change the apply-time authorization checks.
#[cfg(not(windows))]
pub(crate) fn scope_contains(path: &Path, scope: &Path) -> bool {
    path.starts_with(scope)
}

#[cfg(windows)]
pub(crate) fn scope_contains(path: &Path, scope: &Path) -> bool {
    path.starts_with(scope)
        || match (alternate_windows_path(path), alternate_windows_path(scope)) {
            (Some(_), Some(alternate_scope)) => path.starts_with(alternate_scope),
            _ => false,
        }
}

#[cfg(windows)]
fn alternate_windows_path(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let Component::Prefix(prefix) = components.next()? else {
        return None;
    };
    let mut alternative = match prefix.kind() {
        Prefix::Disk(drive) => PathBuf::from(format!(r"\\?\{}:\", drive as char)),
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!(r"{}:\", drive as char)),
        _ => return None,
    };
    if components.next()? != Component::RootDir {
        return None;
    }
    for component in components {
        let Component::Normal(name) = component else {
            return None;
        };
        let name_text = name.to_str()?;
        if name_text.ends_with(['.', ' '])
            || name_text
                .chars()
                .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
        {
            return None;
        }
        let stem = name_text
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) {
            return None;
        }
        for prefix in ["COM", "LPT"] {
            if let Some(suffix) = stem.strip_prefix(prefix) {
                if matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                ) {
                    return None;
                }
            }
        }
        alternative.push(name);
    }
    Some(alternative)
}

#[cfg(not(windows))]
pub(crate) fn glob_matches(path: &Path, matches: impl Fn(&Path) -> bool) -> bool {
    matches(path)
}

#[cfg(windows)]
pub(crate) fn glob_matches(path: &Path, matches: impl Fn(&Path) -> bool) -> bool {
    matches(path) || alternate_windows_path(path).is_some_and(|alternative| matches(&alternative))
}

pub struct Scanner {
    base_path: PathBuf,
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Agg {
    size: u64,
    modified: SystemTime,
}

impl Default for Agg {
    fn default() -> Self {
        Self {
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
        }
    }
}

impl Scanner {
    pub fn new(base_path: PathBuf, rules: Vec<Rule>) -> Self {
        Self { base_path, rules }
    }

    pub fn scan(&self) -> Result<Vec<Entry>, AppError> {
        #[cfg(windows)]
        reject_ambiguous_short_names(&self.base_path, &self.rules)?;
        if !self.base_path.exists() {
            return Ok(vec![]);
        }

        // Build GlobSet for excludes from applicable rules
        let mut builder = GlobSetBuilder::new();
        let mut has_excludes = false;

        for rule in &self.rules {
            let applies = rule.scopes.iter().any(|s| {
                if let Ok(sp) = expand_path(s) {
                    // Rule applies if we are scanning inside the scope, or the scope is inside what we are scanning
                    scope_contains(&self.base_path, &sp) || scope_contains(&sp, &self.base_path)
                } else {
                    false
                }
            });

            if applies {
                if let Some(excludes) = &rule.excludes {
                    for pattern in excludes {
                        if let Ok(glob) = Glob::new(pattern) {
                            builder.add(glob);
                            has_excludes = true;
                        }
                    }
                }
            }
        }

        let exclude_set = if has_excludes {
            Some(builder.build().map_err(|e| AppError::Scan(e.to_string()))?)
        } else {
            None
        };

        let mut agg: HashMap<PathBuf, Agg> = HashMap::new();

        let walker = WalkBuilder::new(&self.base_path)
            .standard_filters(false)
            .hidden(false)
            .follow_links(false)
            .filter_entry(move |entry| {
                if let Some(set) = &exclude_set {
                    if glob_matches(entry.path(), |path| set.is_match(path)) {
                        return false;
                    }
                }
                true
            })
            .build();

        for result in walker {
            let entry = match result {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Scan error: {}", err);
                    continue;
                }
            };

            let path = entry.path();
            if path == self.base_path {
                continue;
            }

            // Resolve group (direct child of base_path)
            let rel = match path.strip_prefix(&self.base_path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let first = match rel.components().next() {
                Some(c) => c.as_os_str(),
                None => continue,
            };
            let group_path = self.base_path.join(first);

            // Metadata: skip if unavailable (permission denied etc.)
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Aggregate only file sizes; for dirs we count via descendant files.
            let add = if md.is_file() { md.len() } else { 0 };
            let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            let slot = agg.entry(group_path).or_default();
            slot.size = slot.size.saturating_add(add);
            if modified > slot.modified {
                slot.modified = modified;
            }
        }

        // Convert aggregated groups into Entries
        let mut out = Vec::with_capacity(agg.len());
        for (group_path, a) in agg {
            let md = match std::fs::metadata(&group_path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let entry_type: EntryType = determine_entry_type(&group_path, &md);

            let (recommended_action, reason) = if entry_type == EntryType::Symlink {
                (
                    ActionType::Keep,
                    "Symlinks are skipped in Phase 2".to_string(),
                )
            } else {
                recommend(&group_path, a.size, a.modified, &self.rules)
            };

            out.push(Entry {
                path: group_path,
                size: a.size,
                modified: a.modified,
                entry_type,
                origin: PlanOrigin::Scan,
                recommended_action,
                reason,
                // Recommendations are informational until the user explicitly selects an action.
                selected_action: ActionType::Keep,
            });
        }

        out.sort_by_key(|entry| std::cmp::Reverse(entry.size));
        Ok(out)
    }
}

pub(crate) fn recommend(
    path: &Path,
    size: u64,
    modified: SystemTime,
    rules: &[Rule],
) -> (ActionType, String) {
    // 0. Safety First: Missing metadata? Matches nothing -> Keep.
    // In this MVP 'size' and 'modified' are passed in. If they were default (UNIX_EPOCH or 0),
    // strictly speaking we should treat it as "unknown".
    // For now, if modified is UNIX_EPOCH, we assume invalid.
    if modified == SystemTime::UNIX_EPOCH {
        return (ActionType::Keep, "Missing metadata (mtime)".to_string());
    }

    // 1. Check Rules
    for rule in rules {
        // Check scope
        let in_scope = rule.scopes.iter().any(|s| {
            if let Ok(sp) = expand_path(s) {
                scope_contains(path, &sp)
            } else {
                false
            }
        });
        if !in_scope {
            continue;
        }

        // Check excludes (Global override for this rule)
        if let Some(excludes) = &rule.excludes {
            // Note: In a real implementation we'd use globset precompiled.
            let mut matched_exclude = false;
            for pattern in excludes {
                if let Ok(glob) = Glob::new(pattern) {
                    let matcher = glob.compile_matcher();
                    if glob_matches(path, |path| matcher.is_match(path)) {
                        matched_exclude = true;
                        break;
                    }
                }
            }
            if matched_exclude {
                continue;
            }
        }

        // Check includes (If defined, MUST match one)
        if let Some(includes) = &rule.includes {
            let mut matched_include = false;
            for pattern in includes {
                if let Ok(glob) = Glob::new(pattern) {
                    let matcher = glob.compile_matcher();
                    if glob_matches(path, |path| matcher.is_match(path)) {
                        matched_include = true;
                        break;
                    }
                }
            }
            if !matched_include {
                continue;
            }
        }

        // Check thresholds
        // Age
        if let Some(days) = rule.older_than_days {
            let threshold = SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(days * 24 * 60 * 60))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if modified >= threshold {
                continue; // Too new
            }
        }

        // Size (Files only - directories passed here have aggregated size, but "min_size_bytes" usually implies single file check?
        // The spec summary said "Files only". But 'path' here is a Group (depth=1).
        // If it's a file group, size is file size. If dir group, size is agg.
        // Let's apply it generally: if total size < min, skip.
        if let Some(min_bytes) = rule.min_size_bytes {
            if size < min_bytes {
                continue; // Too small
            }
        }

        // Match!
        let action = rule.recommended_action.unwrap_or(ActionType::Quarantine);
        let desc = rule.description.as_deref().unwrap_or(&rule.name);
        return (action, format!("Rule: {}", desc));
    }

    // 2. Default Heuristics (Fallback)
    // “Old” threshold: default from policy
    let old_threshold = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            crate::policy::RULES_FALLBACK_OLD_DAYS * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    // “Large” threshold: default from policy
    let large_threshold: u64 = crate::policy::RULES_FALLBACK_LARGE_BYTES;

    let _base_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let is_cache_like = matches!(parent_name, "Caches" | "Logs");
    let is_container_like = matches!(parent_name, "Containers");

    let is_old = modified < old_threshold;
    let is_large = size >= large_threshold;

    if is_cache_like {
        if is_old || is_large {
            return (
                ActionType::Quarantine,
                format!("Fallback: {} group (policy default)", parent_name),
            );
        }
        return (
            ActionType::Keep,
            format!("{} group; recent/small", parent_name),
        );
    }

    if is_container_like {
        if is_old && is_large {
            return (
                ActionType::Quarantine,
                "Fallback: Containers group (policy default)".to_string(),
            );
        }
        return (
            ActionType::Keep,
            "Containers group; keep by default".to_string(),
        );
    }

    (ActionType::Keep, "Detected".to_string())
}
