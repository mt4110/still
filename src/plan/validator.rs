use crate::plan::{ActionType, Plan};
use anyhow::{bail, Result};

use std::path::{Component, Path, PathBuf};

pub struct ValidationContext {
    pub allowed_scopes: Vec<PathBuf>,
    pub allow_outside_scope: bool,
}

pub fn validate(plan: &Plan, ctx: &ValidationContext) -> Result<()> {
    // 1. Schema Enforcement
    if plan.schema_version != crate::policy::PLAN_SCHEMA_VERSION {
        bail!("Validation Error: Unsupported schema_version '{}'. Expected '{}'. Please upgrade 'still' or regenerate the plan.", plan.schema_version, crate::policy::PLAN_SCHEMA_VERSION);
    }
    if plan.generated_by.trim().is_empty() {
        bail!("Validation Error: Plan missing 'generated_by' field.");
    }

    for entry in &plan.entries {
        // 2. Strict Purge Block
        if entry.selected_action == ActionType::Purge {
            bail!(
                "Validation Error: Plan contains forbidden ActionType::Purge for entry: {:?}",
                entry.path
            );
        }

        // 3. Absolute Path Requirement
        if !entry.path.is_absolute() {
            bail!(
                "Validation Error: Plan entry path must be absolute: {:?}",
                entry.path
            );
        }

        // 4. Scope Containment (Default Deny)
        // Enforce only for active actions. Keep is harmless.
        if entry.selected_action != ActionType::Keep && !ctx.allow_outside_scope {
            // Lexical check: entry.path must be contained in at least one allowed_scope
            let mut contained = false;
            for scope in &ctx.allowed_scopes {
                if is_subpath(&entry.path, scope) {
                    contained = true;
                    break;
                }
            }
            if !contained {
                bail!("Validation Error: Path {:?} is outside allowed scopes {:?}. Use --allow-outside-scope to override.", entry.path, ctx.allowed_scopes);
            }
        }
    }
    Ok(())
}

/// Lexically check if `path` is a subpath of `base` without accessing filesystem.
/// Normalizes `.` and `..` components purely lexically.
fn is_subpath(path: &Path, base: &Path) -> bool {
    let path_comps = normalize_components(path);
    let base_comps = normalize_components(base);

    if base_comps.len() > path_comps.len() {
        return false;
    }

    path_comps
        .iter()
        .zip(base_comps.iter())
        .all(|(a, b)| a == b)
}

fn normalize_components(path: &Path) -> Vec<Component<'_>> {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            c => normalized.push(c),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ActionType, Entry, EntryType, Plan, PlanOrigin};
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_entry(action: ActionType) -> Entry {
        Entry {
            path: PathBuf::from("/test/path"),
            size: 100,
            modified: SystemTime::UNIX_EPOCH,
            entry_type: EntryType::File,
            origin: PlanOrigin::Scan,
            recommended_action: ActionType::Keep,
            reason: "test".to_string(),
            selected_action: action,
        }
    }

    fn default_ctx() -> ValidationContext {
        ValidationContext {
            allowed_scopes: vec![PathBuf::from(
                "/Users/masakitakemura/_workspace/still/test_scope",
            )],
            allow_outside_scope: true, // Allow by default for simple tests unless testing scope
        }
    }

    #[test]
    fn test_validate_keep() {
        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![make_entry(ActionType::Keep)],
        };
        assert!(validate(&plan, &default_ctx()).is_ok());
    }

    #[test]
    fn test_validate_trash() {
        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![make_entry(ActionType::Trash)],
        };
        assert!(validate(&plan, &default_ctx()).is_ok());
    }

    #[test]
    fn test_validate_purge_blocked() {
        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![make_entry(ActionType::Purge)],
        };
        assert!(validate(&plan, &default_ctx()).is_err());
    }

    #[test]
    fn test_validate_scope_containment() {
        let allowed = PathBuf::from("/allowed");
        let ctx = ValidationContext {
            allowed_scopes: vec![allowed.clone()],
            allow_outside_scope: false,
        };

        // Good path
        let mut entry = make_entry(ActionType::Keep);
        entry.path = PathBuf::from("/allowed/child");
        let plan_good = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![entry.clone()],
        };
        assert!(validate(&plan_good, &ctx).is_ok());

        // Bad path but Action::Keep -> Should PASS now (Safe by default)
        entry.path = PathBuf::from("/forbidden/child");
        let plan_bad_keep = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![entry.clone()],
        };
        assert!(validate(&plan_bad_keep, &ctx).is_ok());

        // Bad path with Action::Quarantine -> Should FAIL
        let mut entry_q = entry.clone();
        entry_q.selected_action = ActionType::Quarantine;
        let plan_bad_q = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![entry_q],
        };
        assert!(validate(&plan_bad_q, &ctx).is_err());
    }

    #[test]
    fn test_validate_schema_version() {
        let mut plan = Plan {
            schema_version: "1.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![make_entry(ActionType::Keep)],
        };
        assert!(validate(&plan, &default_ctx()).is_err());

        plan.schema_version = "2.0".to_string();
        assert!(validate(&plan, &default_ctx()).is_ok());
    }

    #[test]
    fn test_lexical_subpath() {
        let base = PathBuf::from("/base");

        assert!(is_subpath(&PathBuf::from("/base/child"), &base));
        assert!(is_subpath(&PathBuf::from("/base/child/grandchild"), &base));
        assert!(is_subpath(&PathBuf::from("/base"), &base)); // Equal is subpath

        // Dot normalization
        assert!(is_subpath(&PathBuf::from("/base/./child"), &base));
        assert!(is_subpath(&PathBuf::from("/base/foo/../child"), &base));

        // Outside
        assert!(!is_subpath(&PathBuf::from("/other"), &base));
        assert!(!is_subpath(&PathBuf::from("/bas"), &base)); // Prefix substring check should fail
        assert!(!is_subpath(&PathBuf::from("/base_sibling"), &base));
    }
}
