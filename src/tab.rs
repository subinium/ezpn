use std::collections::HashMap;
use std::time::Instant;

use crate::layout::Layout;
use crate::pane::Pane;
use crate::project;

/// A tab (tmux "window") containing its own layout, panes, and state.
pub struct Tab {
    pub name: String,
    pub layout: Layout,
    pub panes: HashMap<usize, Pane>,
    pub active_pane: usize,
    pub restart_policies: HashMap<usize, project::RestartPolicy>,
    pub restart_state: HashMap<usize, (Instant, u32)>,
    pub zoomed_pane: Option<usize>,
    pub broadcast: bool,
}

impl Tab {
    pub fn new(
        name: String,
        layout: Layout,
        panes: HashMap<usize, Pane>,
        active_pane: usize,
    ) -> Self {
        Self {
            name,
            layout,
            panes,
            active_pane,
            restart_policies: HashMap::new(),
            restart_state: HashMap::new(),
            zoomed_pane: None,
            broadcast: false,
        }
    }
}

/// Manages multiple tabs with save/restore for the event loop.
///
/// Design: the "active" tab's state is unpacked into the caller's local variables.
/// `tabs` stores only *inactive* tabs. When switching, we insert the current tab
/// into `tabs` (making it complete), then remove the target — this avoids the
/// fragile "gap index" math entirely.
pub struct TabManager {
    /// Inactive tabs stored in logical order with the active position as a gap.
    tabs: Vec<Tab>,
    /// Index of the active tab in the logical ordering.
    pub active_idx: usize,
    /// Total tab count (including the unpacked active tab).
    pub count: usize,
    /// Global tab counter for auto-naming.
    next_id: usize,
}

impl TabManager {
    /// Create a new TabManager. The initial tab's state is "unpacked"
    /// (managed by the caller), so `tabs` starts empty.
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active_idx: 0,
            count: 1,
            next_id: 2, // first tab is "1"
        }
    }

    /// Build a TabManager from an ordered list of tabs, with one designated as active.
    /// The active tab is removed from the list and returned for the caller to unpack.
    /// All other tabs go into storage in their original order.
    pub fn from_tabs(mut tabs: Vec<Tab>, active_idx: usize) -> (Self, Tab) {
        assert!(!tabs.is_empty());
        let active_idx = active_idx.min(tabs.len() - 1);
        let active_tab = tabs.remove(active_idx);
        let count = tabs.len() + 1; // +1 for the unpacked active tab
        let next_id = count + 1;
        let mgr = Self {
            tabs,
            active_idx,
            count,
            next_id,
        };
        (mgr, active_tab)
    }

    /// Save the currently active tab's state and switch to a different tab.
    /// Returns the target tab's state to be unpacked by the caller.
    pub fn switch_to(&mut self, target_idx: usize, current: Tab) -> Option<Tab> {
        if target_idx == self.active_idx || target_idx >= self.count {
            return None;
        }

        // Step 1: Insert current tab at its logical position.
        // Storage has (count - 1) elements with the gap at active_idx.
        // Inserting at min(active_idx, tabs.len()) fills the gap.
        let insert_pos = self.active_idx.min(self.tabs.len());
        self.tabs.insert(insert_pos, current);
        // Now `self.tabs` has all `count` tabs in logical order.

        // Step 2: Remove the target tab by its logical index (direct index).
        let target_tab = self.tabs.remove(target_idx);
        // Now `self.tabs` has `count - 1` elements with a gap at target_idx.

        self.active_idx = target_idx;
        Some(target_tab)
    }

    /// Create a new tab. Saves the current tab and returns the new tab's name.
    /// The new tab is placed at the end. The caller sets up the new tab's state.
    pub fn create_tab(&mut self, current: Tab) -> String {
        // Insert current tab at its logical position (fill the gap).
        let insert_pos = self.active_idx.min(self.tabs.len());
        self.tabs.insert(insert_pos, current);
        // Now all existing tabs are in storage.

        let name = format!("{}", self.next_id);
        self.next_id += 1;
        self.count += 1;
        // New tab is at the end, and it's the active one (unpacked by caller).
        self.active_idx = self.count - 1;
        name
    }

    /// Close the active tab. Returns the next tab to activate, or None if last tab.
    /// The caller should kill all panes in the active tab before calling this.
    pub fn close_active(&mut self) -> Option<Tab> {
        if self.count <= 1 {
            return None;
        }

        // Storage has count-1 tabs with a gap at active_idx.
        let old_active = self.active_idx;
        self.count -= 1;

        // Pick adjacent tab: if we were last, go to new last; otherwise stay at same index
        // (which now points to what was the next tab).
        let new_active = if old_active >= self.count {
            self.count - 1 // was last → go to new last
        } else {
            old_active // was not last → tab after gap slides into this position
        };

        // In both cases, new_active <= old_active, so storage_pos = new_active.
        // (Case 1: new_active < old_active; Case 2: new_active == old_active,
        //  which is the storage position of the tab right after the gap.)
        self.active_idx = new_active;
        Some(self.tabs.remove(new_active))
    }

    /// Go to next tab index (wrapping).
    pub fn next_idx(&self) -> usize {
        if self.count <= 1 {
            self.active_idx
        } else {
            (self.active_idx + 1) % self.count
        }
    }

    /// Go to previous tab index (wrapping).
    pub fn prev_idx(&self) -> usize {
        if self.count <= 1 {
            self.active_idx
        } else if self.active_idx == 0 {
            self.count - 1
        } else {
            self.active_idx - 1
        }
    }

    /// Kill all panes in all inactive tabs (for session shutdown). Per SPEC
    /// 03 §4.3, drains `tab.panes` so each `Pane::Drop` fires (joining the
    /// reader thread + releasing the master fd) and clears the per-tab
    /// restart maps + zoomed state. Without the drain the tab retained
    /// ~640 MB × N inactive tabs of vt100 RSS until the whole
    /// `TabManager` itself dropped.
    pub fn kill_all_inactive(&mut self) {
        for tab in &mut self.tabs {
            // Send SIGHUP up-front so the reader thread can observe EOF
            // while we're still iterating; `Pane::Drop` is idempotent and
            // will skip the second kill call.
            for (_, mut pane) in tab.panes.drain() {
                pane.kill();
            }
            tab.restart_policies.clear();
            tab.restart_state.clear();
            tab.zoomed_pane = None;
        }
    }

    /// Get an inactive tab by its logical index.
    /// Returns `None` if the index is the active tab or out of bounds.
    pub fn get_inactive(&self, logical_idx: usize) -> Option<&Tab> {
        if logical_idx == self.active_idx || logical_idx >= self.count {
            return None;
        }
        // Storage has count-1 elements with a gap at active_idx.
        // Logical indices below active_idx map directly; those above map to (idx - 1).
        let storage_idx = if logical_idx < self.active_idx {
            logical_idx
        } else {
            logical_idx - 1
        };
        self.tabs.get(storage_idx)
    }

    /// Get all tab names in order. The active tab is marked with index.
    /// Returns `(index, name, is_active)` for each tab.
    pub fn tab_names(&self, active_tab_name: &str) -> Vec<(usize, String, bool)> {
        let mut result = Vec::with_capacity(self.count);
        let mut storage_iter = self.tabs.iter();

        for i in 0..self.count {
            if i == self.active_idx {
                result.push((i, active_tab_name.to_string(), true));
            } else if let Some(tab) = storage_iter.next() {
                result.push((i, tab.name.clone(), false));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    fn dummy_tab(name: &str) -> Tab {
        Tab::new(name.to_string(), Layout::from_grid(1, 1), HashMap::new(), 0)
    }

    #[test]
    fn new_tab_manager_has_one_tab() {
        let mgr = TabManager::new();
        assert_eq!(mgr.count, 1);
        assert_eq!(mgr.active_idx, 0);
    }

    #[test]
    fn create_tab_increments_count() {
        let mut mgr = TabManager::new();
        let name = mgr.create_tab(dummy_tab("1"));
        assert_eq!(mgr.count, 2);
        assert_eq!(mgr.active_idx, 1); // new tab is active
        assert_eq!(name, "2");
        assert_eq!(mgr.tabs.len(), 1); // old tab in storage
    }

    #[test]
    fn switch_to_from_first_tab() {
        let mut mgr = TabManager::new();
        // Create second tab (active becomes 1)
        mgr.create_tab(dummy_tab("1"));
        assert_eq!(mgr.active_idx, 1);

        // Switch back to tab 0
        let tab = mgr.switch_to(0, dummy_tab("2")).unwrap();
        assert_eq!(tab.name, "1");
        assert_eq!(mgr.active_idx, 0);
    }

    #[test]
    fn switch_to_from_zero() {
        // Start at tab 0, create tab 1, switch to 0, then switch to 1
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active = 1
        let tab_a = mgr.switch_to(0, dummy_tab("B")).unwrap(); // active = 0
        assert_eq!(tab_a.name, "A");
        assert_eq!(mgr.active_idx, 0);

        let tab_b = mgr.switch_to(1, dummy_tab("A-restored")).unwrap();
        assert_eq!(tab_b.name, "B");
        assert_eq!(mgr.active_idx, 1);
    }

    #[test]
    fn switch_three_tabs() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active=1
        mgr.create_tab(dummy_tab("B")); // active=2

        // Switch from 2 to 0
        let tab = mgr.switch_to(0, dummy_tab("C")).unwrap();
        assert_eq!(tab.name, "A");
        assert_eq!(mgr.active_idx, 0);

        // Switch from 0 to 2
        let tab = mgr.switch_to(2, dummy_tab("A")).unwrap();
        assert_eq!(tab.name, "C");
        assert_eq!(mgr.active_idx, 2);

        // Switch from 2 to 1
        let tab = mgr.switch_to(1, dummy_tab("C")).unwrap();
        assert_eq!(tab.name, "B");
        assert_eq!(mgr.active_idx, 1);
    }

    #[test]
    fn close_last_tab_switches_to_prev() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active=1

        // Close tab 1 (active), should load tab 0
        let tab = mgr.close_active().unwrap();
        assert_eq!(tab.name, "A");
        assert_eq!(mgr.active_idx, 0);
        assert_eq!(mgr.count, 1);
    }

    #[test]
    fn close_first_tab_with_two() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active=1
                                        // Switch to tab 0
        mgr.switch_to(0, dummy_tab("B"));
        // Close tab 0
        let tab = mgr.close_active().unwrap();
        // Should load tab that was at logical position 0 after removal
        // which is the old tab 1 (now tab 0)
        assert_eq!(mgr.count, 1);
        assert_eq!(mgr.active_idx, 0);
        assert_eq!(tab.name, "B");
    }

    #[test]
    fn close_single_tab_returns_none() {
        let mut mgr = TabManager::new();
        assert!(mgr.close_active().is_none());
    }

    #[test]
    fn tab_names_correct_order() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active=1
        mgr.create_tab(dummy_tab("B")); // active=2

        let names = mgr.tab_names("C");
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], (0, "A".to_string(), false));
        assert_eq!(names[1], (1, "B".to_string(), false));
        assert_eq!(names[2], (2, "C".to_string(), true));
    }

    #[test]
    fn next_prev_wrap() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A"));
        mgr.create_tab(dummy_tab("B"));
        // active = 2, count = 3
        assert_eq!(mgr.next_idx(), 0); // wraps
        assert_eq!(mgr.prev_idx(), 1);
    }

    #[test]
    fn switch_to_self_returns_none() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A"));
        assert!(mgr.switch_to(mgr.active_idx, dummy_tab("X")).is_none());
    }

    #[test]
    fn switch_to_out_of_bounds_returns_none() {
        let mut mgr = TabManager::new();
        assert!(mgr.switch_to(99, dummy_tab("X")).is_none());
    }

    #[test]
    fn close_middle_tab_with_three() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("A")); // active=1
        mgr.create_tab(dummy_tab("B")); // active=2
                                        // Switch to tab 1 (middle)
        mgr.switch_to(1, dummy_tab("C"));
        assert_eq!(mgr.active_idx, 1);
        // Close middle tab
        let new = mgr.close_active().unwrap();
        assert_eq!(mgr.count, 2);
        // Should get the tab that was at position 1 after removal (old tab 2 = "C")
        assert!(new.name == "C" || new.name == "B");
        assert_eq!(mgr.active_idx, 1.min(mgr.count - 1));
    }

    #[test]
    fn tab_names_active_at_zero() {
        let mut mgr = TabManager::new();
        mgr.create_tab(dummy_tab("B")); // active=1
        mgr.switch_to(0, dummy_tab("B-saved"));
        let names = mgr.tab_names("active-0");
        assert_eq!(names[0], (0, "active-0".to_string(), true));
        assert_eq!(names[1], (1, "B-saved".to_string(), false));
    }

    #[test]
    fn from_tabs_preserves_order() {
        let tabs = vec![dummy_tab("A"), dummy_tab("B"), dummy_tab("C")];
        let (mgr, active) = TabManager::from_tabs(tabs, 1); // B is active
        assert_eq!(active.name, "B");
        assert_eq!(mgr.count, 3);
        assert_eq!(mgr.active_idx, 1);

        // Verify tab order: A at 0, B (unpacked), C at 2
        let names = mgr.tab_names("B");
        assert_eq!(names[0], (0, "A".to_string(), false));
        assert_eq!(names[1], (1, "B".to_string(), true));
        assert_eq!(names[2], (2, "C".to_string(), false));
    }

    #[test]
    fn from_tabs_active_at_zero() {
        let tabs = vec![dummy_tab("X"), dummy_tab("Y")];
        let (mgr, active) = TabManager::from_tabs(tabs, 0);
        assert_eq!(active.name, "X");
        assert_eq!(mgr.active_idx, 0);
        let names = mgr.tab_names("X");
        assert_eq!(names[0], (0, "X".to_string(), true));
        assert_eq!(names[1], (1, "Y".to_string(), false));
    }

    #[test]
    fn from_tabs_active_at_last() {
        let tabs = vec![dummy_tab("A"), dummy_tab("B"), dummy_tab("C")];
        let (mgr, active) = TabManager::from_tabs(tabs, 2);
        assert_eq!(active.name, "C");
        assert_eq!(mgr.active_idx, 2);
        let names = mgr.tab_names("C");
        assert_eq!(names[0], (0, "A".to_string(), false));
        assert_eq!(names[1], (1, "B".to_string(), false));
        assert_eq!(names[2], (2, "C".to_string(), true));
    }

    #[test]
    fn from_tabs_single() {
        let tabs = vec![dummy_tab("only")];
        let (mgr, active) = TabManager::from_tabs(tabs, 0);
        assert_eq!(active.name, "only");
        assert_eq!(mgr.count, 1);
        assert_eq!(mgr.active_idx, 0);
    }

    /// SPEC 03 §4.3: `kill_all_inactive` drains `tab.panes` (releasing fds
    /// and joining reader threads via `Pane::Drop`) and clears the per-tab
    /// restart bookkeeping. We exercise the bookkeeping side without
    /// spawning real PTYs by injecting a tab and seeding its restart maps.
    #[test]
    fn kill_all_inactive_clears_bookkeeping() {
        let mut mgr = TabManager::new();
        let mut tab = dummy_tab("syn");
        tab.restart_policies
            .insert(42, project::RestartPolicy::Always);
        tab.restart_state.insert(42, (Instant::now(), 3));
        tab.zoomed_pane = Some(42);
        mgr.tabs.push(tab);

        mgr.kill_all_inactive();

        let cleaned = &mgr.tabs[0];
        assert!(
            cleaned.panes.is_empty(),
            "kill_all_inactive must drain tab.panes"
        );
        assert!(
            cleaned.restart_policies.is_empty(),
            "restart_policies must be cleared"
        );
        assert!(
            cleaned.restart_state.is_empty(),
            "restart_state must be cleared"
        );
        assert!(cleaned.zoomed_pane.is_none(), "zoomed_pane must be reset");
    }
}
