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

use crate::config::rules::{Rule, expand_path};
use globset::{Glob, GlobSetBuilder};

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
                    self.base_path.starts_with(&sp) || sp.starts_with(&self.base_path)
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
                    if set.is_match(entry.path()) {
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
                (ActionType::Keep, "Symlinks are skipped in Phase 2".to_string())
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
                selected_action: recommended_action,
            });
        }

        out.sort_by(|a, b| b.size.cmp(&a.size));
        Ok(out)
    }
}

pub(crate) fn recommend(path: &Path, size: u64, modified: SystemTime, rules: &[Rule]) -> (ActionType, String) {
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
                path.starts_with(&sp)
            } else {
                false
            }
        });
        if !in_scope { continue; }

        // Check excludes (Global override for this rule)
        if let Some(excludes) = &rule.excludes {
            // Note: In a real implementation we'd use globset precompiled.
            let mut matched_exclude = false;
            for pattern in excludes {
                if let Ok(glob) = Glob::new(pattern) {
                    if glob.compile_matcher().is_match(path) {
                        matched_exclude = true;
                        break;
                    }
                }
            }
            if matched_exclude { continue; }
        }

        // Check includes (If defined, MUST match one)
        if let Some(includes) = &rule.includes {
            let mut matched_include = false;
            for pattern in includes {
                if let Ok(glob) = Glob::new(pattern) {
                    if glob.compile_matcher().is_match(path) {
                        matched_include = true;
                        break;
                    }
                }
            }
            if !matched_include { continue; }
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
        .checked_sub(std::time::Duration::from_secs(crate::policy::RULES_FALLBACK_OLD_DAYS * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    // “Large” threshold: default from policy
    let large_threshold: u64 = crate::policy::RULES_FALLBACK_LARGE_BYTES;

    let _base_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    
    let parent_name = path.parent()
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
        return (ActionType::Keep, format!("{} group; recent/small", parent_name));
    }

    if is_container_like {
        if is_old && is_large {
            return (ActionType::Quarantine, "Fallback: Containers group (policy default)".to_string());
        }
        return (ActionType::Keep, "Containers group; keep by default".to_string());
    }

    (ActionType::Keep, "Detected".to_string())
}
