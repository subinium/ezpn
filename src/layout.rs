use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Horizontal, // panes arranged left | right
    Vertical,   // panes arranged top / bottom
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LayoutNode {
    Leaf {
        id: usize,
    },
    Split {
        direction: Direction,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl Default for LayoutNode {
    fn default() -> Self {
        LayoutNode::Leaf { id: 0 }
    }
}

#[derive(Clone, Debug)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Clone, Copy)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

pub struct SepLine {
    pub horizontal: bool,
    pub x: u16,
    pub y: u16,
    pub length: u16,
}

/// Result of hitting a separator line (for drag-to-resize).
pub struct SepHit {
    pub path: Vec<bool>,      // tree path: false=first, true=second
    pub direction: Direction, // split direction (H or V)
    pub area: Rect,           // content area of the Split node
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    pub root: LayoutNode,
    pub next_id: usize,
}

impl Layout {
    pub fn from_grid(rows: usize, cols: usize) -> Self {
        let mut next_id = 0;
        let root = build_grid(rows, cols, &mut next_id);
        Layout { root, next_id }
    }

    /// Parse a layout spec: "7:3" or "1:1:1" or "7:3/5:5" or "1/1:1"
    /// `/` separates rows, `:` separates columns within a row.
    /// Numbers are relative weights.
    ///
    /// Also supports named presets:
    ///   "ide"     → 7:3/1:1  (editor + sidebar / two bottom panes)
    ///   "dev"     → 7:3      (main + side)
    ///   "monitor" → 1:1:1    (3 equal columns)
    ///   "quad"    → 2x2 grid
    ///   "stack"   → 1/1/1    (3 vertical rows)
    ///   "main"    → 6:4/1    (wide top + full bottom)
    pub fn from_spec(spec: &str) -> Result<Self, String> {
        // Named presets
        let resolved = match spec.trim() {
            "ide" => "7:3/1:1",
            "dev" => "7:3",
            "monitor" => "1:1:1",
            "quad" => return Ok(Self::from_grid(2, 2)),
            "stack" => "1/1/1",
            "main" => "6:4/1",
            "trio" => "1/1:1",
            other => other,
        };
        let rows: Vec<&str> = resolved.split('/').collect();
        if rows.is_empty() {
            return Err("empty layout spec".into());
        }

        let mut next_id = 0;
        let row_nodes: Vec<(LayoutNode, usize)> = rows
            .iter()
            .map(|row| {
                let cols: Vec<u32> = row
                    .split(':')
                    .map(|s| {
                        let weight = s
                            .trim()
                            .parse::<u32>()
                            .map_err(|_| format!("invalid weight: '{}'", s))?;
                        if (1..=9).contains(&weight) {
                            Ok(weight)
                        } else {
                            Err(format!("weight must be 1-9: '{}'", s))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if cols.is_empty() {
                    return Err("empty row in layout spec".into());
                }
                let total: u32 = cols.iter().sum();
                if total == 0 {
                    return Err("row weights sum to 0".into());
                }
                let node = build_weighted_row(&cols, total, &mut next_id);
                Ok((node, cols.len()))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let total_panes: usize = row_nodes.iter().map(|(_, c)| *c).sum();
        if total_panes > 100 {
            return Err(format!("too many panes: {}", total_panes));
        }

        let row_count = row_nodes.len();
        let nodes: Vec<LayoutNode> = row_nodes.into_iter().map(|(n, _)| n).collect();
        let root = build_weighted_column(&nodes, row_count);
        Ok(Layout { root, next_id })
    }

    pub fn pane_ids(&self) -> Vec<usize> {
        let mut ids = Vec::new();
        collect_ids(&self.root, &mut ids);
        ids
    }

    pub fn pane_count(&self) -> usize {
        count_leaves(&self.root)
    }

    /// Content rects for all panes (area inside borders).
    pub fn pane_rects(&self, inner: &Rect) -> HashMap<usize, Rect> {
        let mut rects = HashMap::new();
        collect_rects(&self.root, inner, &mut rects);
        rects
    }

    /// Separator lines (for border rendering).
    pub fn separators(&self, inner: &Rect, outer: &Rect) -> Vec<SepLine> {
        let mut seps = Vec::new();
        collect_seps(&self.root, inner, outer, &mut seps);
        seps
    }

    /// Find pane at a screen position (checks content rects).
    pub fn find_at(&self, x: u16, y: u16, inner: &Rect) -> Option<usize> {
        find_at_node(&self.root, x, y, inner)
    }

    /// Split a pane. Returns new pane ID. Auto-equalizes ratios.
    pub fn split(&mut self, target_id: usize, dir: Direction) -> usize {
        let new_id = self.next_id;
        self.next_id += 1;
        let old = std::mem::take(&mut self.root);
        self.root = split_node(old, target_id, new_id, dir);
        self.equalize(); // auto-equalize after split
        new_id
    }

    /// Equalize all pane sizes: set each split ratio so every leaf gets equal space.
    pub fn equalize(&mut self) {
        equalize_node(&mut self.root);
    }

    /// Remove a pane (collapses parent split). Returns false if it's the last pane.
    pub fn remove(&mut self, target_id: usize) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let old = std::mem::take(&mut self.root);
        if let Some(new_root) = remove_node(old, target_id) {
            self.root = new_root;
            true
        } else {
            false
        }
    }

    /// Find which separator is at screen position (for drag-to-resize).
    pub fn find_separator_at(&self, x: u16, y: u16, inner: &Rect) -> Option<SepHit> {
        let mut path = Vec::new();
        find_sep_at(&self.root, x, y, inner, &mut path)
    }

    /// Update the ratio of a Split node identified by tree path.
    pub fn set_ratio_at_path(&mut self, path: &[bool], ratio: f32) {
        set_ratio_at(&mut self.root, path, ratio);
    }

    /// Navigate to the nearest pane in a direction.
    pub fn navigate(&self, from_id: usize, nav: NavDir, inner: &Rect) -> Option<usize> {
        let rects = self.pane_rects(inner);
        let from = rects.get(&from_id)?;
        rects
            .iter()
            .filter(|(&id, r)| id != from_id && is_adjacent(from, r, nav))
            .min_by_key(|(_, r)| nav_distance(from, r, nav))
            .map(|(&id, _)| id)
    }

    /// Next pane ID in tree order (for cycling).
    pub fn next_pane(&self, current: usize) -> usize {
        let ids = self.pane_ids();
        if ids.is_empty() {
            return current;
        }
        let pos = ids.iter().position(|&id| id == current).unwrap_or(0);
        ids[(pos + 1) % ids.len()]
    }

    /// Previous pane ID in tree order (for cycling).
    pub fn prev_pane(&self, current: usize) -> usize {
        let ids = self.pane_ids();
        if ids.is_empty() {
            return current;
        }
        let pos = ids.iter().position(|&id| id == current).unwrap_or(0);
        if pos == 0 {
            ids[ids.len() - 1]
        } else {
            ids[pos - 1]
        }
    }

    /// Swap two pane IDs in the tree (swap their positions).
    pub fn swap_panes(&mut self, a: usize, b: usize) {
        swap_leaf_ids(&mut self.root, a, b);
    }

    /// Resize a pane by moving the nearest matching separator.
    /// Returns true if a resize was applied.
    pub fn resize_pane(&mut self, pane_id: usize, dir: NavDir, delta: f32) -> bool {
        let mut breadcrumbs = Vec::new();
        collect_path_to_pane(&self.root, pane_id, &mut Vec::new(), &mut breadcrumbs);

        let target_dir = match dir {
            NavDir::Left | NavDir::Right => Direction::Horizontal,
            NavDir::Up | NavDir::Down => Direction::Vertical,
        };
        let need_in_second = matches!(dir, NavDir::Left | NavDir::Up);

        // Search from deepest to shallowest for the right split to adjust
        for (path, split_dir, in_second) in breadcrumbs.iter().rev() {
            if *split_dir == target_dir && *in_second == need_in_second {
                let sign = if need_in_second { -1.0 } else { 1.0 };
                adjust_ratio_at(&mut self.root, path, delta * sign);
                return true;
            }
        }
        false
    }
}

// ─── Tree Construction ─────────────────────────────────────

/// Build a row from weighted columns: [7, 3] → 70:30 split
fn build_weighted_row(weights: &[u32], total: u32, next_id: &mut usize) -> LayoutNode {
    if weights.len() == 1 {
        let id = *next_id;
        *next_id += 1;
        return LayoutNode::Leaf { id };
    }
    let first_w = weights[0];
    let rest_total = total - first_w;
    let ratio = first_w as f32 / total as f32;
    let id = *next_id;
    *next_id += 1;
    LayoutNode::Split {
        direction: Direction::Horizontal,
        ratio,
        first: Box::new(LayoutNode::Leaf { id }),
        second: Box::new(build_weighted_row(&weights[1..], rest_total, next_id)),
    }
}

/// Stack rows vertically with equal weight
fn build_weighted_column(rows: &[LayoutNode], count: usize) -> LayoutNode {
    if rows.len() == 1 {
        return rows[0].clone();
    }
    let ratio = 1.0 / count as f32;
    LayoutNode::Split {
        direction: Direction::Vertical,
        ratio,
        first: Box::new(rows[0].clone()),
        second: Box::new(build_weighted_column(&rows[1..], count - 1)),
    }
}

fn build_grid(rows: usize, cols: usize, next_id: &mut usize) -> LayoutNode {
    if rows == 1 {
        return build_row(cols, next_id);
    }
    LayoutNode::Split {
        direction: Direction::Vertical,
        ratio: 1.0 / rows as f32,
        first: Box::new(build_row(cols, next_id)),
        second: Box::new(build_grid(rows - 1, cols, next_id)),
    }
}

fn build_row(cols: usize, next_id: &mut usize) -> LayoutNode {
    if cols == 1 {
        let id = *next_id;
        *next_id += 1;
        return LayoutNode::Leaf { id };
    }
    let id = *next_id;
    *next_id += 1;
    LayoutNode::Split {
        direction: Direction::Horizontal,
        ratio: 1.0 / cols as f32,
        first: Box::new(LayoutNode::Leaf { id }),
        second: Box::new(build_row(cols - 1, next_id)),
    }
}

// ─── Equalize ──────────────────────────────────────────────

fn equalize_node(node: &mut LayoutNode) {
    if let LayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        let left = count_leaves(first);
        let total = left + count_leaves(second);
        *ratio = left as f32 / total as f32;
        equalize_node(first);
        equalize_node(second);
    }
}

// ─── Rect Calculation ──────────────────────────────────────

fn collect_ids(node: &LayoutNode, ids: &mut Vec<usize>) {
    match node {
        LayoutNode::Leaf { id } => ids.push(*id),
        LayoutNode::Split { first, second, .. } => {
            collect_ids(first, ids);
            collect_ids(second, ids);
        }
    }
}

fn count_leaves(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Leaf { .. } => 1,
        LayoutNode::Split { first, second, .. } => count_leaves(first) + count_leaves(second),
    }
}

fn collect_rects(node: &LayoutNode, area: &Rect, out: &mut HashMap<usize, Rect>) {
    match node {
        LayoutNode::Leaf { id } => {
            out.insert(*id, area.clone());
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a1, a2) = split_area(area, *direction, *ratio);
            collect_rects(first, &a1, out);
            collect_rects(second, &a2, out);
        }
    }
}

/// Smallest cell width any leaf pane is allowed to occupy. A pane below
/// this becomes useless (the border + a single content column squeezes
/// shells into infinite reflow) and several render paths assume at least
/// this much space exists. Issue #17.
pub const MIN_PANE_W: u16 = 3;

/// Smallest cell height any leaf pane is allowed to occupy. Same rationale
/// as [`MIN_PANE_W`].
pub const MIN_PANE_H: u16 = 2;

/// Returns true when `area` has enough room to hold a separator plus two
/// MIN-sized children along `dir`. Callers can pre-check before invoking
/// [`Layout::split`] and surface a "pane too small" message instead of
/// silently producing a degenerate split.
#[allow(dead_code)] // wired up by command-palette / split keybindings in a follow-up
pub fn can_split(area: &Rect, dir: Direction) -> bool {
    // `> 2*MIN + 0` instead of `>= 2*MIN + 1` so clippy's int_plus_one
    // lint stays quiet without sacrificing the check (`a >= b+1` ≡ `a > b`).
    match dir {
        Direction::Horizontal => area.w > 2 * MIN_PANE_W,
        Direction::Vertical => area.h > 2 * MIN_PANE_H,
    }
}

pub fn split_area(area: &Rect, dir: Direction, ratio: f32) -> (Rect, Rect) {
    match dir {
        Direction::Horizontal => {
            let usable = area.w.saturating_sub(1); // 1 cell for separator
            if usable < 2 {
                // Not enough room for two children. Hand the caller back the
                // whole area as the first pane and an explicit zero-width
                // second so render code can detect / skip it. This branch is
                // only reachable from snapshot restore on tiny terminals;
                // interactive splits are guarded by `can_split`.
                return (area.clone(), Rect { w: 0, ..*area });
            }
            // Clamp so both children stay at or above MIN_PANE_W when the
            // area can afford it. For the corner case `usable < 2*MIN_PANE_W`
            // (e.g. 5-col terminal), `clamp(min, max)` requires `min <= max`
            // — so we use the pre-existing `[1, usable-1]` floor in that
            // narrow window. Both children may be below MIN there, but
            // nothing crashes; a follow-up will surface a UI warning.
            let lo = MIN_PANE_W.min(usable.saturating_sub(MIN_PANE_W));
            let hi = (usable.saturating_sub(MIN_PANE_W)).max(lo);
            let fw = ((usable as f32 * ratio).round() as u16).clamp(lo.max(1), hi.max(1));
            let sw = usable - fw;
            (
                Rect { w: fw, ..*area },
                Rect {
                    x: area.x + fw + 1,
                    w: sw,
                    ..*area
                },
            )
        }
        Direction::Vertical => {
            let usable = area.h.saturating_sub(1);
            if usable < 2 {
                return (area.clone(), Rect { h: 0, ..*area });
            }
            let lo = MIN_PANE_H.min(usable.saturating_sub(MIN_PANE_H));
            let hi = (usable.saturating_sub(MIN_PANE_H)).max(lo);
            let fh = ((usable as f32 * ratio).round() as u16).clamp(lo.max(1), hi.max(1));
            let sh = usable - fh;
            (
                Rect { h: fh, ..*area },
                Rect {
                    y: area.y + fh + 1,
                    h: sh,
                    ..*area
                },
            )
        }
    }
}

// ─── Separators ────────────────────────────────────────────

fn collect_seps(node: &LayoutNode, area: &Rect, outer: &Rect, seps: &mut Vec<SepLine>) {
    if let LayoutNode::Split {
        direction,
        ratio,
        first,
        second,
    } = node
    {
        let (a1, a2) = split_area(area, *direction, *ratio);

        match direction {
            Direction::Horizontal => {
                let sx = a1.x + a1.w;
                let y0 = area.y.saturating_sub(1).max(outer.y);
                let y1 = (area.y + area.h).min(outer.y + outer.h - 1);
                seps.push(SepLine {
                    horizontal: false,
                    x: sx,
                    y: y0,
                    length: y1.saturating_sub(y0) + 1,
                });
            }
            Direction::Vertical => {
                let sy = a1.y + a1.h;
                let x0 = area.x.saturating_sub(1).max(outer.x);
                let x1 = (area.x + area.w).min(outer.x + outer.w - 1);
                seps.push(SepLine {
                    horizontal: true,
                    x: x0,
                    y: sy,
                    length: x1.saturating_sub(x0) + 1,
                });
            }
        }

        collect_seps(first, &a1, outer, seps);
        collect_seps(second, &a2, outer, seps);
    }
}

// ─── Hit Testing ───────────────────────────────────────────

fn find_at_node(node: &LayoutNode, x: u16, y: u16, area: &Rect) -> Option<usize> {
    match node {
        LayoutNode::Leaf { id } => {
            if x >= area.x && x < area.x + area.w && y >= area.y && y < area.y + area.h {
                Some(*id)
            } else {
                None
            }
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a1, a2) = split_area(area, *direction, *ratio);
            find_at_node(first, x, y, &a1).or_else(|| find_at_node(second, x, y, &a2))
        }
    }
}

// ─── Separator Hit Detection (Drag-to-Resize) ────────────

fn find_sep_at(
    node: &LayoutNode,
    x: u16,
    y: u16,
    area: &Rect,
    path: &mut Vec<bool>,
) -> Option<SepHit> {
    if let LayoutNode::Split {
        direction,
        ratio,
        first,
        second,
    } = node
    {
        let (a1, a2) = split_area(area, *direction, *ratio);

        // Check if (x, y) is on this split's separator (±1 cell tolerance for easier grab)
        let on_sep = match direction {
            Direction::Horizontal => {
                let sep_x = a1.x + a1.w;
                x >= sep_x.saturating_sub(1)
                    && x <= sep_x.saturating_add(1)
                    && y >= area.y
                    && y < area.y + area.h
            }
            Direction::Vertical => {
                let sep_y = a1.y + a1.h;
                y >= sep_y.saturating_sub(1)
                    && y <= sep_y.saturating_add(1)
                    && x >= area.x
                    && x < area.x + area.w
            }
        };

        if on_sep {
            return Some(SepHit {
                path: path.clone(),
                direction: *direction,
                area: area.clone(),
            });
        }

        // Recurse into children
        path.push(false);
        if let Some(hit) = find_sep_at(first, x, y, &a1, path) {
            return Some(hit);
        }
        path.pop();

        path.push(true);
        if let Some(hit) = find_sep_at(second, x, y, &a2, path) {
            return Some(hit);
        }
        path.pop();
    }
    None
}

fn set_ratio_at(node: &mut LayoutNode, path: &[bool], ratio: f32) {
    if path.is_empty() {
        if let LayoutNode::Split { ratio: r, .. } = node {
            *r = ratio.clamp(0.1, 0.9);
        }
    } else if let LayoutNode::Split { first, second, .. } = node {
        if !path[0] {
            set_ratio_at(first, &path[1..], ratio);
        } else {
            set_ratio_at(second, &path[1..], ratio);
        }
    }
}

// ─── Tree Modification ────────────────────────────────────

fn split_node(node: LayoutNode, target: usize, new_id: usize, dir: Direction) -> LayoutNode {
    match node {
        LayoutNode::Leaf { id } if id == target => LayoutNode::Split {
            direction: dir,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf { id }),
            second: Box::new(LayoutNode::Leaf { id: new_id }),
        },
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutNode::Split {
            direction,
            ratio,
            first: Box::new(split_node(*first, target, new_id, dir)),
            second: Box::new(split_node(*second, target, new_id, dir)),
        },
        other => other,
    }
}

fn remove_node(node: LayoutNode, target: usize) -> Option<LayoutNode> {
    match node {
        LayoutNode::Leaf { id } if id == target => None,
        LayoutNode::Split {
            first,
            second,
            direction,
            ratio,
        } => {
            if contains(&first, target) {
                match remove_node(*first, target) {
                    Some(f) => Some(LayoutNode::Split {
                        direction,
                        ratio,
                        first: Box::new(f),
                        second,
                    }),
                    None => Some(*second),
                }
            } else if contains(&second, target) {
                match remove_node(*second, target) {
                    Some(s) => Some(LayoutNode::Split {
                        direction,
                        ratio,
                        first,
                        second: Box::new(s),
                    }),
                    None => Some(*first),
                }
            } else {
                Some(LayoutNode::Split {
                    direction,
                    ratio,
                    first,
                    second,
                })
            }
        }
        other => Some(other),
    }
}

fn contains(node: &LayoutNode, target: usize) -> bool {
    match node {
        LayoutNode::Leaf { id } => *id == target,
        LayoutNode::Split { first, second, .. } => {
            contains(first, target) || contains(second, target)
        }
    }
}

// ─── Swap ──────────────────────────────────────────────────

fn swap_leaf_ids(node: &mut LayoutNode, a: usize, b: usize) {
    match node {
        LayoutNode::Leaf { id } => {
            if *id == a {
                *id = b;
            } else if *id == b {
                *id = a;
            }
        }
        LayoutNode::Split { first, second, .. } => {
            swap_leaf_ids(first, a, b);
            swap_leaf_ids(second, a, b);
        }
    }
}

// ─── Keyboard Resize ──────────────────────────────────────

fn adjust_ratio_at(node: &mut LayoutNode, path: &[bool], delta: f32) {
    if path.is_empty() {
        if let LayoutNode::Split { ratio, .. } = node {
            *ratio = (*ratio + delta).clamp(0.1, 0.9);
        }
    } else if let LayoutNode::Split { first, second, .. } = node {
        if !path[0] {
            adjust_ratio_at(first, &path[1..], delta);
        } else {
            adjust_ratio_at(second, &path[1..], delta);
        }
    }
}

/// Collect breadcrumbs from root to pane: (path_to_split, direction, pane_is_in_second).
fn collect_path_to_pane(
    node: &LayoutNode,
    target: usize,
    current_path: &mut Vec<bool>,
    breadcrumbs: &mut Vec<(Vec<bool>, Direction, bool)>,
) -> bool {
    match node {
        LayoutNode::Leaf { id } => *id == target,
        LayoutNode::Split {
            direction,
            first,
            second,
            ..
        } => {
            let path_here = current_path.clone();

            current_path.push(false);
            if collect_path_to_pane(first, target, current_path, breadcrumbs) {
                current_path.pop();
                breadcrumbs.push((path_here, *direction, false));
                return true;
            }
            current_path.pop();

            current_path.push(true);
            if collect_path_to_pane(second, target, current_path, breadcrumbs) {
                current_path.pop();
                breadcrumbs.push((path_here, *direction, true));
                return true;
            }
            current_path.pop();

            false
        }
    }
}

// ─── Navigation ────────────────────────────────────────────

fn is_adjacent(from: &Rect, to: &Rect, dir: NavDir) -> bool {
    match dir {
        NavDir::Left => to.x + to.w <= from.x && v_overlap(from, to),
        NavDir::Right => to.x >= from.x + from.w && v_overlap(from, to),
        NavDir::Up => to.y + to.h <= from.y && h_overlap(from, to),
        NavDir::Down => to.y >= from.y + from.h && h_overlap(from, to),
    }
}

fn v_overlap(a: &Rect, b: &Rect) -> bool {
    a.y < b.y + b.h && b.y < a.y + a.h
}

fn h_overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w
}

fn nav_distance(from: &Rect, to: &Rect, dir: NavDir) -> u16 {
    match dir {
        NavDir::Left => from.x.saturating_sub(to.x + to.w),
        NavDir::Right => to.x.saturating_sub(from.x + from.w),
        NavDir::Up => from.y.saturating_sub(to.y + to.h),
        NavDir::Down => to.y.saturating_sub(from.y + from.h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner_80x24() -> Rect {
        Rect {
            x: 1,
            y: 1,
            w: 78,
            h: 22,
        }
    }

    #[test]
    fn grid_1x1() {
        let layout = Layout::from_grid(1, 1);
        assert_eq!(layout.pane_count(), 1);
        assert_eq!(layout.pane_ids(), vec![0]);
    }

    #[test]
    fn grid_2x3() {
        let layout = Layout::from_grid(2, 3);
        assert_eq!(layout.pane_count(), 6);
        assert_eq!(layout.pane_ids(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn pane_rects_cover_inner() {
        let layout = Layout::from_grid(2, 2);
        let inner = inner_80x24();
        let rects = layout.pane_rects(&inner);
        assert_eq!(rects.len(), 4);
        for rect in rects.values() {
            assert!(rect.w > 0, "pane width should be > 0");
            assert!(rect.h > 0, "pane height should be > 0");
            assert!(rect.x >= inner.x);
            assert!(rect.y >= inner.y);
            assert!(rect.x + rect.w <= inner.x + inner.w);
            assert!(rect.y + rect.h <= inner.y + inner.h);
        }
    }

    #[test]
    fn split_increases_count() {
        let mut layout = Layout::from_grid(1, 1);
        assert_eq!(layout.pane_count(), 1);
        layout.split(0, Direction::Horizontal);
        assert_eq!(layout.pane_count(), 2);
        layout.split(0, Direction::Vertical);
        assert_eq!(layout.pane_count(), 3);
    }

    #[test]
    fn split_area_respects_min_pane_w() {
        // Roomy split — both halves should be ≥ MIN_PANE_W
        let area = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        let (a, b) = super::split_area(&area, Direction::Horizontal, 0.5);
        assert!(a.w >= super::MIN_PANE_W);
        assert!(b.w >= super::MIN_PANE_W);

        // Extreme ratio (0.01) used to collapse one side to 1 cell
        let (a, b) = super::split_area(&area, Direction::Horizontal, 0.01);
        assert!(a.w >= super::MIN_PANE_W);
        assert!(b.w >= super::MIN_PANE_W);
    }

    #[test]
    fn can_split_rejects_undersized_area() {
        // 5 cols can't fit two 3-col panes plus a separator → reject
        let tiny = Rect {
            x: 0,
            y: 0,
            w: 5,
            h: 24,
        };
        assert!(!super::can_split(&tiny, Direction::Horizontal));

        // 7 cols (3 + 1 + 3) is the minimum that satisfies horizontal split
        let just_enough = Rect { w: 7, ..tiny };
        assert!(super::can_split(&just_enough, Direction::Horizontal));
    }

    #[test]
    fn remove_decreases_count() {
        let mut layout = Layout::from_grid(1, 2);
        assert_eq!(layout.pane_count(), 2);
        assert!(layout.remove(1));
        assert_eq!(layout.pane_count(), 1);
    }

    #[test]
    fn cannot_remove_last_pane() {
        let mut layout = Layout::from_grid(1, 1);
        assert!(!layout.remove(0));
        assert_eq!(layout.pane_count(), 1);
    }

    #[test]
    fn equalize_makes_equal_rects() {
        let mut layout = Layout::from_grid(1, 3);
        let inner = inner_80x24();
        layout.equalize();
        let rects = layout.pane_rects(&inner);
        let widths: Vec<u16> = layout
            .pane_ids()
            .iter()
            .filter_map(|id| rects.get(id).map(|r| r.w))
            .collect();
        let max_diff = widths.iter().max().unwrap() - widths.iter().min().unwrap();
        assert!(
            max_diff <= 1,
            "widths should differ by at most 1, got {:?}",
            widths
        );
    }

    #[test]
    fn find_at_returns_correct_pane() {
        let layout = Layout::from_grid(1, 2);
        let inner = inner_80x24();
        let rects = layout.pane_rects(&inner);
        for (&pid, rect) in &rects {
            let found = layout.find_at(rect.x + rect.w / 2, rect.y + rect.h / 2, &inner);
            assert_eq!(
                found,
                Some(pid),
                "clicking center of pane {} should find it",
                pid
            );
        }
    }

    #[test]
    fn navigate_horizontal() {
        let layout = Layout::from_grid(1, 3);
        let inner = inner_80x24();
        assert_eq!(layout.navigate(0, NavDir::Right, &inner), Some(1));
        assert_eq!(layout.navigate(1, NavDir::Right, &inner), Some(2));
        assert_eq!(layout.navigate(2, NavDir::Left, &inner), Some(1));
        assert_eq!(layout.navigate(0, NavDir::Left, &inner), None);
    }

    #[test]
    fn navigate_vertical() {
        let layout = Layout::from_grid(2, 1);
        let inner = inner_80x24();
        assert_eq!(layout.navigate(0, NavDir::Down, &inner), Some(1));
        assert_eq!(layout.navigate(1, NavDir::Up, &inner), Some(0));
    }

    #[test]
    fn next_pane_cycles() {
        let layout = Layout::from_grid(1, 3);
        assert_eq!(layout.next_pane(0), 1);
        assert_eq!(layout.next_pane(1), 2);
        assert_eq!(layout.next_pane(2), 0); // wraps
    }

    #[test]
    fn separators_count() {
        let layout = Layout::from_grid(2, 3);
        let inner = inner_80x24();
        let outer = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        let seps = layout.separators(&inner, &outer);
        // 2x3 grid: 2 horizontal separators (between 3 cols) + 1 vertical (between 2 rows)
        // Actually with binary tree: each Split creates 1 separator
        // Grid 2x3 tree: V(Row3, Row3) → 1 vertical sep + 2 H-seps per row = 5
        assert_eq!(seps.len(), 5);
    }

    #[test]
    fn set_ratio_and_verify() {
        let mut layout = Layout::from_grid(1, 2);
        let inner = inner_80x24();
        layout.set_ratio_at_path(&[], 0.3);
        let rects = layout.pane_rects(&inner);
        let r0 = &rects[&0];
        let r1 = &rects[&1];
        assert!(r0.w < r1.w, "pane 0 should be narrower after 0.3 ratio");
    }

    // ── Layout Spec Tests ──

    #[test]
    fn spec_simple_ratio() {
        let layout = Layout::from_spec("7:3").unwrap();
        assert_eq!(layout.pane_count(), 2);
        let inner = inner_80x24();
        let rects = layout.pane_rects(&inner);
        let ids = layout.pane_ids();
        let w0 = rects[&ids[0]].w;
        let w1 = rects[&ids[1]].w;
        assert!(
            w0 > w1,
            "7:3 ratio — left should be wider: {} vs {}",
            w0,
            w1
        );
    }

    #[test]
    fn spec_three_equal() {
        let layout = Layout::from_spec("1:1:1").unwrap();
        assert_eq!(layout.pane_count(), 3);
    }

    #[test]
    fn spec_two_rows() {
        let layout = Layout::from_spec("7:3/5:5").unwrap();
        assert_eq!(layout.pane_count(), 4);
    }

    #[test]
    fn spec_mixed() {
        let layout = Layout::from_spec("1/1:1").unwrap();
        assert_eq!(layout.pane_count(), 3);
    }

    #[test]
    fn spec_single() {
        let layout = Layout::from_spec("1").unwrap();
        assert_eq!(layout.pane_count(), 1);
    }

    #[test]
    fn spec_invalid() {
        assert!(Layout::from_spec("").is_err());
        assert!(Layout::from_spec("abc").is_err());
        assert!(Layout::from_spec("0:0").is_err());
        assert!(Layout::from_spec("10").is_err());
    }

    #[test]
    fn spec_presets() {
        let ide = Layout::from_spec("ide").unwrap();
        assert_eq!(ide.pane_count(), 4); // 7:3/1:1 = 2+2

        let dev = Layout::from_spec("dev").unwrap();
        assert_eq!(dev.pane_count(), 2); // 7:3

        let monitor = Layout::from_spec("monitor").unwrap();
        assert_eq!(monitor.pane_count(), 3); // 1:1:1

        let quad = Layout::from_spec("quad").unwrap();
        assert_eq!(quad.pane_count(), 4); // 2x2

        let stack = Layout::from_spec("stack").unwrap();
        assert_eq!(stack.pane_count(), 3); // 1/1/1

        let main = Layout::from_spec("main").unwrap();
        assert_eq!(main.pane_count(), 3); // 6:4/1

        let trio = Layout::from_spec("trio").unwrap();
        assert_eq!(trio.pane_count(), 3); // 1/1:1
    }
}
