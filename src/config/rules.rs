use crate::plan::ActionType;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Rule {
    pub name: String,
    pub scopes: Vec<String>,
    pub excludes: Option<Vec<String>>,
    pub includes: Option<Vec<String>>,
    pub older_than_days: Option<u64>,
    pub min_size_bytes: Option<u64>,
    pub recommended_action: Option<ActionType>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
}

pub fn load<P: AsRef<Path>>(path: P) -> Result<Vec<Rule>> {
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read rules file: {:?}", path.as_ref()))?;

    let rule_set: RuleSet =
        toml::from_str(&content).with_context(|| "Failed to parse rules TOML")?;

    validate_rules(&rule_set.rules)?;

    Ok(rule_set.rules)
}

fn validate_rules(rules: &[Rule]) -> Result<()> {
    for rule in rules {
        // Validate scopes
        if rule.scopes.is_empty() {
            return Err(anyhow!("Rule '{}' has no scopes defined", rule.name));
        }
        for scope in &rule.scopes {
            validate_scope(scope).with_context(|| format!("In rule '{}'", rule.name))?;
        }

        // Validate patterns
        if let Some(excludes) = &rule.excludes {
            for pattern in excludes {
                if pattern.trim().is_empty() {
                    return Err(anyhow!("Rule '{}' has empty exclude pattern", rule.name));
                }
            }
        }
        if let Some(includes) = &rule.includes {
            for pattern in includes {
                if pattern.trim().is_empty() {
                    return Err(anyhow!("Rule '{}' has empty include pattern", rule.name));
                }
            }
        }
    }
    Ok(())
}

fn validate_scope(scope_str: &str) -> Result<()> {
    let scope_path = expand_path(scope_str)?;

    // Check for root
    if scope_path.parent().is_none() {
        return Err(anyhow!("Scope '{}' is root, which is unsafe", scope_str));
    }

    // Check for home directory
    if let Some(user_dirs) = directories::UserDirs::new() {
        let home = user_dirs.home_dir();
        if scope_path == home {
            return Err(anyhow!(
                "Scope '{}' is the home directory, which is too broad",
                scope_str
            ));
        }
        // Also check if it's a parent of home (unlikely but possible)
        if home.starts_with(&scope_path) && home != scope_path {
            return Err(anyhow!(
                "Scope '{}' contains home directory, which is too broad",
                scope_str
            ));
        }
    }

    Ok(())
}

pub fn expand_path(path_str: &str) -> Result<PathBuf> {
    if path_str.starts_with("~") {
        if let Some(user_dirs) = directories::UserDirs::new() {
            let home = user_dirs.home_dir();
            let without_tilde = path_str.strip_prefix("~").unwrap_or(path_str);
            // Handle ~/.foo vs ~/foo
            let suffix = without_tilde.trim_start_matches(std::path::is_separator);
            return Ok(home.join(suffix));
        }
    }
    Ok(PathBuf::from(path_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_expand_path() {
        let dirs = directories::UserDirs::new().unwrap();
        let expected = dirs.home_dir().join("Library");
        assert_eq!(expand_path("~/Library").unwrap(), expected);
        assert_eq!(expand_path("~").unwrap(), dirs.home_dir());
        #[cfg(windows)]
        assert_eq!(expand_path(r"~\Library").unwrap(), expected);
    }

    #[test]
    fn test_validate_scope_unsafe() {
        assert!(validate_scope("/").is_err());
        assert!(validate_scope("~").is_err());
    }

    #[test]
    fn test_validate_scope_safe() {
        assert!(validate_scope("~/Library/Caches").is_ok());
        assert!(validate_scope("/tmp/foo").is_ok());
    }

    #[test]
    fn test_load_rules_valid() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("rules.toml");
        let content = r#"
            [[rules]]
            name = "Cache Cleanup"
            scopes = ["~/Library/Caches"]
            excludes = ["*.keep"]
            recommended_action = "Trash"
        "#;
        let mut file = File::create(&file_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let rules = load(&file_path).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "Cache Cleanup");
        assert_eq!(rules[0].recommended_action, Some(ActionType::Trash));
    }

    #[test]
    fn test_load_rules_invalid_scope() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("bad_rules.toml");
        let content = r#"
            [[rules]]
            name = "Root Cleanup"
            scopes = ["/"]
        "#;
        let mut file = File::create(&file_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        assert!(load(&file_path).is_err());
    }

    #[test]
    fn test_embedded_rules_parsing() {
        let content = crate::policy::DEFAULT_RULES_TOML;
        let rule_set: RuleSet =
            toml::from_str(content).expect("Default rules TOML should be valid");
        assert!(
            !rule_set.rules.is_empty(),
            "Default rules should not be empty"
        );
        for rule in rule_set.rules {
            assert!(!rule.scopes.is_empty());
            assert!(!rule.name.is_empty());
            // Ensure at least one threshold is set if recommended_action is set
            if rule.recommended_action.is_some() {
                assert!(
                    rule.older_than_days.is_some()
                        || rule.min_size_bytes.is_some()
                        || rule.includes.is_some()
                );
            }
        }
    }
}
