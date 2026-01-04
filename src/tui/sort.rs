use crate::plan::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortCriteria {
    SizeDesc,
    AgeOldestFirst,
    PathAsc,
}

impl Default for SortCriteria {
    fn default() -> Self {
        SortCriteria::SizeDesc
    }
}

impl SortCriteria {
    pub fn next(&self) -> Self {
        match self {
            SortCriteria::SizeDesc => SortCriteria::AgeOldestFirst,
            SortCriteria::AgeOldestFirst => SortCriteria::PathAsc,
            SortCriteria::PathAsc => SortCriteria::SizeDesc,
        }
    }

    #[allow(dead_code)]
    pub fn sort(&self, entries: &mut [Entry]) {
        entries.sort_by(|a, b| self.compare(a, b));
    }

    pub fn compare(&self, a: &Entry, b: &Entry) -> std::cmp::Ordering {
        match self {
            SortCriteria::SizeDesc => {
                // Secondary sort by path for stability
                b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path))
            }
            SortCriteria::AgeOldestFirst => {
                a.modified.cmp(&b.modified).then_with(|| a.path.cmp(&b.path))
            }
            SortCriteria::PathAsc => {
                a.path.cmp(&b.path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, Duration};
    use crate::plan::{EntryType, PlanOrigin, ActionType};

    fn make_entry(path: &str, size: u64, age_secs: u64) -> Entry {
        Entry {
            path: PathBuf::from(path),
            size,
            modified: SystemTime::now() - Duration::from_secs(age_secs),
            entry_type: EntryType::File,
            origin: PlanOrigin::Scan,
            recommended_action: ActionType::Keep,
            reason: "test".to_string(),
            selected_action: ActionType::Keep,
        }
    }

    #[test]
    fn test_sort_size_desc() {
        let mut entries = vec![
            make_entry("small", 10, 0),
            make_entry("big", 100, 0),
            make_entry("med", 50, 0),
        ];
        SortCriteria::SizeDesc.sort(&mut entries);
        assert_eq!(entries[0].size, 100);
        assert_eq!(entries[1].size, 50);
        assert_eq!(entries[2].size, 10);
    }

    #[test]
    fn test_sort_age_oldest_first() {
        // age_secs: bigger number = older
        // modified = now - age_secs. 
        // So bigger age_secs -> smaller modified timestamp
        // OldestFirst means sorting by modified ASC?
        // e.g. T1 (old) < T2 (new). 
        // a.modified.cmp(b.modified) -> [oldest, ..., newest]
        
        let mut entries = vec![
            make_entry("new", 0, 10),     // modified = now - 10
            make_entry("old", 0, 1000),   // modified = now - 1000
            make_entry("mid", 0, 100),    // modified = now - 100
        ];
        
        SortCriteria::AgeOldestFirst.sort(&mut entries);
        
        // Expected: old, mid, new
        assert_eq!(entries[0].path.to_string_lossy(), "old");
        assert_eq!(entries[1].path.to_string_lossy(), "mid");
        assert_eq!(entries[2].path.to_string_lossy(), "new");
    }

    #[test]
    fn test_sort_stability_secondary() {
        let mut entries = vec![
            make_entry("b", 100, 0),
            make_entry("a", 100, 0),
        ];
        // Size match, should fallback to Path Asc
        SortCriteria::SizeDesc.sort(&mut entries);
        assert_eq!(entries[0].path.to_string_lossy(), "a");
        assert_eq!(entries[1].path.to_string_lossy(), "b");
    }
}
