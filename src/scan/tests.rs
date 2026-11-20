use crate::config::rules::Rule;
use crate::plan::ActionType;
use crate::scan::walk::recommend;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[cfg(windows)]
#[test]
fn equivalent_windows_drive_prefixes_remain_confined_to_ordinary_components() {
    use crate::scan::walk::scope_contains;
    use std::path::Path;
    let check = |path: &str, scope: &str| scope_contains(Path::new(path), Path::new(scope));
    assert!(check(r"C:\work\project\target.dat", r"\\?\C:\work\project"));
    assert!(check(r"\\?\C:\work\project\target.dat", r"C:\work\project"));
    assert!(!check(
        r"C:\work\project-other\target.dat",
        r"\\?\C:\work\project"
    ));
    assert!(!check(
        r"D:\work\project\target.dat",
        r"\\?\C:\work\project"
    ));
    assert!(!check(
        r"C:\work\project\..\private",
        r"\\?\C:\work\project"
    ));
    assert!(!check(r"C:\work\project.\data", r"\\?\C:\work\project."));
    assert!(!check(r"C:\work\CON\data", r"\\?\C:\work\CON"));
    assert!(!check(r"C:\work\CON .txt", r"\\?\C:\work"));
    assert!(!check(r"C:\work\file:stream", r"\\?\C:\work"));
    assert!(!check(r"work\project\target.dat", r"\\?\C:\work\project"));
    assert!(!check(r"\\server\share\data", r"\\?\UNC\server\share"));
    assert!(!check(r"\\.\PhysicalDrive0\data", r"\\?\C:\"));
    assert!(!check(r"C:work\project", r"\\?\C:\work"));
}

#[cfg(windows)]
#[test]
fn glob_matches_equivalent_drive_paths_without_expanding_unsafe_paths() {
    use crate::scan::walk::glob_matches;
    use std::path::Path;
    let ordinary = globset::Glob::new(r"C:\work\project\*.txt")
        .unwrap()
        .compile_matcher();
    let verbatim = globset::Glob::new(r"\\?\C:\work\project\*.txt")
        .unwrap()
        .compile_matcher();
    assert!(glob_matches(
        Path::new(r"\\?\C:\work\project\file.txt"),
        |path| ordinary.is_match(path)
    ));
    assert!(glob_matches(
        Path::new(r"C:\work\project\file.txt"),
        |path| verbatim.is_match(path)
    ));
    assert!(!glob_matches(
        Path::new(r"\\?\C:\work\project-other\file.txt"),
        |path| ordinary.is_match(path)
    ));
    assert!(!glob_matches(
        Path::new(r"\\?\C:\work\project\file.txt:stream"),
        |path| ordinary.is_match(path)
    ));
    assert!(!glob_matches(
        Path::new(r"\\?\UNC\host\share\file.txt"),
        |path| ordinary.is_match(path)
    ));
}

#[cfg(windows)]
#[test]
fn canonical_scope_applies_recommendation_and_excludes_to_ordinary_scan() -> anyhow::Result<()> {
    use crate::scan::walk::Scanner;
    let temp = tempfile::tempdir()?;
    let ordinary_root = temp.path().to_path_buf();
    let canonical_root = ordinary_root.canonicalize()?;
    std::fs::write(ordinary_root.join("candidate.txt"), b"candidate")?;
    std::fs::create_dir(ordinary_root.join("skip"))?;
    std::fs::write(ordinary_root.join("skip").join("secret.txt"), b"secret")?;
    let mut rule = make_rule(
        "matching scope",
        canonical_root.to_str().unwrap(),
        None,
        None,
    );
    rule.excludes = Some(vec![format!(r"{}\skip*", canonical_root.display())]);
    rule.includes = Some(vec![format!(r"{}\candidate*", canonical_root.display())]);
    let entries = Scanner::new(ordinary_root, vec![rule]).scan()?;
    let candidate = entries
        .iter()
        .find(|entry| entry.path.ends_with("candidate.txt"))
        .unwrap();
    assert_eq!(candidate.recommended_action, ActionType::Quarantine);
    assert_eq!(candidate.selected_action, ActionType::Keep);
    assert!(!entries.iter().any(|entry| entry.path.ends_with("skip")));
    Ok(())
}

// Helper to create a basic rule
fn make_rule(name: &str, scope: &str, older: Option<u64>, size: Option<u64>) -> Rule {
    Rule {
        name: name.to_string(),
        scopes: vec![scope.to_string()],
        excludes: None,
        includes: None,
        older_than_days: older,
        min_size_bytes: size,
        recommended_action: Some(ActionType::Quarantine),
        description: Some("Test Rule".to_string()),
    }
}

#[test]
fn test_recommend_missing_metadata_safety() {
    let path = PathBuf::from("/Users/me/Library/Caches/foo");
    let rules = vec![make_rule("Catch All", "~/Library/Caches", None, None)];

    let (action, reason) = recommend(&path, 100, SystemTime::UNIX_EPOCH, &rules);
    assert_eq!(action, ActionType::Keep);
    assert!(reason.contains("Missing metadata"));
}

#[test]
fn test_recommend_rule_match_age() {
    let path = PathBuf::from("/Users/me/Library/Caches/old_stuff");
    let rules = vec![make_rule(
        "Old Stuff",
        "/Users/me/Library/Caches",
        Some(10),
        None,
    )];

    let old_time = SystemTime::now() - Duration::from_secs(11 * 24 * 60 * 60);
    let (action, reason) = recommend(&path, 100, old_time, &rules);

    // Should match because it IS older than 10 days
    // Wait, logic says: if modified >= threshold { continue; } (Too new)
    // threshold = now - 10 days.
    // old_time = now - 11 days.
    // old_time < threshold. So it does NOT continue. It matches.
    assert_eq!(action, ActionType::Quarantine);
    assert!(reason.contains("Rule: Test Rule"));
}

#[test]
fn test_recommend_rule_skip_too_new() {
    let path = PathBuf::from("/Users/me/Library/Caches/new_stuff");
    let rules = vec![make_rule(
        "Old Stuff",
        "/Users/me/Library/Caches",
        Some(10),
        None,
    )];

    let new_time = SystemTime::now() - Duration::from_secs(5 * 24 * 60 * 60);
    let (action, _reason) = recommend(&path, 100, new_time, &rules);

    // Too new, should fall through to fallback (or Keep if fallback doesn't match)
    // Fallback for caches is 30 days (default policy). So 5 days is Keep.
    assert_eq!(action, ActionType::Keep);
}

#[test]
fn test_recommend_rule_match_size() {
    let path = PathBuf::from("/Users/me/Library/Caches/big_stuff");
    let rules = vec![make_rule(
        "Big Stuff",
        "/Users/me/Library/Caches",
        None,
        Some(1000),
    )];

    let time = SystemTime::now();
    let (action, _) = recommend(&path, 2000, time, &rules);

    assert_eq!(action, ActionType::Quarantine);
}

#[test]
fn test_recommend_fallback_trigger() {
    let path = PathBuf::from("/Users/me/Library/Caches/fallback_candidate");
    let rules = vec![]; // No rules

    // Very old (older than policy fallback ~30 days)
    let very_old = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
    let (action, reason) = recommend(&path, 100, very_old, &rules);

    assert_eq!(action, ActionType::Quarantine);
    assert!(reason.contains("Fallback:"));
}

#[test]
fn slack_and_teams_logs_are_kept_by_legacy_scan_even_when_large_and_old() {
    let rules: crate::config::rules::RuleSet =
        toml::from_str(crate::policy::DEFAULT_RULES_TOML).unwrap();
    let home = directories::BaseDirs::new().unwrap();
    let old = SystemTime::now() - Duration::from_secs(365 * 86_400);
    for name in ["Slack", "Microsoft Teams/logs/previous"] {
        let path = home.home_dir().join("Library/Logs").join(name);
        assert_eq!(
            recommend(&path, 1_000_000_000, old, &rules.rules).0,
            ActionType::Keep
        );
    }
}
