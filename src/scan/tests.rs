#[cfg(test)]
mod tests {
    use crate::scan::walk::recommend;
    use crate::config::rules::Rule;
    use crate::plan::ActionType;
    use std::path::PathBuf;
    use std::time::{SystemTime, Duration};

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
        let rules = vec![make_rule("Old Stuff", "/Users/me/Library/Caches", Some(10), None)];
        
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
        let rules = vec![make_rule("Old Stuff", "/Users/me/Library/Caches", Some(10), None)];
        
        let new_time = SystemTime::now() - Duration::from_secs(5 * 24 * 60 * 60);
        let (action, _reason) = recommend(&path, 100, new_time, &rules);
        
        // Too new, should fall through to fallback (or Keep if fallback doesn't match)
        // Fallback for caches is 30 days (default policy). So 5 days is Keep.
        assert_eq!(action, ActionType::Keep);
    }

    #[test]
    fn test_recommend_rule_match_size() {
        let path = PathBuf::from("/Users/me/Library/Caches/big_stuff");
        let rules = vec![make_rule("Big Stuff", "/Users/me/Library/Caches", None, Some(1000))];
        
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
}
