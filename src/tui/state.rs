use crate::plan::{ActionType, Entry, Plan};
use crate::tui::filter::FilterState;
use crate::tui::sort::SortCriteria;
use ratatui::widgets::TableState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    Filtering,
    BulkConfirm,
}

pub struct AppState {
    pub plan: Plan,
    pub table_state: TableState,
    pub should_quit: bool,

    // Phase 4 additions
    pub mode: AppMode,
    pub filter: FilterState,
    pub sort: SortCriteria,
    // Cached indices of entries that match filter, sorted.
    // This maps View Index -> Plan Index
    view_indices: Vec<usize>,
}

impl AppState {
    pub fn new(plan: Plan) -> Self {
        let mut table_state = TableState::default();
        if !plan.entries.is_empty() {
            table_state.select(Some(0));
        }

        let mut state = Self {
            plan,
            table_state,
            should_quit: false,
            mode: AppMode::Normal,
            filter: FilterState::default(),
            sort: SortCriteria::default(),
            view_indices: Vec::new(),
        };

        // Initialize view
        state.update_view();
        state
    }

    /// Re-calculates view_indices based on current filter and sort state.
    /// Call this whenever filter or sort changes, or bulk edits happen.
    pub fn update_view(&mut self) {
        // 1. Filter
        self.view_indices = self
            .plan
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.filter.matches(entry))
            .map(|(i, _)| i)
            .collect();

        // 2. Sort
        // We can't easily sort indices based on values without cloning entries or generic lifetime hell?
        // Actually, we can just sort indices by lookups into filter state.
        // Wait, SortCriteria takes `&mut [Entry]`.
        // To preserve original indices (so selected_action edits work), we shouldn't reorder `plan.entries` in place,
        // UNLESS we don't care about original order?
        // The Plan is a list of entries. Reordering it is fine for the UI.
        // BUT if we reorder `plan.entries`, we lose the ability to easily map back if we cared about original scan order.
        // For MVP, actually sorting `plan.entries` is simpler than maintaining an index map for sort.
        // However, invalidates `view_indices` logic if we mutate in place.

        // Let's stick to View Mapping to keep Plan stable? No, sorting the Plan is fine.
        // IF we sort the Plan, then view_indices is just 0..len if no filter.

        // Actually, let's sort the VIEW indices only.
        let entries = &self.plan.entries;
        let sort = self.sort;
        self.view_indices.sort_by(|&a, &b| {
            let entry_a = &entries[a];
            let entry_b = &entries[b];
            sort.compare(entry_a, entry_b)
        });

        // Safety: Ensure selection is valid
        if self.view_indices.is_empty() {
            self.table_state.select(None);
        } else if let Some(i) = self.table_state.selected() {
            if i >= self.view_indices.len() {
                self.table_state
                    .select(Some(self.view_indices.len().saturating_sub(1)));
            }
        } else {
            self.table_state.select(Some(0));
        }
    }

    pub fn next(&mut self) {
        if self.view_indices.is_empty() {
            return;
        }

        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.view_indices.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.view_indices.is_empty() {
            return;
        }

        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.view_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    // Get current entry via view index -> plan index
    pub fn current_entry(&self) -> Option<&Entry> {
        self.table_state
            .selected()
            .and_then(|i| self.view_indices.get(i))
            .and_then(|&real_idx| self.plan.entries.get(real_idx))
    }

    pub fn current_entry_mut(&mut self) -> Option<&mut Entry> {
        self.table_state
            .selected()
            .and_then(|i| self.view_indices.get(i))
            .copied() // copy index to avoid borrow
            .and_then(|real_idx| self.plan.entries.get_mut(real_idx))
    }

    pub fn cycle_action(&mut self) {
        if let Some(entry) = self.current_entry_mut() {
            entry.selected_action = match entry.selected_action {
                ActionType::Keep => ActionType::Quarantine,
                ActionType::Quarantine => ActionType::Trash,
                ActionType::Trash => ActionType::Keep,
                ActionType::Purge => ActionType::Keep, // Should not happen
            };
        }
    }

    pub fn set_current_action(&mut self, action: ActionType) {
        if action == ActionType::Purge {
            return;
        } // Safety
        if let Some(entry) = self.current_entry_mut() {
            entry.selected_action = action;
        }
    }

    /// Phase 4: Bulk Apply
    /// Applies the given action to ALL entries currently visible in the filter view.
    /// STRICTLY REJECTS Purge.
    pub fn bulk_apply(&mut self, action: ActionType) {
        if action == ActionType::Purge {
            // Should verify this with unit tests as panic or no-op
            return;
        }

        for &idx in &self.view_indices {
            if let Some(entry) = self.plan.entries.get_mut(idx) {
                entry.selected_action = action;
            }
        }
    }

    // Iterator for rendering (only filtered items)
    pub fn view_entries(&self) -> impl Iterator<Item = &Entry> {
        self.view_indices.iter().map(|&i| &self.plan.entries[i])
    }

    pub fn view_len(&self) -> usize {
        self.view_indices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::ActionType;
    use crate::plan::{Entry, EntryType, Plan, PlanOrigin};
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_entry(path: &str, action: ActionType) -> Entry {
        Entry {
            path: PathBuf::from(path),
            size: 100,
            modified: SystemTime::UNIX_EPOCH,
            entry_type: EntryType::File,
            origin: PlanOrigin::Scan,
            recommended_action: ActionType::Keep,
            reason: "test".to_string(),
            selected_action: action,
        }
    }

    #[test]
    fn test_cycle_action_skip_purge() {
        let entry = make_entry("/tmp/a", ActionType::Keep);
        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![entry],
        };
        let mut state = AppState::new(plan);

        // Keep -> Quarantine
        state.cycle_action();
        assert_eq!(
            state.current_entry().unwrap().selected_action,
            ActionType::Quarantine
        );

        // Quarantine -> Trash
        state.cycle_action();
        assert_eq!(
            state.current_entry().unwrap().selected_action,
            ActionType::Trash
        );

        // Trash -> Keep (SKIP PURGE)
        state.cycle_action();
        assert_eq!(
            state.current_entry().unwrap().selected_action,
            ActionType::Keep
        );
    }

    #[test]
    fn test_bulk_apply_rejects_purge() {
        let entry1 = make_entry("/tmp/1", ActionType::Keep);
        let entry2 = make_entry("/tmp/2", ActionType::Keep);
        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![entry1, entry2],
        };
        let mut state = AppState::new(plan);

        state.bulk_apply(ActionType::Purge);

        // Verify no change
        assert_eq!(state.plan.entries[0].selected_action, ActionType::Keep);
        assert_eq!(state.plan.entries[1].selected_action, ActionType::Keep);
    }

    #[test]
    fn test_filter_view_updates() {
        let mut e1 = make_entry("/tmp/cache", ActionType::Keep);
        e1.reason = "cache".to_string();
        let mut e2 = make_entry("/tmp/log", ActionType::Keep);
        e2.reason = "log".to_string();

        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![e1, e2],
        };
        let mut state = AppState::new(plan);

        assert_eq!(state.view_len(), 2);

        state.filter.text = "cache".to_string();
        state.update_view();

        assert_eq!(state.view_len(), 1);
        assert_eq!(
            state.current_entry().unwrap().path.to_string_lossy(),
            "/tmp/cache"
        );
    }
    #[test]
    fn test_bulk_apply_filtered_only() {
        let mut e1 = make_entry("/tmp/1", ActionType::Keep);
        e1.reason = "include me".to_string();

        let mut e2 = make_entry("/tmp/2", ActionType::Keep);
        e2.reason = "exclude me".to_string();

        let plan = Plan {
            schema_version: "2.0".to_string(),
            generated_by: "test".to_string(),
            created_at: SystemTime::now(),
            entries: vec![e1, e2],
        };
        let mut state = AppState::new(plan);

        // Apply filter
        state.filter.text = "include".to_string();
        state.update_view();
        assert_eq!(state.view_len(), 1);

        // Bulk apply Trash
        state.bulk_apply(ActionType::Trash);

        // Verify e1 (visible) is Trashed
        assert_eq!(state.plan.entries[0].selected_action, ActionType::Trash);
        // Verify e2 (hidden) is Unchanged (Keep)
        assert_eq!(state.plan.entries[1].selected_action, ActionType::Keep);
    }
}
