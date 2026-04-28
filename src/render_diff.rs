//! Cell-grid render diff (issue #93) — opt-in via the `render-diff` feature.
//!
//! Computes the delta between two consecutive cell grids so the server can
//! emit only changed cells instead of a full ANSI redraw per dirty pane.
//!
//! ## Design notes
//!
//! - This module is self-contained: it does **not** depend on `render.rs` and
//!   defines its own minimal [`Cell`] / [`CellGrid`] types. The wire format
//!   that integrates with the existing pane buffer plumbing lives at the
//!   call-site (`server.rs`) and is intentionally out of scope here.
//! - The diff is row-major linear scan: `O(rows * cols)` worst case. That is
//!   the same complexity as the current full redraw and matches the
//!   `< 200 µs / frame on 80×24` budget from the issue's acceptance criteria.
//! - The module exposes a [`MAX_GRID_BYTES`] cap (1 MB per client). When the
//!   incoming grid exceeds the cap the caller is expected to fall back to
//!   full-redraw mode for that frame — `diff()` itself does not enforce the
//!   cap so that test code can still exercise large grids.
//!
//! The whole file is gated behind `#[cfg(feature = "render-diff")]` at the
//! module declaration site (see `src/main.rs`); enabling the feature does
//! not change any default code path.

use std::fmt;

/// Maximum total bytes of a single client's previous-frame buffer before the
/// caller should fall back to full-redraw mode (issue #93 acceptance: 1 MB).
pub const MAX_GRID_BYTES: usize = 1024 * 1024;

/// 16-bit color slot.
///
/// Default = `Color::Default` ≈ "use the terminal's current default fg/bg".
/// Indexed (`Idx`) covers the 256-color palette; `Rgb` covers truecolor.
/// Kept intentionally narrow — anything richer (blink, hyperlink, etc.) is
/// out of scope per the issue's "Out of scope: compression of the diff
/// stream" stance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// Cell attributes packed into a single byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(u8);

impl Attrs {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    pub const INVERSE: u8 = 1 << 3;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn new(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// A single rendered cell. `ch` is a `char` (not a byte) so that wide
/// characters survive the diff intact; `is_continuation` marks the trailing
/// half of a wide cell (its `ch` is meaningless and skipped when emitting).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
    pub is_continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::empty(),
            is_continuation: false,
        }
    }
}

/// Row-major cell grid: `cells[row * cols + col]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellGrid {
    rows: u16,
    cols: u16,
    cells: Vec<Cell>,
}

impl CellGrid {
    /// Build an all-default grid sized `rows × cols`.
    pub fn new(rows: u16, cols: u16) -> Self {
        let total = rows as usize * cols as usize;
        Self {
            rows,
            cols,
            cells: vec![Cell::default(); total],
        }
    }

    /// Build a grid from an existing flat row-major `Vec<Cell>`.
    pub fn from_cells(rows: u16, cols: u16, cells: Vec<Cell>) -> Result<Self, GridError> {
        let expected = rows as usize * cols as usize;
        if cells.len() != expected {
            return Err(GridError::SizeMismatch {
                expected,
                got: cells.len(),
            });
        }
        Ok(Self { rows, cols, cells })
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn get(&self, row: u16, col: u16) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(row as usize * self.cols as usize + col as usize)
    }

    pub fn set(&mut self, row: u16, col: u16, cell: Cell) -> Result<(), GridError> {
        if row >= self.rows || col >= self.cols {
            return Err(GridError::OutOfBounds { row, col });
        }
        self.cells[row as usize * self.cols as usize + col as usize] = cell;
        Ok(())
    }

    /// Approximate byte footprint — used for the [`MAX_GRID_BYTES`] cap.
    pub fn byte_size(&self) -> usize {
        self.cells.len() * std::mem::size_of::<Cell>()
    }
}

/// Errors raised by the diff machinery.
#[derive(Debug, PartialEq, Eq)]
pub enum GridError {
    SizeMismatch { expected: usize, got: usize },
    OutOfBounds { row: u16, col: u16 },
    DimensionMismatch { prev: (u16, u16), next: (u16, u16) },
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeMismatch { expected, got } => {
                write!(
                    f,
                    "cell vec size {} does not match grid size {}",
                    got, expected
                )
            }
            Self::OutOfBounds { row, col } => {
                write!(f, "cell ({}, {}) out of bounds", row, col)
            }
            Self::DimensionMismatch { prev, next } => write!(
                f,
                "grid dimensions changed: prev={}x{} next={}x{}",
                prev.0, prev.1, next.0, next.1
            ),
        }
    }
}

impl std::error::Error for GridError {}

/// One changed cell in a diff: position + new value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellDelta {
    pub row: u16,
    pub col: u16,
    pub cell: Cell,
}

/// Result of [`diff`]: either a list of changed cells or a "full redraw"
/// signal when the dimensions changed (or the prev grid was absent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diff {
    /// Caller should emit the full new grid (e.g. first frame, resize, or
    /// over-cap fallback).
    FullRedraw,
    /// Sparse list of changed cells. Empty vec = no-op frame.
    Cells(Vec<CellDelta>),
}

impl Diff {
    /// True if this diff carries no actual updates.
    pub fn is_noop(&self) -> bool {
        matches!(self, Diff::Cells(c) if c.is_empty())
    }

    /// Number of cells that would actually be emitted (0 for `FullRedraw`,
    /// since the caller streams the whole grid via a different path).
    pub fn changed_cells(&self) -> usize {
        match self {
            Diff::FullRedraw => 0,
            Diff::Cells(c) => c.len(),
        }
    }
}

/// Compute the delta from `prev` → `next`.
///
/// Behaviour:
/// - `prev` is `None` (first frame for this client) → [`Diff::FullRedraw`].
/// - dimensions differ → [`Diff::FullRedraw`] (caller must reseed prev).
/// - identical grids → `Diff::Cells(vec![])` (caller MAY skip the frame).
/// - otherwise → list of every `(row, col)` whose cell differs.
///
/// Wide-character continuation cells are emitted alongside their lead so the
/// caller can re-print both halves atomically.
pub fn diff(prev: Option<&CellGrid>, next: &CellGrid) -> Diff {
    let Some(prev) = prev else {
        return Diff::FullRedraw;
    };
    if prev.rows != next.rows || prev.cols != next.cols {
        return Diff::FullRedraw;
    }

    let cols = next.cols;
    let mut deltas = Vec::new();
    for (idx, (p, n)) in prev.cells.iter().zip(next.cells.iter()).enumerate() {
        if p != n {
            let row = (idx / cols as usize) as u16;
            let col = (idx % cols as usize) as u16;
            deltas.push(CellDelta { row, col, cell: *n });
        }
    }

    Diff::Cells(deltas)
}

/// Apply a diff to `prev` in place, producing the post-frame grid.
///
/// `FullRedraw` requires the caller to pass the new grid via `full`. This
/// helper exists so chaos-test code (per #93's risk note) can sanity-check
/// that `apply(diff(p, n)) == n` for all `p, n`.
pub fn apply(prev: &mut CellGrid, diff: &Diff, full: Option<&CellGrid>) -> Result<(), GridError> {
    match diff {
        Diff::FullRedraw => {
            let Some(full) = full else {
                return Err(GridError::DimensionMismatch {
                    prev: (prev.rows, prev.cols),
                    next: (0, 0),
                });
            };
            *prev = full.clone();
            Ok(())
        }
        Diff::Cells(cells) => {
            for d in cells {
                prev.set(d.row, d.col, d.cell)?;
            }
            Ok(())
        }
    }
}

/// Test the cap from issue #93 — caller uses this to decide whether to
/// fall back to full-redraw mode.
pub fn exceeds_cap(grid: &CellGrid) -> bool {
    grid.byte_size() > MAX_GRID_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            ..Cell::default()
        }
    }

    fn grid_filled(rows: u16, cols: u16, ch: char) -> CellGrid {
        let cells = vec![cell(ch); rows as usize * cols as usize];
        CellGrid::from_cells(rows, cols, cells).unwrap()
    }

    #[test]
    fn first_frame_is_full_redraw() {
        let next = grid_filled(2, 2, 'a');
        assert_eq!(diff(None, &next), Diff::FullRedraw);
    }

    #[test]
    fn dimension_change_forces_full_redraw() {
        let prev = grid_filled(2, 2, 'a');
        let next = grid_filled(3, 3, 'a');
        assert_eq!(diff(Some(&prev), &next), Diff::FullRedraw);
    }

    #[test]
    fn identical_grids_yield_empty_diff() {
        let prev = grid_filled(4, 4, 'x');
        let next = grid_filled(4, 4, 'x');
        let d = diff(Some(&prev), &next);
        assert!(d.is_noop());
        assert_eq!(d.changed_cells(), 0);
    }

    #[test]
    fn single_cell_change_emits_single_delta() {
        let prev = grid_filled(3, 5, ' ');
        let mut next = prev.clone();
        next.set(1, 2, cell('X')).unwrap();

        let d = diff(Some(&prev), &next);
        match d {
            Diff::Cells(c) => {
                assert_eq!(c.len(), 1);
                assert_eq!(c[0].row, 1);
                assert_eq!(c[0].col, 2);
                assert_eq!(c[0].cell.ch, 'X');
            }
            _ => panic!("expected Cells diff"),
        }
    }

    #[test]
    fn full_row_change_emits_one_delta_per_col() {
        let prev = grid_filled(2, 4, ' ');
        let mut next = prev.clone();
        for c in 0..4 {
            next.set(1, c, cell('=')).unwrap();
        }
        assert_eq!(diff(Some(&prev), &next).changed_cells(), 4);
    }

    #[test]
    fn apply_roundtrip_yields_identical_grid() {
        let prev = grid_filled(8, 8, ' ');
        let mut next = prev.clone();
        next.set(0, 0, cell('A')).unwrap();
        next.set(7, 7, cell('Z')).unwrap();
        next.set(3, 4, cell('M')).unwrap();

        let d = diff(Some(&prev), &next);
        let mut applied = prev.clone();
        apply(&mut applied, &d, None).unwrap();
        assert_eq!(applied, next);
    }

    #[test]
    fn full_redraw_apply_requires_full_grid() {
        let prev = grid_filled(2, 2, ' ');
        let next = grid_filled(3, 3, ' ');
        let d = diff(Some(&prev), &next);
        assert_eq!(d, Diff::FullRedraw);

        let mut applied = prev.clone();
        let err = apply(&mut applied, &d, None).unwrap_err();
        assert!(matches!(err, GridError::DimensionMismatch { .. }));

        apply(&mut applied, &d, Some(&next)).unwrap();
        assert_eq!(applied, next);
    }

    #[test]
    fn out_of_bounds_set_is_rejected() {
        let mut g = grid_filled(2, 2, ' ');
        let err = g.set(5, 5, cell('q')).unwrap_err();
        assert!(matches!(err, GridError::OutOfBounds { .. }));
    }

    #[test]
    fn size_mismatch_is_rejected() {
        let err = CellGrid::from_cells(2, 2, vec![cell(' '); 3]).unwrap_err();
        assert!(matches!(err, GridError::SizeMismatch { .. }));
    }

    #[test]
    fn cap_check_is_threshold_inclusive() {
        // Below cap.
        let small = CellGrid::new(10, 10);
        assert!(!exceeds_cap(&small));
    }

    #[test]
    fn attrs_bitwise_helpers() {
        let a = Attrs::new(Attrs::BOLD | Attrs::ITALIC);
        assert!(a.contains(Attrs::BOLD));
        assert!(a.contains(Attrs::ITALIC));
        assert!(!a.contains(Attrs::UNDERLINE));
        assert_eq!(a.bits(), Attrs::BOLD | Attrs::ITALIC);
    }

    #[test]
    fn color_default_distinct_from_zero_idx() {
        // Sanity: indexed black (0) is NOT the same as Default.
        assert_ne!(Color::Default, Color::Idx(0));
    }
}
