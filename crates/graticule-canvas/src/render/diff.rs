//! Grid diff: compare a dirty back grid against the front grid (what the
//! terminal currently shows) and emit a minimal stream of [`Draw`] commands.
//!
//! This is the nvim `grid_line` analog. It walks only the dirty rows and only
//! from each row's leftmost dirty column, grouping consecutive equal-style cells
//! into run-length packed [`Draw::Cells`] commands.
//!
//! The diff is **pure**: it produces commands and mutates neither grid. That
//! keeps it unit-testable (feed two grids, assert the commands), lets the
//! backend own all I/O and capability negotiation, and — critically — means a
//! failed terminal write cannot advance the model. Promotion is a separate step
//! ([`promote`]) that the frame calls only after the bytes have been flushed.
//!
//! # Cost
//!
//! - Idle frame: `O(1)`. A clean [`Damage`] returns immediately.
//! - Changed cell: `O(changed cells)`, plus one `O(w)` backward scan per dirty
//!   row to locate its uniform blank tail (which is what makes the
//!   `clr_eol` fast path cheap to find).
//! - Scroll candidate search: bounded by `MAX_SCROLL_LINES` candidate shifts
//!   over the dirty band, with a full-row anchor check that rejects a candidate
//!   before the band walk. No grid is copied: the post-scroll front is a
//!   *view*, not a rotated buffer.

use crate::buffer::Style;
use crate::buffer::cell::CellWidth;
use crate::buffer::cell::{Cell, CompactSymbol};
use crate::buffer::grid::{BandRotation, CellGrid, Damage};

/// One logical draw operation the backend should perform. The backend turns
/// these into escape codes; keeping them logical (not raw bytes) means the same
/// diff drives a real terminal and a test capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draw {
    /// Scroll a contiguous band of rows down by `amount` before painting the
    /// rest of the frame. Content moves down, exposing `amount` blank rows at
    /// the top of the band. Emitted when history was prepended above the
    /// viewport.
    ScrollDown {
        /// First row of the band.
        y: u16,
        /// Number of rows in the band.
        height: u16,
        /// Rows to shift.
        amount: u16,
    },
    /// Scroll a contiguous band of rows up by `amount`: content moves up,
    /// exposing `amount` blank rows at the bottom. This is the streaming-append
    /// shape — a new row lands at the bottom and every settled row above shifts
    /// up one — so a whole-viewport repaint collapses into one terminal scroll
    /// plus the genuinely-new row. Rows that were already identical stay
    /// identical after the rotation, which is what keeps streaming from
    /// flickering on terminals without synchronized updates.
    ScrollUp {
        /// First row of the band.
        y: u16,
        /// Number of rows in the band.
        height: u16,
        /// Rows to shift.
        amount: u16,
    },
    /// Paint a run of cells starting at `(x, y)`. Every cell in the run carries
    /// the same style; the cursor is positioned at `(x, y)` before writing.
    /// Wide-glyph continuation cells are omitted (the head paints both
    /// columns), so the backend advances by each entry's
    /// [`CellWidth::advance`].
    Cells {
        /// Leftmost column of the run.
        x: u16,
        /// Row of the run.
        y: u16,
        /// The uniform style for the whole run.
        style: Style,
        /// `(symbol, width)` pairs in visual order.
        cells: Vec<(CompactSymbol, CellWidth)>,
    },
    /// Clear from `(x, y)` to the end of that row with `style`. On BCE terminals
    /// the backend emits `clr_eol`; without BCE it paints `width` explicit
    /// styled spaces.
    ClearEol {
        /// Column the clear starts at.
        x: u16,
        /// Row to clear.
        y: u16,
        /// The style the cleared cells already carry.
        style: Style,
        /// Columns covered, i.e. `grid_width - x`.
        width: u16,
    },
}

/// A complete diff over a grid: the draw commands plus the grid size they were
/// computed against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawCmd {
    /// The commands, in the order the backend must perform them.
    pub draws: Vec<Draw>,
    /// Grid width the commands were computed against.
    pub w: u16,
    /// Grid height the commands were computed against.
    pub h: u16,
}

impl DrawCmd {
    /// The leading scroll translation, if this frame's diff resolved one.
    /// Always `draws[0]` when present; carried separately so callers that only
    /// need the rotation do not pattern-match the list.
    pub fn scroll(&self) -> Option<&Draw> {
        self.draws
            .first()
            .filter(|d| matches!(d, Draw::ScrollDown { .. } | Draw::ScrollUp { .. }))
    }

    /// Whether the diff found nothing to paint.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// How far a scroll candidate may shift; beyond this a repaint is cheap anyway
/// and the scan would waste work.
const MAX_SCROLL_LINES: u16 = 256;

/// A translation candidate the diff decided to emit as a scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrollOp {
    pub(super) up: bool,
    pub(super) y: u16,
    pub(super) height: u16,
    pub(super) amount: u16,
    saved_rows: usize,
}

/// The front grid as the terminal will show it *after* this frame's scroll, if
/// any, has been applied.
///
/// Mapping each screen row to the front row that will occupy it removes the need
/// to materialise a rotated copy of the front grid: the previous implementation
/// cloned the whole grid (hundreds of KB) on every scrolled frame purely to
/// compare against a projection. A row outside the scroll band is its own
/// source; a row inside it maps by the shift; a row the shift exposed is
/// vacated and reads as the terminal's default blank.
struct ProjectedFront<'a> {
    front: &'a CellGrid,
    scroll: Option<ScrollOp>,
}

impl<'a> ProjectedFront<'a> {
    fn new(front: &'a CellGrid, scroll: Option<ScrollOp>) -> Self {
        Self { front, scroll }
    }

    /// The front row that will occupy screen row `y` after the pending scroll,
    /// or `None` when the scroll exposes that row as blank.
    fn source_row(&self, y: u16) -> Option<u16> {
        let Some(op) = self.scroll else {
            return Some(y);
        };
        let band_end = op.y.saturating_add(op.height);
        if y < op.y || y >= band_end {
            // Outside the band the rotation changes nothing.
            return Some(y);
        }
        if op.up {
            let src = y.checked_add(op.amount)?;
            (src < band_end).then_some(src)
        } else {
            let src = y.checked_sub(op.amount)?;
            (src >= op.y).then_some(src)
        }
    }

    /// The post-scroll row slice for screen row `y`, or `None` when the row is
    /// exposed (and therefore blank).
    fn row(&self, y: u16) -> Option<&'a [Cell]> {
        self.source_row(y).and_then(|src| self.front.row_slice(src))
    }
}

/// Diff `back` (desired) against `front` (current terminal state), emitting draw
/// commands only for the region `damage` marks dirty.
///
/// Neither grid nor the damage tracker is modified: for a proven scroll
/// translation the comparison runs against a `ProjectedFront` view, and the
/// real front grid is rotated only by [`promote_scrolled`] after the backend has
/// successfully flushed the commands.
pub fn diff(back: &CellGrid, damage: &Damage, front: &CellGrid) -> DrawCmd {
    let (w, h) = back.size();
    debug_assert_eq!(
        front.size(),
        (w, h),
        "front grid must match back grid size before diffing"
    );

    let mut draws = Vec::new();

    let Some((lo, hi)) = damage.rows() else {
        return DrawCmd { draws, w, h };
    };

    // Translation stage. A band of rows that moved wholesale (a streaming
    // transcript pushing history up) becomes one scroll op instead of a full
    // repaint. Correctness contract for a candidate shift k:
    //   * every overlapping row must be identical to its shifted source,
    //     regardless of dirty bookkeeping;
    //   * every vacated non-blank row must be dirty so it is repainted;
    //   * at least one row that differs at its current coordinate must become
    //     equal after the shift. Rewritten-but-identical and repeated blank rows
    //     are not scroll evidence.
    // These conditions make translation a proof, not a similarity heuristic.
    let scroll = if hi.saturating_sub(lo) >= 2 && damage.is_dirty() {
        detect_scroll(back, damage, front, lo, hi, MAX_SCROLL_LINES)
    } else {
        None
    };

    if let Some(op) = scroll {
        draws.push(if op.up {
            Draw::ScrollUp {
                y: op.y,
                height: op.height,
                amount: op.amount,
            }
        } else {
            Draw::ScrollDown {
                y: op.y,
                height: op.height,
                amount: op.amount,
            }
        });
    }

    let projected = ProjectedFront::new(front, scroll);
    // The terminal exposes default blanks where a scroll vacated a row.
    let terminal_blank = Cell::blank();

    for y in lo..=hi {
        let Some(start) = damage.col_of(y) else {
            continue;
        };
        // A terminal scroll blanks each vacated row in full. Its original dirty
        // column was measured against the pre-scroll row and therefore cannot be
        // used as a safe comparison prefix after projection: cells before `start`
        // may have matched the old row but differ from the new blank row.
        // Comparing every dirty row in the translated band from column zero is
        // both exact and cheap (overlapping translated rows compare equal
        // immediately).
        let start = if scroll.is_some() { 0 } else { start };
        let tail = blank_tail_suffix(back, y);
        diff_row(
            &mut draws,
            back,
            &projected,
            &terminal_blank,
            y,
            start,
            w,
            tail,
        );
    }

    DrawCmd { draws, w, h }
}

/// The maximal uniform blank suffix of row `y`: the leftmost column `s` such
/// that `row[s..]` is all width-1 spaces sharing one style.
///
/// Returns the suffix's style alongside `s`, or `None` when the row does not end
/// in a blank. Computed once per dirty row with a single backward scan, which is
/// what makes the `clr_eol` fast path affordable: the previous implementation
/// re-scanned to the end of the row for every changed cell, making a pathological
/// row quadratic.
fn blank_tail_suffix(back: &CellGrid, y: u16) -> Option<(u16, Style)> {
    let row = back.row_slice(y)?;
    let style = row.last()?.style;
    if !is_blank(&row[row.len() - 1]) {
        return None;
    }
    let mut start = row.len();
    for (i, cell) in row.iter().enumerate().rev() {
        if cell.style != style || !is_blank(cell) {
            break;
        }
        start = i;
    }
    Some((start as u16, style))
}

/// Whether a cell is a width-1 space (what `clr_eol` conceptually writes).
#[inline]
fn is_blank(cell: &Cell) -> bool {
    cell.width == CellWidth::Narrow && cell.symbol == " "
}

/// Diff one row from `start_col` to the right edge, appending draw commands.
#[allow(clippy::too_many_arguments)]
fn diff_row(
    draws: &mut Vec<Draw>,
    back: &CellGrid,
    front: &ProjectedFront<'_>,
    terminal_blank: &Cell,
    y: u16,
    start: u16,
    w: u16,
    tail: Option<(u16, Style)>,
) {
    // Walk the row; whenever the back cell differs from the projected front
    // cell, start accumulating a run. Runs group cells that share a style; a
    // style change flushes the current run and starts a new one.
    let mut run: Option<(u16, Style)> = None;
    let mut run_cells: Vec<(CompactSymbol, CellWidth)> = Vec::new();
    let front_row = front.row(y);

    let mut x = start;
    while x < w {
        let Some(back_cell) = back.get(x, y) else {
            break;
        };
        let front_cell = front_row
            .and_then(|row| row.get(x as usize))
            .unwrap_or(terminal_blank);

        // A wide glyph's trailing column is painted by its head (at x-1) and is
        // never emitted on its own. It still consumes its slot.
        if back_cell.width.is_continuation() {
            if back_cell != front_cell && x > 0 {
                // If the screen's continuation cell does not match the back grid,
                // the wide-glyph pair on screen is broken or out of sync.
                // A continuation cell cannot be emitted alone; we must emit the head
                // glyph at x-1 so the terminal paints the full wide glyph and repairs
                // this column.
                if let Some(head_cell) = back.get(x - 1, y) {
                    flush_run(draws, &mut run, &mut run_cells, y);
                    run = Some((x - 1, head_cell.style));
                    run_cells.push((head_cell.symbol.clone(), head_cell.width));
                }
            }
            x += 1;
            continue;
        }

        if back_cell == front_cell {
            flush_run(draws, &mut run, &mut run_cells, y);
            x += 1;
            continue;
        }

        // From the blank suffix onward the rest of the row is one uniform blank,
        // so a single end-of-line clear covers it.
        if let Some((tail_start, tail_style)) = tail
            && x >= tail_start
        {
            flush_run(draws, &mut run, &mut run_cells, y);
            draws.push(Draw::ClearEol {
                x,
                y,
                style: tail_style,
                width: w.saturating_sub(x),
            });
            break;
        }

        match run {
            None => run = Some((x, back_cell.style)),
            Some((_, style)) if style != back_cell.style => {
                flush_run(draws, &mut run, &mut run_cells, y);
                run = Some((x, back_cell.style));
            }
            Some(_) => {}
        }

        run_cells.push((back_cell.symbol.clone(), back_cell.width));
        x += back_cell.width.advance();
    }

    flush_run(draws, &mut run, &mut run_cells, y);
}

/// Emit a pending run as a [`Draw::Cells`] if it is non-empty.
fn flush_run(
    draws: &mut Vec<Draw>,
    run: &mut Option<(u16, Style)>,
    run_cells: &mut Vec<(CompactSymbol, CellWidth)>,
    y: u16,
) {
    if let Some((x, style)) = run.take()
        && !run_cells.is_empty()
    {
        draws.push(Draw::Cells {
            x,
            y,
            style,
            cells: std::mem::take(run_cells),
        });
    }
}

/// Full-row equality across two grids.
#[inline]
fn rows_equal(back: &CellGrid, front: &CellGrid, back_y: u16, front_y: u16) -> bool {
    match (back.row_slice(back_y), front.row_slice(front_y)) {
        (Some(b), Some(f)) => b == f,
        (None, None) => true,
        _ => false,
    }
}

/// Detect a whole-band vertical translation between the front and back grids
/// within the dirty band `lo..=hi`.
///
/// `up = true` models the streaming-append shape: content moves up by `k`, so
/// screen row `y` should hold `front[y + k]` (rows vacated at the band bottom
/// come up blank/new). `up = false` models a prepend: row `y` holds
/// `front[y - k]`.
///
/// Rows whose shifted source falls outside the band impose no translation
/// evidence (the caller repaints them or they are the vacated rows). The
/// candidate that saves the most genuinely changed rows wins; ties prefer the
/// smaller shift. The up direction is tried first because streaming append is the
/// common case.
fn detect_scroll(
    back: &CellGrid,
    damage: &Damage,
    front: &CellGrid,
    lo: u16,
    hi: u16,
    max_shift: u16,
) -> Option<ScrollOp> {
    let mut best: Option<ScrollOp> = None;
    for up in [true, false] {
        if let Some(op) = detect_scroll_direction(back, damage, front, lo, hi, max_shift, up) {
            let better = match best {
                Some(current) => {
                    op.saved_rows > current.saved_rows
                        || (op.saved_rows == current.saved_rows && op.amount < current.amount)
                }
                None => true,
            };
            if better {
                best = Some(op);
            }
        }
    }
    best
}

fn detect_scroll_direction(
    back: &CellGrid,
    damage: &Damage,
    front: &CellGrid,
    lo: u16,
    hi: u16,
    max_shift: u16,
    up: bool,
) -> Option<ScrollOp> {
    let (w, h) = back.size();
    if w == 0 || h == 0 || hi <= lo {
        return None;
    }
    let band_height = hi - lo + 1;
    let max_shift = max_shift.min(band_height.saturating_sub(1));
    if max_shift == 0 {
        return None;
    }

    // Screen row y after an `up` scroll holds front[y + k]; after a `down`
    // scroll it holds front[y - k]. Sources outside the band are vacated rows —
    // the scroll blanks them and the row diff repaints whatever the back grid
    // wants there.
    let source_row = |y: u16, k: u16| -> Option<u16> {
        if up {
            y.checked_add(k).filter(|&s| s <= hi && s < h)
        } else {
            y.checked_sub(k).filter(|&s| s >= lo)
        }
    };

    let mut best: Option<(usize, u16)> = None;
    let anchor_y = if up { lo } else { hi };
    for k in 1..=max_shift {
        // Fast anchor check: the first row in the shift direction MUST match its
        // shifted source, which rejects almost every candidate in one row
        // comparison instead of a full band walk.
        if let Some(anchor_src) = source_row(anchor_y, k)
            && !rows_equal(back, front, anchor_y, anchor_src)
        {
            continue;
        }

        let mut ok = true;
        let mut saved_changed_rows = 0usize;
        for y in lo..=hi {
            let row_is_dirty = damage.col_of(y).is_some();
            let Some(src) = source_row(y, k) else {
                // Vacated row: no evidence (it will be blanked and, if dirty,
                // repainted by the row diff).
                if !row_is_dirty {
                    // A CLEAN vacated row would be blanked with nobody
                    // repainting it — only safe when the back row is the
                    // terminal's default blank, including style. A visually
                    // blank row with a panel background is not equivalent:
                    // SU/SD exposes terminal-default cells, not styled ones.
                    let terminal_blank = Cell::blank();
                    let blank = back
                        .row_slice(y)
                        .is_none_or(|row| row.iter().all(|c| c == &terminal_blank));
                    if !blank {
                        ok = false;
                        break;
                    }
                }
                continue;
            };
            if !rows_equal(back, front, y, src) {
                // Dirty rows are not exempt. Repainting a suffix cannot prove
                // the untouched prefix survived an incorrect translation, and
                // permissive matching turns repeated blank rows into false
                // scroll evidence.
                ok = false;
                break;
            }
            if row_is_dirty && !rows_equal(back, front, y, y) {
                saved_changed_rows += 1;
            }
        }
        if ok && saved_changed_rows > 0 {
            let better = match best {
                Some((best_saved, best_k)) => {
                    saved_changed_rows > best_saved
                        || (saved_changed_rows == best_saved && k <= best_k)
                }
                None => true,
            };
            if better {
                best = Some((saved_changed_rows, k));
            }
        }
    }
    let (saved_rows, amount) = best?;
    Some(ScrollOp {
        up,
        y: lo,
        height: band_height,
        amount,
        saved_rows,
    })
}

/// Promote the back grid's dirty cells into the front grid, then clear the
/// damage tracker. Called by the frame loop *after* the backend has applied the
/// diff's commands — at which point the front grid faithfully mirrors the
/// terminal again.
pub fn promote(back: &CellGrid, damage: &mut Damage, front: &mut CellGrid) {
    promote_impl(back, damage, front, None);
}

/// [`promote`] for a frame whose diff began with a scroll: the scroll rotated
/// whole front rows whose columns the back grid's damage does not cover, so
/// those rows are copied in full (equal cells are cheap; unequal ones are
/// exactly the drift the rotation introduced).
pub fn promote_scrolled(back: &CellGrid, damage: &mut Damage, front: &mut CellGrid, cmd: &DrawCmd) {
    if let Some(scroll) = cmd.scroll() {
        match *scroll {
            Draw::ScrollDown { y, height, amount } => {
                front.rotate_band(y, height, BandRotation::Down, amount);
            }
            Draw::ScrollUp { y, height, amount } => {
                front.rotate_band(y, height, BandRotation::Up, amount);
            }
            _ => {}
        }
    }
    let band = cmd.scroll().and_then(|scroll| match *scroll {
        Draw::ScrollDown { y, height, .. } | Draw::ScrollUp { y, height, .. } => Some((y, height)),
        _ => None,
    });
    promote_impl(back, damage, front, band);
}

fn promote_impl(
    back: &CellGrid,
    damage: &mut Damage,
    front: &mut CellGrid,
    scrolled_band: Option<(u16, u16)>,
) {
    let (w, _h) = back.size();
    let (band_lo, band_hi) = scrolled_band
        .map(|(y, height)| (y, y.saturating_add(height)))
        .unwrap_or((u16::MAX, u16::MAX));
    if let Some((lo, hi)) = damage.rows() {
        for y in lo..=hi {
            let in_band = y >= band_lo && y < band_hi;
            let start = damage
                .col_of(y)
                .map(|start| if in_band { 0 } else { start });
            let Some(start) = start else {
                // A clean row inside the rotated band must still be synced: the
                // rotation moved its front content even though nothing in the
                // back row changed.
                if in_band {
                    copy_row_into_front(back, front, y, 0, w);
                }
                continue;
            };
            copy_row_into_front(back, front, y, start, w);
        }
    }
    damage.clear();
}

fn copy_row_into_front(back: &CellGrid, front: &mut CellGrid, y: u16, from: u16, w: u16) {
    for x in from..w {
        if let Some(cell) = back.get(x, y).cloned() {
            front.overwrite(x, y, cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::cell::Cell;
    use crate::buffer::grid::{CellGrid, Damage, Fit};
    use crate::buffer::{Color, Style};

    /// Test harness: packages a `CellGrid` with its `Damage` tracker to set up
    /// diff scenarios cleanly in unit tests.
    #[derive(Clone)]
    struct Pane {
        grid: CellGrid,
        damage: Damage,
    }

    impl Pane {
        fn new(w: u16, h: u16) -> Self {
            Self {
                grid: CellGrid::new(w, h),
                damage: Damage::new(h),
            }
        }
        fn put(&mut self, x: u16, y: u16, fit: Fit, style: Style, text: &str) {
            self.grid.put(&mut self.damage, x, y, fit, style, text);
        }
        fn set(&mut self, x: u16, y: u16, cell: Cell) {
            self.grid.set(&mut self.damage, x, y, cell);
        }
        fn fill_rect(&mut self, x: u16, y: u16, w: u16, h: u16, style: Style) {
            self.grid.fill_rect(&mut self.damage, x, y, w, h, style);
        }
        fn mark(&mut self, x: u16, y: u16) {
            self.damage.mark(x, y);
        }
        fn mark_all_dirty(&mut self) {
            self.damage.mark_all();
        }
        fn clear_dirty(&mut self) {
            self.damage.clear();
        }
        fn is_dirty(&self) -> bool {
            self.damage.is_dirty()
        }
        fn get(&self, x: u16, y: u16) -> Option<&Cell> {
            self.grid.get(x, y)
        }
        fn rotate_band(&mut self, y: u16, h: u16, dir: BandRotation, amount: u16) {
            self.grid.rotate_band(y, h, dir, amount);
        }
        fn diff(&self, front: &Pane) -> DrawCmd {
            super::diff(&self.grid, &self.damage, &front.grid)
        }
        fn promote(&mut self, front: &mut Pane) {
            super::promote(&self.grid, &mut self.damage, &mut front.grid);
        }
        fn promote_scrolled(&mut self, front: &mut Pane, cmd: &DrawCmd) {
            super::promote_scrolled(&self.grid, &mut self.damage, &mut front.grid, cmd);
        }
    }

    /// A grid whose rows spell the given labels, one per row (rest blank).
    fn labeled(w: u16, labels: &[&str]) -> Pane {
        let mut g = Pane::new(w, labels.len() as u16);
        for (y, label) in labels.iter().enumerate() {
            g.put(0, y as u16, Fit::Clip, Style::default(), label);
        }
        g.clear_dirty();
        g
    }

    fn grid(text: &str, w: u16, h: u16) -> Pane {
        let mut g = Pane::new(w, h);
        g.put(0, 0, Fit::Clip, Style::default(), text);
        g.clear_dirty();
        g
    }

    /// Streaming-transcript shape: the back grid holds the same history as the
    /// front but shifted **up** one row, with one new tail row.
    #[test]
    fn wholesale_up_shift_becomes_one_scroll_plus_new_row() {
        let front = labeled(10, &["h1", "h2", "h3", ""]);
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = back.diff(&front);
        match cmd.scroll() {
            Some(Draw::ScrollUp { y, height, amount }) => {
                assert_eq!(
                    (*y, *height, *amount),
                    (0, 3, 1),
                    "band rows 0..=2 scroll up by 1, got y={y} h={height} a={amount}"
                );
            }
            other => panic!(
                "expected a scroll op, got {other:?} — draws: {:?}",
                cmd.draws
            ),
        }
        // The genuinely-new row still repaints; the moved rows must NOT.
        let repaint_rows: Vec<u16> = cmd.draws[1..]
            .iter()
            .filter_map(|d| match d {
                Draw::Cells { y, .. } => Some(*y),
                _ => None,
            })
            .collect();
        assert!(
            repaint_rows.iter().all(|&y| y == 2),
            "only the new row repaints, got {repaint_rows:?}"
        );
    }

    #[test]
    fn vacated_scroll_row_repaints_prefix_that_was_clean_before_scroll() {
        let front = labeled(8, &["aaaaAAAA", "bbbbBBBB", "keepOLD!"]);
        let mut back = front.clone();
        back.put(0, 0, Fit::Clip, Style::default(), "bbbbBBBB");
        back.put(0, 1, Fit::Clip, Style::default(), "keepOLD!");
        // Only the suffix differs at this coordinate before scrolling, so the
        // retained dirty marker begins at column four. ScrollUp will blank the
        // whole row, however, and the unchanged `keep` prefix must be painted
        // again from column zero.
        back.put(0, 2, Fit::Clip, Style::default(), "keepNEW!");

        let cmd = back.diff(&front);
        assert!(matches!(
            cmd.scroll(),
            Some(Draw::ScrollUp { amount: 1, .. })
        ));
        assert!(cmd.draws.iter().any(|draw| {
            matches!(draw, Draw::Cells { x: 0, y: 2, cells, .. }
                if cells.iter().map(|(symbol, _)| symbol.as_str()).collect::<String>()
                    == "keepNEW!")
        }));
    }

    /// Prepend shape: content inserted above the viewport pushes history
    /// **down**. The scroll resolves to the down direction.
    #[test]
    fn wholesale_down_shift_becomes_one_scroll() {
        let front = labeled(10, &["h2", "h3", ""]);
        let mut back = labeled(10, &["NEW", "h2", "h3"]);
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = back.diff(&front);
        match cmd.scroll() {
            Some(Draw::ScrollDown { y, height, amount }) => {
                assert_eq!(
                    (*y, *height, *amount),
                    (0, 3, 1),
                    "rows 0..=2 scroll down by 1"
                );
            }
            other => panic!(
                "expected a scroll op, got {other:?} — draws: {:?}",
                cmd.draws
            ),
        }
    }

    /// A no-shift frame (in-place cell edits) must not emit a scroll.
    #[test]
    fn in_place_edits_do_not_scroll() {
        let mut back = labeled(10, &["one", "two"]);
        let front = labeled(10, &["one", "two"]);
        back.set(0, 0, Cell::narrow("O", Style::default()));
        let cmd = back.diff(&front);
        assert!(
            cmd.scroll().is_none(),
            "an in-place edit must never translate to a scroll: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn full_dirty_local_edit_with_repeated_blank_rows_does_not_scroll() {
        let front = labeled(12, &["Header", "", "body", "", "", ""]);
        let mut back = front.clone();
        back.set(0, 0, Cell::narrow("h", Style::default()));
        // Full-frame background/widget passes can conservatively mark every row.
        // Repeated blanks must never be treated as translation evidence.
        back.mark_all_dirty();

        let cmd = back.diff(&front);
        assert!(
            cmd.scroll().is_none(),
            "a local edit must not become a terminal scroll: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn scroll_rejects_clean_vacated_row_with_nondefault_style() {
        let mut front = labeled(6, &["A", "B", "C", "", "D"]);
        let panel = Style::default().bg(Color::Rgb(8, 9, 10));
        front.fill_rect(0, 3, 6, 1, panel);
        front.clear_dirty();

        let mut back = front.clone();
        // Desired rows 0..=2 are an exact two-row upward translation. Row 3
        // remains a clean, panel-colored blank; row 4 is genuinely new.
        for y in 0..=2 {
            for x in 0..6 {
                back.set(x, y, front.get(x, y + 2).unwrap().clone());
            }
        }
        back.put(0, 4, Fit::Clip, Style::default(), "NEW");

        let cmd = back.diff(&front);
        assert!(
            cmd.scroll().is_none(),
            "scrolling would erase the clean row's panel background: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn diff_does_not_mutate_front_before_successful_promotion() {
        let front = labeled(10, &["h1", "h2", "h3", ""]);
        let original: Vec<String> = (0..4)
            .map(|y| front.get(0, y).unwrap().symbol.to_string())
            .collect();
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = back.diff(&front);
        assert!(cmd.scroll().is_some());
        let after_diff: Vec<String> = (0..4)
            .map(|y| front.get(0, y).unwrap().symbol.to_string())
            .collect();
        assert_eq!(
            after_diff, original,
            "planning a scroll must not advance committed terminal state"
        );
    }

    /// Mismatched history (content changed AND moved) must not scroll — the safe
    /// fallback is the ordinary row repaint.
    #[test]
    fn mismatched_shift_does_not_scroll() {
        let front = labeled(10, &["hx", "h3", ""]);
        let mut back = labeled(10, &["h3", "NEW", ""]);
        back.clear_dirty();
        for (y, ch) in [(0u16, '3'), (1, 'N')] {
            back.set(1, y, Cell::narrow(ch.to_string(), Style::default()));
        }
        let cmd = back.diff(&front);
        assert!(
            cmd.scroll().is_none(),
            "a shift over changed history must fall back to repaint: {:?}",
            cmd.draws
        );
    }

    /// A frame whose only change is a shift with nothing new to paint is not
    /// worth a scroll — no anchor rows means no repaint, so the diff must
    /// resolve to nothing (the terminal already matches after any hypothetical
    /// scroll, and matches without one).
    #[test]
    fn pure_shift_without_repaint_is_not_a_scroll() {
        let front = labeled(10, &["a", "b", "c"]);
        let mut back = labeled(10, &["a", "b", "c"]);
        // Mark rows dirty with content equal to what's already there — the
        // degenerate "writer rewrote identical bytes" frame.
        back.set(0, 1, Cell::narrow("b", Style::default()));
        back.set(0, 2, Cell::narrow("c", Style::default()));
        let cmd = back.diff(&front);
        assert!(cmd.scroll().is_none(), "no-repaint frame must not scroll");
        assert!(
            cmd.draws
                .iter()
                .all(|d| !matches!(d, Draw::ScrollDown { .. })),
            "no scroll variant may appear: {:?}",
            cmd.draws
        );
    }

    /// promote after a scrolled diff leaves front mirroring back exactly.
    #[test]
    fn promote_after_scroll_syncs_front_to_back() {
        let mut front = labeled(10, &["h1", "h2", "h3", ""]);
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = back.diff(&front);
        assert!(cmd.scroll().is_some());
        back.promote_scrolled(&mut front, &cmd);

        // Front must now equal back row-for-row.
        for y in 0..4u16 {
            for x in 0..10u16 {
                assert_eq!(
                    back.get(x, y).map(|c| c.symbol.clone()),
                    front.get(x, y).map(|c| c.symbol.clone()),
                    "row {y} diverged after scrolled promote"
                );
            }
        }
        // And a second diff against the promoted front is empty.
        assert!(back.diff(&front).draws.is_empty());
    }

    /// Band rotation sanity: content moves, exposed rows blank, and the
    /// operation is a no-op on out-of-range inputs.
    #[test]
    fn rotate_band_moves_content_and_blanks_exposed_rows() {
        let mut g = labeled(4, &["aa", "bb", "cc"]);
        g.rotate_band(0, 3, BandRotation::Down, 1);
        assert_eq!(g.get(0, 0).unwrap().symbol, " ");
        assert_eq!(g.get(0, 1).unwrap().symbol, "a");
        assert_eq!(g.get(0, 2).unwrap().symbol, "b");

        let mut g2 = labeled(4, &["aa", "bb", "cc"]);
        g2.rotate_band(0, 3, BandRotation::Up, 1);
        assert_eq!(g2.get(0, 0).unwrap().symbol, "b");
        assert_eq!(g2.get(0, 1).unwrap().symbol, "c");
        assert_eq!(g2.get(0, 2).unwrap().symbol, " ");

        // Degenerate inputs are no-ops.
        let mut g3 = labeled(4, &["aa"]);
        g3.rotate_band(0, 3, BandRotation::Up, 0);
        g3.rotate_band(0, 3, BandRotation::Up, 3);
        g3.rotate_band(9, 3, BandRotation::Up, 1);
        assert_eq!(g3.get(0, 0).unwrap().symbol, "a");
    }

    #[test]
    fn identical_grids_emit_nothing() {
        let back = grid("abc", 4, 1);
        let mut front = grid("abc", 4, 1);
        let cmd = back.diff(&front);
        assert!(cmd.draws.is_empty());
        Pane::new(4, 1).promote(&mut front); // no-op smoke
    }

    #[test]
    fn single_cell_change_emits_one_run() {
        let mut back = grid("abc", 4, 1);
        back.set(1, 0, Cell::narrow("B", Style::default()));
        let front = grid("abc", 4, 1);

        let cmd = back.diff(&front);
        assert_eq!(cmd.draws.len(), 1);
        match &cmd.draws[0] {
            Draw::Cells { x, y, cells, .. } => {
                assert_eq!((*x, *y), (1, 0));
                assert_eq!(cells, &vec![("B".into(), CellWidth::Narrow)]);
            }
            other => panic!("expected Cells, got {other:?}"),
        }
    }

    #[test]
    fn run_breaks_on_style_change() {
        let mut back = grid("abcd", 4, 1);
        back.set(
            2,
            0,
            Cell::narrow("c", Style::default().fg(Color::Rgb(1, 1, 1))),
        );
        // And change 'd' too with the same new style -> same run as 'c'.
        back.set(
            3,
            0,
            Cell::narrow("D", Style::default().fg(Color::Rgb(1, 1, 1))),
        );
        let front = grid("abcd", 4, 1);

        let cmd = back.diff(&front);
        // One run: cols 2..4, uniform style.
        assert_eq!(cmd.draws.len(), 1);
    }

    #[test]
    fn wide_glyph_head_emitted_continuation_skipped() {
        let mut back = grid("", 6, 1);
        back.put(0, 0, Fit::Clip, Style::default(), "😀a");
        let front = Pane::new(6, 1);

        let cmd = back.diff(&front);
        // The continuation cell at col 1 is skipped; we get one run with the
        // wide head and 'a'.
        assert_eq!(cmd.draws.len(), 1);
        match &cmd.draws[0] {
            Draw::Cells { cells, .. } => {
                assert_eq!(cells[0], ("😀".into(), CellWidth::Wide));
                assert_eq!(cells[1], ("a".into(), CellWidth::Narrow));
                assert_eq!(cells.len(), 2);
            }
            other => panic!("expected Cells, got {other:?}"),
        }
    }

    #[test]
    fn clean_rows_outside_dirty_range_are_skipped() {
        let mut back = grid("abc", 4, 3);
        back.clear_dirty();
        back.set(0, 2, Cell::narrow("Z", Style::default()));
        let front = grid("abc", 4, 3);

        let cmd = back.diff(&front);
        assert!(
            cmd.draws
                .iter()
                .all(|d| matches!(d, Draw::Cells { y, .. } if *y == 2))
        );
    }

    #[test]
    fn promote_syncs_front_and_clears_dirty() {
        let mut back = grid("abc", 4, 1);
        back.set(1, 0, Cell::narrow("B", Style::default()));
        let mut front = grid("abc", 4, 1);

        back.promote(&mut front);
        assert_eq!(front.get(1, 0).unwrap().symbol, "B");
        assert!(!back.is_dirty());
        // A second diff against the promoted front is now empty.
        assert!(back.diff(&front).draws.is_empty());
    }

    #[test]
    fn wide_glyph_selection_toggle_diffs_head_only() {
        let panel = Color::Rgb(18, 19, 19);
        let sel = Color::Rgb(38, 48, 44);
        let w = 6u16;
        let mut front = Pane::new(w, 1);
        front.put(
            0,
            0,
            Fit::Clip,
            Style::default().bg(panel).fg(Color::White),
            "中",
        );
        for x in 2..w {
            front.set(x, 0, Cell::blank_styled(Style::default().bg(panel)));
        }
        front.clear_dirty();

        let mut back = Pane::new(w, 1);
        back.put(
            0,
            0,
            Fit::Clip,
            Style::default().bg(sel).fg(Color::White),
            "中",
        );
        for x in 2..w {
            back.set(x, 0, Cell::blank_styled(Style::default().bg(panel)));
        }

        let cmd = back.diff(&front);
        // The wide head must be emitted with the SELECTED bg.
        assert!(cmd.draws.iter().any(|d| matches!(d,
                Draw::Cells { style, cells, .. }
                if style.bg == sel && cells.iter().any(|(s, _)| s == "中"))));
        // No ClearEol may clobber the wide glyph's columns (0..2).
        for d in &cmd.draws {
            if let Draw::ClearEol { x, .. } = d {
                assert!(*x >= 2, "ClearEol at x={x} would clobber the wide glyph");
            }
        }
    }

    /// The blank-tail clear must be found in one backward scan per row, not one
    /// scan per changed cell: a row that changes early and ends in a long blank
    /// tail emits exactly one ClearEol covering the tail.
    #[test]
    fn blank_tail_becomes_a_single_end_of_line_clear() {
        let mut back = grid("", 20, 1);
        let panel = Style::default().bg(Color::Rgb(4, 5, 6));
        back.fill_rect(0, 0, 20, 1, panel);
        back.put(0, 0, Fit::Clip, panel, "x");

        let front = Pane::new(20, 1);
        let cmd = back.diff(&front);
        let clears: Vec<(u16, u16)> = cmd
            .draws
            .iter()
            .filter_map(|d| match d {
                Draw::ClearEol { x, width, .. } => Some((*x, *width)),
                _ => None,
            })
            .collect();
        assert_eq!(clears.len(), 1, "exactly one clear for the blank tail");
        assert_eq!(clears[0], (1, 19));
    }
}
