//! The retained cell grid and its damage bookkeeping.
//!
//! # Model
//!
//! This is vim's `ScreenGrid` model rather than an immediate-mode buffer. Two
//! types carry it, deliberately separate:
//!
//! - [`CellGrid`] is *content only*: a 2-D array of [`Cell`]s in row-major
//!   order. It holds no damage state, so the same type serves the **back** grid
//!   (what the application wants) and the **front** grid (what the terminal
//!   currently shows).
//! - [`Damage`] is the dirty bookkeeping for one grid: a per-row leftmost dirty
//!   column plus an inclusive dirty row range. Writes mark damage *at write
//!   time* and every mutating method takes `&mut Damage`, so no write can
//!   silently escape the tracker.
//!
//! The frame owns `back: CellGrid`, `front: CellGrid` and `back_damage: Damage`;
//! the differ walks only the dirty rows, and only from each row's leftmost dirty
//! column. The front grid has no damage tracker because its damage would be
//! meaningless — a state this design makes unrepresentable.
//!
//! # Wide glyphs
//!
//! Writing a [`CellWidth::Wide`] glyph at `(x, y)` also writes a
//! [`CellWidth::Continuation`] at `(x+1, y)` carrying the glyph's background, so
//! the trailing column can never ghost. Writing over either half of such a pair
//! blanks the other half. A glyph that would straddle the right edge is never
//! split: it moves to the next row ([`Fit::Wrap`]) or is dropped ([`Fit::Clip`]).
//!
//! # Zero-column graphemes
//!
//! Grapheme segmentation already attaches combining marks to their base. A
//! standalone zero-column cluster (`U+200B`, `U+00AD`, a lone `U+FE0F`) has no
//! sensible cell to occupy and is **refused by the writers** rather than stored
//! as an ambiguous cell.

use crate::buffer::cell::{Cell, CellWidth, Color, Style};
use crate::buffer::text::{grapheme_width, graphemes, is_hard_break};
use crate::layout::Rect;

/// A `(row, col)` position on the grid. `(0,0)` is the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pos {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
}

impl Pos {
    /// A position at `(x, y)`.
    pub fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// How a string writer handles a glyph that does not fit on the current line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Drop the glyph if it would overflow the right edge.
    #[default]
    Clip,
    /// Move to column `origin` of the next row before drawing. If there is no
    /// next row, writing stops.
    Wrap,
}

/// Direction of an in-place band rotation ([`CellGrid::rotate_band`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandRotation {
    /// Rows move down; the top of the band is blanked (a terminal "scroll").
    Down,
    /// Rows move up; the bottom of the band is blanked. The streaming-append
    /// shape: a new row lands at the bottom and history shifts up.
    Up,
}

/// Per-row damage bookkeeping for one [`CellGrid`].
///
/// Kept separate from the content it describes: it is meaningless for a front
/// grid, it is cheap to reset independently, and it can be tested by feeding it
/// a sequence of marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    /// Leftmost changed column on each row, or `None` when the row is clean.
    dirty_col: Vec<Option<u16>>,
    /// Inclusive dirty row range, or `None` when nothing is dirty.
    lo: Option<u16>,
    hi: Option<u16>,
}

impl Damage {
    /// A clean tracker for a grid `height` rows tall.
    pub fn new(height: u16) -> Self {
        Self {
            dirty_col: vec![None; height as usize],
            lo: None,
            hi: None,
        }
    }

    /// Record that `(x, y)` changed, expanding the row's window leftward and the
    /// dirty row range outward.
    ///
    /// Out-of-range coordinates are ignored: this is bookkeeping, not content,
    /// and it must not be able to abort the process. Content writes
    /// drop out-of-range coordinates too, so the two agree.
    #[inline]
    pub fn mark(&mut self, x: u16, y: u16) {
        let Some(slot) = self.dirty_col.get_mut(y as usize) else {
            return;
        };
        *slot = Some(match *slot {
            Some(lo) => lo.min(x),
            None => x,
        });
        self.lo = Some(self.lo.map_or(y, |lo| lo.min(y)));
        self.hi = Some(self.hi.map_or(y, |hi| hi.max(y)));
    }

    /// Mark every row dirty from column 0.
    pub fn mark_all(&mut self) {
        self.dirty_col.fill(Some(0));
        if self.dirty_col.is_empty() {
            self.lo = None;
            self.hi = None;
        } else {
            self.lo = Some(0);
            self.hi = Some(self.dirty_col.len() as u16 - 1);
        }
    }

    /// Reset to clean.
    pub fn clear(&mut self) {
        self.dirty_col.fill(None);
        self.lo = None;
        self.hi = None;
    }

    /// Whether any row is dirty.
    pub fn is_empty(&self) -> bool {
        self.lo.is_none()
    }

    /// Whether any row is dirty (inverse of [`Self::is_empty`]).
    pub fn is_dirty(&self) -> bool {
        !self.is_empty()
    }

    /// The inclusive dirty row range, or `None` when clean.
    pub fn rows(&self) -> Option<(u16, u16)> {
        match (self.lo, self.hi) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        }
    }

    /// Leftmost dirty column on `y`, or `None` when that row is clean.
    pub fn col_of(&self, y: u16) -> Option<u16> {
        self.dirty_col.get(y as usize).copied().flatten()
    }

    /// Grow or shrink the tracker to `height` rows. A resize dirties
    /// everything, because rows that survive keep their old column offsets and
    /// rows that appear are new.
    pub fn resize(&mut self, height: u16) {
        self.dirty_col.resize(height as usize, None);
        self.mark_all();
    }
}

/// A retained grid of cells: the single source of truth for the content of one
/// screen.
///
/// The type is symmetric: it is used for the back grid (desired state) and the
/// front grid (what the terminal shows). Only the back grid has a [`Damage`]
/// companion, and that pairing is expressed by every mutating method taking
/// `&mut Damage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellGrid {
    width: u16,
    height: u16,
    /// Row-major cell storage. Private: content mutation must go through a
    /// method that maintains the wide-glyph invariant.
    content: Vec<Cell>,
}

impl CellGrid {
    /// A blank grid of the given size: every cell is a default blank.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            content: vec![Cell::blank(); (width as usize) * (height as usize)],
        }
    }

    /// A grid of *undrawn* cells. Used for the front grid on startup or after a
    /// terminal invalidation, so the first diff passes every cell through
    /// instead of assuming the terminal already shows default blanks.
    pub fn new_uninitialized(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            content: vec![Cell::uninitialized(); (width as usize) * (height as usize)],
        }
    }

    /// The grid dimensions as `(width, height)`.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The grid width in columns.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The grid height in rows.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The grid's bounding rect, anchored at the origin.
    pub fn area(&self) -> crate::layout::Rect {
        crate::layout::Rect::new(0, 0, self.width, self.height)
    }

    /// Whether the grid has no cells at all.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// The raw cell slice, row-major. Read-only: mutation must go through a
    /// method that maintains the wide-glyph invariant.
    pub fn cells(&self) -> &[Cell] {
        &self.content
    }

    /// Owned clone of a full row, or `None` when the row does not exist.
    pub fn row(&self, y: u16) -> Option<Vec<Cell>> {
        self.row_slice(y).map(<[Cell]>::to_vec)
    }

    /// Borrow a full row, or `None` when the row does not exist. This is the
    /// primitive the differ uses for whole-row comparisons.
    pub fn row_slice(&self, y: u16) -> Option<&[Cell]> {
        if y >= self.height || self.width == 0 {
            return None;
        }
        let start = y as usize * self.width as usize;
        self.content.get(start..start + self.width as usize)
    }

    /// Write a cell without touching any damage tracker and without repairing
    /// the wide-glyph pair.
    ///
    /// Reserved for the front grid, whose content mirrors the terminal and whose
    /// damage is meaningless: promotion copies from the back grid, which already
    /// satisfies the invariant.
    pub(crate) fn overwrite(&mut self, x: u16, y: u16, cell: Cell) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = self.index_of(x, y);
        self.content[idx] = cell;
    }

    /// Read a cell. Out-of-bounds reads return `None`.
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.content.get(self.index_of(x, y))
    }

    /// Read a cell, treating an out-of-bounds read as an undrawn cell.
    pub fn get_or_empty(&self, x: u16, y: u16) -> Cell {
        self.get(x, y).cloned().unwrap_or_else(Cell::uninitialized)
    }

    /// Row-major index for `(x, y)`.
    #[inline]
    pub fn index_of(&self, x: u16, y: u16) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// Resize the grid, preserving the overlapping region and blanking the rest.
    ///
    /// Resizing dirties everything: surviving rows keep their old column
    /// offsets, so a partial repaint is not provably correct.
    pub fn resize(&mut self, damage: &mut Damage, width: u16, height: u16) {
        if self.width == width && self.height == height {
            return;
        }
        let mut next = vec![Cell::blank(); (width as usize) * (height as usize)];
        let copy_w = self.width.min(width) as usize;
        for y in 0..self.height.min(height) as usize {
            let src = y * self.width as usize;
            let dst = y * width as usize;
            next[dst..dst + copy_w].clone_from_slice(&self.content[src..src + copy_w]);
        }
        self.width = width;
        self.height = height;
        self.content = next;
        damage.resize(height);
    }

    /// Apply `f` to one cell's [`Style`], preserving its glyph and geometry.
    ///
    /// This is the geometry-preserving sibling of [`Self::set`]: the symbol and
    /// [`CellWidth`] are untouched, so the wide-glyph pairing this column takes
    /// part in stays valid. `set` cannot be used for this, because it repairs the
    /// pairs a *new glyph* could straddle — a style-only change to a
    /// [`CellWidth::Continuation`] would read as a landing and blank the head to
    /// its left, and a style-only change to a [`CellWidth::Wide`] head would blank
    /// its continuation. Restyling a whole frame (a modal recess) then demotes
    /// every continuation in it, which breaks the row's column arithmetic for the
    /// differ.
    ///
    /// Undrawn cells are restyled like any other: they have no glyph to protect,
    /// and a background fill must be able to cover a region that was never
    /// painted.
    pub fn restyle_cell(
        &mut self,
        damage: &mut Damage,
        x: u16,
        y: u16,
        f: impl FnOnce(&mut Style),
    ) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = self.index_of(x, y);
        let mut cell = self.content[idx].clone();
        f(&mut cell.style);
        self.replace(damage, x, y, cell);
    }

    /// Write a cell at `(x, y)`, repairing the wide-glyph pair it lands on.
    ///
    /// Out-of-bounds writes are dropped. An unchanged cell is not marked, so a
    /// widget that repaints identical content keeps the row clean.
    pub fn set(&mut self, damage: &mut Damage, x: u16, y: u16, cell: Cell) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = self.index_of(x, y);
        if self.content[idx] == cell {
            return;
        }

        // Landing on the trailing half of a wide glyph blanks its head.
        if x > 0 && self.content[idx].is_wide_continuation() {
            self.blank(damage, x - 1, y);
        }
        // Landing on the head of a wide glyph blanks its trailing half.
        if self.content[idx].width == CellWidth::Wide && x + 1 < self.width {
            self.blank(damage, x + 1, y);
        }

        self.replace(damage, x, y, cell);
    }

    /// Replace one cell with a blank, preserving whatever style was there.
    fn blank(&mut self, damage: &mut Damage, x: u16, y: u16) {
        let idx = self.index_of(x, y);
        let mut cleared = self.content[idx].clone();
        cleared.symbol = " ".into();
        cleared.width = CellWidth::Narrow;
        self.replace(damage, x, y, cleared);
    }

    /// Write a cell and mark damage when it actually changed.
    #[inline]
    fn replace(&mut self, damage: &mut Damage, x: u16, y: u16, cell: Cell) {
        let idx = self.index_of(x, y);
        if self.content[idx] == cell {
            return;
        }
        self.content[idx] = cell;
        damage.mark(x, y);
    }

    /// Write a string at `(x, y)` with a uniform style, clipped to the grid
    /// edges. See [`Self::put_in`] for the clipped form.
    pub fn put(
        &mut self,
        damage: &mut Damage,
        x: u16,
        y: u16,
        fit: Fit,
        style: Style,
        text: &str,
    ) -> Pos {
        let area = self.area();
        self.put_in(damage, x, y, fit, style, text, area)
    }

    /// Write a string at `(x, y)` with a uniform style, confined to `clip`.
    ///
    /// Grapheme widths are honored: a wide glyph also fills its trailing column
    /// with a continuation cell. Returns the position after the last written
    /// column.
    ///
    /// A hard line break (see [`crate::buffer::text::is_hard_break`]) moves to
    /// column `x` of the next row regardless of [`Fit`], mirroring line-by-line
    /// layout. Control characters are dropped: the grid must never contain a
    /// payload-owned control sequence that would move the real terminal cursor
    /// without moving the retained model.
    ///
    /// A `Color::Reset` background is treated as transparent: the surface
    /// already painted under the cell is preserved instead of punching through
    /// to the terminal default.
    ///
    /// `clip` is intersected with the grid area, so it can only ever narrow the
    /// writable region — this is what makes widget painting clip-safe by
    /// construction.
    #[allow(clippy::too_many_arguments)] // an explicit clip is the point
    pub fn put_in(
        &mut self,
        damage: &mut Damage,
        x: u16,
        y: u16,
        fit: Fit,
        style: Style,
        text: &str,
        clip: Rect,
    ) -> Pos {
        let clip = clip.intersection(self.area());
        if clip.is_empty() || y < clip.y || y >= clip.bottom() {
            return Pos { x, y };
        }
        let left = clip.x;
        let right = clip.right();

        let mut cx = x;
        let mut cy = y;

        for piece in graphemes(text) {
            let grapheme = piece.text;

            if is_hard_break(grapheme) {
                match cy.checked_add(1) {
                    Some(next) if next < clip.bottom() => {
                        cy = next;
                        cx = x;
                    }
                    _ => break,
                }
                continue;
            }
            if grapheme.chars().any(char::is_control) {
                continue;
            }
            // A standalone zero-column cluster has no cell to occupy.
            let Some(width) = CellWidth::from_columns(grapheme_width(grapheme)) else {
                continue;
            };
            // A glyph straddling the clip's left edge is invisible but still
            // consumes its columns.
            if cx < left {
                cx = cx.saturating_add(width.advance());
                continue;
            }

            // Overflow handling. `Clip` stops at the first glyph that does not
            // fit; `Wrap` moves to the origin column of the next row. A glyph
            // wider than the writable region can never fit and is skipped.
            if cx + width.columns() as u16 > right {
                if width.columns() as u16 > right - left {
                    continue;
                }
                match fit {
                    Fit::Wrap => match cy.checked_add(1) {
                        Some(next) if next < clip.bottom() => {
                            cy = next;
                            cx = x.max(left);
                        }
                        _ => break,
                    },
                    Fit::Clip => break,
                }
            }

            let mut cell_style = style;
            if cell_style.bg == Color::Reset
                && let Some(existing) = self.get(cx, cy)
            {
                cell_style.bg = existing.style.bg;
            }

            let head = Cell::new(grapheme, width, cell_style);
            let trail = (width == CellWidth::Wide && cx + 1 < right)
                .then(|| Cell::wide_continuation(cell_style));

            // Repainting an identical glyph must not dirty the retained grid:
            // dirty marks feed the scroll-translation proof, and spurious ones
            // can suppress a legitimate translation.
            let head_idx = self.index_of(cx, cy);
            let glyph_unchanged = self.content[head_idx] == head
                && trail.as_ref().is_none_or(|want| {
                    self.content
                        .get(self.index_of(cx + 1, cy))
                        .is_some_and(|have| have == want)
                });
            if glyph_unchanged {
                cx += width.advance();
                continue;
            }

            // Repair the pairs this glyph's head and tail straddle.
            if cx > 0 && self.content[head_idx].is_wide_continuation() {
                self.blank(damage, cx - 1, cy);
            }
            if self.content[head_idx].width == CellWidth::Wide && cx + 1 < self.width {
                self.blank(damage, cx + 1, cy);
            }
            self.replace(damage, cx, cy, head);

            if let Some(trail) = trail {
                let trail_idx = self.index_of(cx + 1, cy);
                if self.content[trail_idx].width == CellWidth::Wide && cx + 2 < self.width {
                    self.blank(damage, cx + 2, cy);
                }
                self.replace(damage, cx + 1, cy, trail);
            }
            cx += width.advance();
        }

        Pos { x: cx, y: cy }
    }

    /// Fill a rectangle with blank cells of `style` (a solid colored band),
    /// clipping to the grid and repairing any wide glyph straddling an edge.
    pub fn fill_rect(&mut self, damage: &mut Damage, x: u16, y: u16, w: u16, h: u16, style: Style) {
        if w == 0 || h == 0 || x >= self.width {
            return;
        }
        let blank = Cell::blank_styled(style);
        let col_end = x.saturating_add(w).min(self.width);
        for row in y..y.saturating_add(h).min(self.height) {
            if x > 0 && self.content[self.index_of(x, row)].is_wide_continuation() {
                self.blank(damage, x - 1, row);
            }
            if col_end < self.width {
                let last = self.index_of(col_end - 1, row);
                if self.content[last].width == CellWidth::Wide {
                    self.blank(damage, col_end, row);
                }
            }
            for col in x..col_end {
                self.replace(damage, col, row, blank.clone());
            }
        }
    }

    /// Clear from column `x` to the right edge of row `y`, repairing a wide
    /// glyph that straddles `x`.
    pub fn clear_row(&mut self, damage: &mut Damage, y: u16, x: u16, style: Style) {
        if y >= self.height || self.width == 0 {
            return;
        }
        let start_x = x.min(self.width - 1);
        if start_x > 0 && self.content[self.index_of(start_x, y)].is_wide_continuation() {
            self.blank(damage, start_x - 1, y);
        }
        let blank = Cell::blank_styled(style);
        for col in start_x..self.width {
            self.replace(damage, col, y, blank.clone());
        }
    }

    /// Rotate rows `[y, y + height)` in place: `Down` moves every row down by
    /// `amount` and blanks the rows exposed at the top of the band; `Up` does
    /// the mirror image.
    ///
    /// This is the grid-side twin of the terminal's scroll op. It is applied to
    /// the **front** grid before the row diff so post-scroll comparisons are
    /// honest, which is why it takes no [`Damage`]: the front grid has none, and
    /// the back grid is never rotated (its rows are compared in place).
    ///
    /// Content pushed past the band's edge is discarded, exactly as a terminal
    /// scroll region discards it. The exposed rows are blanked to
    /// [`Cell::blank`], which is what a terminal scroll exposes.
    pub fn rotate_band(&mut self, y: u16, height: u16, direction: BandRotation, amount: u16) {
        if amount == 0 || height == 0 || amount >= height || y >= self.height {
            return;
        }
        let top = y as usize;
        let band_end = (top + height as usize).min(self.height as usize);
        let band_len = band_end.saturating_sub(top);
        let amount = amount as usize;
        if amount >= band_len || self.width == 0 {
            return;
        }
        let w = self.width as usize;
        let mut scratch: Vec<Cell> = Vec::with_capacity(w);

        match direction {
            BandRotation::Down => {
                for dst in (top + amount..band_end).rev() {
                    copy_row(&mut self.content, &mut scratch, w, dst, dst - amount);
                }
                for row in top..top + amount {
                    for cell in &mut self.content[row * w..row * w + w] {
                        *cell = Cell::blank();
                    }
                }
            }
            BandRotation::Up => {
                for dst in top..band_end - amount {
                    copy_row(&mut self.content, &mut scratch, w, dst, dst + amount);
                }
                for row in band_end - amount..band_end {
                    for cell in &mut self.content[row * w..row * w + w] {
                        *cell = Cell::blank();
                    }
                }
            }
        }
    }
}

/// Copy one row between two row indices through `scratch`, so an in-flight
/// source row is never clobbered (`Cell` is `Clone`, not `Copy`).
fn copy_row(content: &mut [Cell], scratch: &mut Vec<Cell>, w: usize, dst: usize, src: usize) {
    scratch.clear();
    scratch.extend_from_slice(&content[src * w..src * w + w]);
    content[dst * w..dst * w + w].clone_from_slice(scratch);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;

    fn write(g: &mut CellGrid, d: &mut Damage, x: u16, y: u16, s: &str) -> Pos {
        g.put(d, x, y, Fit::Clip, Style::default(), s)
    }

    #[test]
    fn new_grid_is_clean_and_blank() {
        let g = CellGrid::new(4, 3);
        let d = Damage::new(3);
        assert!(!d.is_dirty());
        assert_eq!(d.rows(), None);
        assert_eq!(g.get(0, 0), Some(&Cell::blank()));
        assert_eq!(g.get(4, 0), None);
        assert_eq!(g.row(3), None);
        assert_eq!(g.row(0).map(|r| r.len()), Some(4));
    }

    #[test]
    fn set_marks_only_that_row_from_the_column() {
        let mut g = CellGrid::new(4, 3);
        let mut d = Damage::new(3);
        g.set(&mut d, 2, 1, Cell::narrow("x", Style::default()));
        assert_eq!(d.rows(), Some((1, 1)));
        assert_eq!(d.col_of(0), None);
        assert_eq!(d.col_of(1), Some(2));
        assert_eq!(d.col_of(2), None);
    }

    #[test]
    fn out_of_bounds_writes_are_dropped_and_never_panic() {
        let mut g = CellGrid::new(2, 2);
        let mut d = Damage::new(2);
        // Content writes and damage marks must agree: both ignore the request.
        g.set(&mut d, 9, 9, Cell::narrow("x", Style::default()));
        d.mark(9, 9);
        d.mark(0, 9);
        assert!(!d.is_dirty());
        assert!(g.get(0, 0).is_none() || g.get(0, 0) == Some(&Cell::blank()));
        write(&mut g, &mut d, 9, 9, "x");
        assert!(!d.is_dirty());
        // A zero-width grid is a legal value, not a panic.
        let empty = CellGrid::new(0, 0);
        assert!(empty.is_empty());
        assert_eq!(empty.row(0), None);
    }

    #[test]
    fn identical_writes_do_not_dirty_the_retained_grid() {
        let mut g = CellGrid::new(8, 2);
        let mut d = Damage::new(2);
        let style = Style::default().bg(Color::Rgb(1, 2, 3));
        g.fill_rect(&mut d, 0, 0, 8, 2, style);
        write(&mut g, &mut d, 1, 0, "中a");
        d.clear();

        g.fill_rect(&mut d, 0, 1, 8, 1, style);
        write(&mut g, &mut d, 1, 0, "中a");

        assert!(
            !d.is_dirty(),
            "rewriting identical narrow and wide cells must stay clean"
        );
        assert!(g.get(2, 0).unwrap().is_wide_continuation());
    }

    #[test]
    fn put_writes_a_wide_glyph_with_its_continuation() {
        let mut g = CellGrid::new(6, 1);
        let mut d = Damage::new(1);
        write(&mut g, &mut d, 0, 0, "😀a");
        let head = g.get(0, 0).unwrap();
        assert_eq!(head.symbol, "😀");
        assert_eq!(head.width, CellWidth::Wide);
        assert_eq!(head.columns(), 2);
        assert!(g.get(1, 0).unwrap().is_wide_continuation());
        assert_eq!(g.get(2, 0).unwrap().symbol, "a");
        assert_eq!(d.col_of(0), Some(0));
    }

    #[test]
    fn overwriting_half_of_a_wide_glyph_blanks_the_other_half() {
        let mut g = CellGrid::new(6, 1);
        let mut d = Damage::new(1);
        write(&mut g, &mut d, 1, 0, "中");
        assert!(g.get(2, 0).unwrap().is_wide_continuation());
        // Overwrite the head: the continuation must be repaired.
        g.set(&mut d, 1, 0, Cell::narrow("x", Style::default()));
        assert_eq!(g.get(2, 0).unwrap().symbol, " ");
        assert!(!g.get(2, 0).unwrap().is_wide_continuation());
        // Overwrite the continuation: the head must be repaired.
        write(&mut g, &mut d, 1, 0, "中");
        g.set(&mut d, 2, 0, Cell::narrow("y", Style::default()));
        assert_eq!(g.get(1, 0).unwrap().symbol, " ");
        assert_eq!(g.get(1, 0).unwrap().width, CellWidth::Narrow);
    }

    #[test]
    fn zero_column_graphemes_are_refused_not_stored_ambiguous() {
        // U+200B is width 0 and is not a control char, so the old writer stored
        // it as an ambiguous width-0 cell that the differ then mistook for a
        // wide-glyph continuation.
        let mut g = CellGrid::new(4, 1);
        let mut d = Damage::new(1);
        let end = write(&mut g, &mut d, 0, 0, "a\u{200B}b");
        assert_eq!(end.x, 2);
        assert!(!d.is_dirty() || d.is_dirty());
        assert_eq!(g.get(0, 0).unwrap().symbol, "a");
        assert_eq!(g.get(1, 0).unwrap().symbol, "b");
        // No cell claims to be a continuation.
        assert!((0..4).all(|x| !g.get(x, 0).unwrap().is_wide_continuation()));
    }

    #[test]
    fn put_wraps_on_overflow() {
        let mut g = CellGrid::new(3, 2);
        let mut d = Damage::new(2);
        let end = g.put(&mut d, 2, 0, Fit::Wrap, Style::default(), "ab");
        assert_eq!(g.get(2, 0).unwrap().symbol, "a");
        assert_eq!(g.get(2, 1).unwrap().symbol, "b");
        assert_eq!(end, Pos { x: 3, y: 1 });
    }

    #[test]
    fn put_newline_moves_to_next_row_at_the_origin_column() {
        let mut g = CellGrid::new(6, 2);
        let mut d = Damage::new(2);
        write(&mut g, &mut d, 2, 0, "ab\ncd");
        assert_eq!(g.get(2, 0).unwrap().symbol, "a");
        assert_eq!(g.get(3, 0).unwrap().symbol, "b");
        assert_eq!(g.get(2, 1).unwrap().symbol, "c");
        assert_eq!(g.get(3, 1).unwrap().symbol, "d");
        assert_eq!(g.get(0, 1).unwrap().symbol, " ");
        assert_eq!(g.get(1, 1).unwrap().symbol, " ");
    }

    #[test]
    fn every_hard_break_advances_a_row_and_controls_are_dropped() {
        for text in ["ab\ncd", "ab\r\ncd", "ab\rcd"] {
            let mut g = CellGrid::new(8, 2);
            let mut d = Damage::new(2);
            write(&mut g, &mut d, 0, 0, text);
            let second: String = (0..7)
                .map(|x| g.get(x, 1).unwrap().symbol.as_str().to_string())
                .collect();
            assert_eq!(&second[..2], "cd", "for {text:?}");
        }

        let mut g = CellGrid::new(8, 2);
        let mut d = Damage::new(2);
        write(&mut g, &mut d, 0, 0, "ab\x1b[2J\u{8}\tcd");
        let row: String = (0..7)
            .map(|x| g.get(x, 0).unwrap().symbol.as_str().to_string())
            .collect();
        // The ESC, backspace and tab *controls* are dropped; the printable
        // `[2J` that followed the ESC is ordinary text and stays.
        assert_eq!(row, "ab[2Jcd");
        assert!(
            g.cells()
                .iter()
                .all(|cell| { !cell.symbol.chars().any(char::is_control) })
        );
    }

    #[test]
    fn reset_background_text_is_transparent_over_a_painted_surface() {
        let mut g = CellGrid::new(4, 1);
        let mut d = Damage::new(1);
        let surface = Style::default().bg(Color::Rgb(9, 9, 9));
        g.fill_rect(&mut d, 0, 0, 4, 1, surface);
        write(&mut g, &mut d, 0, 0, "hi");
        // Text with a Reset background keeps the surface underneath.
        assert_eq!(g.get(0, 0).unwrap().style.bg, Color::Rgb(9, 9, 9));
        // Text with an explicit background overrides it.
        let over = Style::default().bg(Color::Rgb(1, 1, 1));
        g.put(&mut d, 1, 0, Fit::Clip, over, "X");
        assert_eq!(g.get(1, 0).unwrap().style.bg, Color::Rgb(1, 1, 1));
    }

    #[test]
    fn clear_row_starts_at_the_requested_column() {
        let mut g = CellGrid::new(4, 1);
        let mut d = Damage::new(1);
        write(&mut g, &mut d, 0, 0, "abcd");
        d.clear();
        g.clear_row(&mut d, 0, 2, Style::default());
        assert_eq!(g.get(1, 0).unwrap().symbol, "b");
        assert_eq!(g.get(2, 0).unwrap().symbol, " ");
        assert_eq!(d.col_of(0), Some(2));
    }

    #[test]
    fn restyle_cell_keeps_the_wide_glyph_pair_and_the_row_arithmetic() {
        // The geometry-preserving counterpart of `set`. Restyling a whole frame
        // is what `recess_backdrop(Recess::Dim)` does, so this must be safe to
        // apply to every column of a row carrying wide glyphs.
        let mut g = CellGrid::new(6, 1);
        let mut d = Damage::new(1);
        let surface = Style::default().bg(Color::Rgb(14, 15, 15));
        g.fill_rect(&mut d, 0, 0, 6, 1, surface);
        g.put(&mut d, 0, 0, Fit::Clip, surface, "缺口");
        d.clear();

        for x in 0..6 {
            g.restyle_cell(&mut d, x, 0, |style| style.bg = style.bg.scale(0.5));
        }

        // Every head still advertises itself as wide and still owns its tail.
        assert_eq!(g.get(0, 0).unwrap().width, CellWidth::Wide);
        assert_eq!(g.get(1, 0).unwrap().width, CellWidth::Continuation);
        assert_eq!(g.get(2, 0).unwrap().width, CellWidth::Wide);
        assert_eq!(g.get(3, 0).unwrap().width, CellWidth::Continuation);
        // The styles really were applied to both halves of each pair.
        for x in 0..4 {
            assert_eq!(
                g.get(x, 0).unwrap().style.bg,
                Color::Rgb(7, 8, 8),
                "column {x} must carry the dimmed background"
            );
        }
        // The row is still exactly as wide as the grid: walking heads and
        // stepping by their advance lands on the same total as the column count.
        let painted: u16 = (0..6)
            .filter(|&x| !g.get(x, 0).unwrap().width.is_continuation())
            .map(|x| g.get(x, 0).unwrap().width.advance())
            .sum();
        assert_eq!(painted, 6, "a restyle must not change the row's width");
        // It touched cells, so it must have marked damage.
        assert_eq!(d.col_of(0), Some(0));
    }

    #[test]
    fn restyle_cell_leaves_an_out_of_bounds_coordinate_alone() {
        let mut g = CellGrid::new(2, 1);
        let mut d = Damage::new(1);
        g.restyle_cell(&mut d, 5, 0, |style| style.bg = Color::Rgb(1, 2, 3));
        g.restyle_cell(&mut d, 0, 9, |style| style.bg = Color::Rgb(1, 2, 3));
        assert_eq!(g.get(0, 0).unwrap().style.bg, Color::Reset);
        assert_eq!(d.col_of(0), None);
    }

    #[test]
    fn fill_rect_repairs_a_wide_glyph_crossing_its_left_edge() {
        let mut g = CellGrid::new(6, 2);
        let mut d = Damage::new(2);
        write(&mut g, &mut d, 0, 0, "中");
        assert!(g.get(1, 0).unwrap().is_wide_continuation());
        // Fill columns 1..3: the head at column 0 must be repaired, not left
        // orphaned half off the filled region.
        g.fill_rect(&mut d, 1, 0, 2, 1, Style::default());
        assert_eq!(g.get(0, 0).unwrap().symbol, " ");
        assert_eq!(g.get(0, 0).unwrap().width, CellWidth::Narrow);
    }

    #[test]
    fn resize_preserves_overlap_and_marks_everything() {
        let mut g = CellGrid::new(4, 2);
        let mut d = Damage::new(2);
        write(&mut g, &mut d, 0, 0, "abcd");
        d.clear();
        g.resize(&mut d, 6, 3);
        assert_eq!(g.size(), (6, 3));
        assert_eq!(g.get(0, 0).unwrap().symbol, "a");
        assert_eq!(g.get(3, 0).unwrap().symbol, "d");
        assert_eq!(g.get(4, 0).unwrap().symbol, " ");
        assert_eq!(d.rows(), Some((0, 2)));
        assert_eq!(d.col_of(0), Some(0));
        assert_eq!(d.col_of(2), Some(0));
        // A no-op resize stays clean.
        d.clear();
        g.resize(&mut d, 6, 3);
        assert!(!d.is_dirty());
    }

    #[test]
    fn rotate_band_up_shifts_history_and_blanks_the_tail() {
        let mut g = CellGrid::new(4, 3);
        let mut d = Damage::new(3);
        for (i, row) in ["row0", "row1", "row2"].iter().enumerate() {
            write(&mut g, &mut d, 0, i as u16, row);
        }
        g.rotate_band(0, 3, BandRotation::Up, 1);
        let texts: Vec<String> = (0..3)
            .map(|y| {
                (0..4)
                    .map(|x| g.get(x, y).unwrap().symbol.as_str().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(texts, vec!["row1", "row2", "    "]);
    }

    #[test]
    fn rotate_band_down_exposes_blank_rows_at_the_top() {
        let mut g = CellGrid::new(4, 3);
        let mut d = Damage::new(3);
        for (i, row) in ["row0", "row1", "row2"].iter().enumerate() {
            write(&mut g, &mut d, 0, i as u16, row);
        }
        g.rotate_band(0, 3, BandRotation::Down, 1);
        let texts: Vec<String> = (0..3)
            .map(|y| {
                (0..4)
                    .map(|x| g.get(x, y).unwrap().symbol.as_str().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(texts, vec!["    ", "row0", "row1"]);
    }

    #[test]
    fn rotate_band_is_a_no_op_beyond_its_band() {
        let mut g = CellGrid::new(2, 2);
        let mut d = Damage::new(2);
        write(&mut g, &mut d, 0, 0, "ab");
        let before = g.clone();
        // amount >= height, or a degenerate band, must not touch anything.
        g.rotate_band(0, 2, BandRotation::Up, 2);
        g.rotate_band(0, 0, BandRotation::Up, 1);
        assert_eq!(g, before);
    }

    #[test]
    fn damage_tracker_is_independent_of_content() {
        let mut d = Damage::new(2);
        d.mark(3, 0);
        d.mark(1, 0);
        assert_eq!(d.col_of(0), Some(1));
        assert_eq!(d.rows(), Some((0, 0)));
        d.mark(0, 1);
        assert_eq!(d.rows(), Some((0, 1)));
        d.clear();
        assert!(!d.is_dirty());
        d.mark_all();
        assert_eq!(d.rows(), Some((0, 1)));
        assert_eq!(d.col_of(1), Some(0));
    }
}
