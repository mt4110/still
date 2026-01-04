use crate::plan::Entry;

#[derive(Debug, Clone, Default)]
pub struct FilterState {
    pub text: String,
    // Add these later if needed for advanced filter UI
    // pub action: Option<ActionType>,
    // pub entry_type: Option<EntryType>,
}

impl FilterState {
    pub fn matches(&self, entry: &Entry) -> bool {
        if self.text.is_empty() {
            return true;
        }

        let query = self.text.to_lowercase();
        
        // Match Path (using prettified version for WYSIWYG feeling, or raw path)
        // Let's match raw path components for broader coverage
        if entry.path.to_string_lossy().to_lowercase().contains(&query) {
            return true;
        }

        // Match Reason
        if entry.reason.to_lowercase().contains(&query) {
            return true;
        }

        false
    }

    pub fn is_active(&self) -> bool {
        !self.text.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;
    use crate::plan::PlanOrigin;
    use crate::plan::{EntryType, ActionType};

    fn make_entry(path: &str, reason: &str) -> Entry {
        Entry {
            path: PathBuf::from(path),
            size: 100,
            modified: SystemTime::UNIX_EPOCH,
            entry_type: EntryType::File,
            origin: PlanOrigin::Scan,
            recommended_action: ActionType::Keep,
            reason: reason.to_string(),
            selected_action: ActionType::Keep,
        }
    }

    #[test]
    fn test_filter_matches_path() {
        let filter = FilterState { text: "cache".to_string() };
        let entry = make_entry("/Library/Caches/foo", "rule match");
        assert!(filter.matches(&entry));
    }

    #[test]
    fn test_filter_matches_reason() {
        let filter = FilterState { text: "rule".to_string() };
        let entry = make_entry("/Library/Logs/foo", "rule match");
        assert!(filter.matches(&entry));
    }

    #[test]
    fn test_filter_case_insensitive() {
        let filter = FilterState { text: "CACHE".to_string() };
        let entry = make_entry("/Library/Caches/foo", "rule match");
        assert!(filter.matches(&entry));
    }

    #[test]
    fn test_filter_no_match() {
        let filter = FilterState { text: "xyz".to_string() };
        let entry = make_entry("/Library/Caches/foo", "rule match");
        assert!(!filter.matches(&entry));
    }
}
