#![allow(dead_code)]
use std::collections::HashMap;

use crate::layout::Layout;
use crate::pane::Pane;

pub struct Tab {
    pub name: String,
    pub layout: Layout,
    pub panes: HashMap<usize, Pane>,
    pub active_pane: usize,
}

pub struct TabManager {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl TabManager {
    pub fn new(tab: Tab) -> Self {
        Self {
            tabs: vec![tab],
            active: 0,
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn add(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    pub fn close_active(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        let idx = self.active;
        // Kill all panes in closing tab
        for pane in self.tabs[idx].panes.values_mut() {
            pane.kill();
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
    }

    pub fn go_to(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn rename_active(&mut self, name: String) {
        self.tabs[self.active].name = name;
    }

    /// Count all live panes across all tabs.
    pub fn live_pane_count(&self) -> usize {
        self.tabs
            .iter()
            .flat_map(|t| t.panes.values())
            .filter(|p| p.is_alive())
            .count()
    }
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
        }
    }
}
