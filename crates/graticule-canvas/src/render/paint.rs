//! The paint context and the [`Widget`] contract.
//!
//! Everything that draws into the retained grid draws through a [`Paint`], and a
//! [`Paint`] carries a clip rectangle that every write is confined to. This makes
//! clipping *structural* rather than a convention each widget has to remember:
//!
//! - [`Paint::fit`] can only ever narrow the clip, so a child can never paint
//!   outside its parent's allocation even if it tries.
//! - A widget cannot obtain an unclipped handle to the grid, so "paint leaked
//!   outside my box" is not a reachable state.
//!
//! The layer sits *below* `terminal`: `widgets -> render::paint -> buffer`.
//! Drawing a paragraph into a buffer has nothing to do with owning a terminal,
//! and before this split every widget imported `terminal::frame` for the trait
//! definition.

use crate::buffer::cell::{Cell, Style};
use crate::buffer::grid::{CellGrid, Damage, Fit, Pos};
use crate::layout::Rect;

/// A clipped, damage-tracking writing surface.
pub struct Paint<'g> {
    grid: &'g mut CellGrid,
    damage: &'g mut Damage,
    clip: Rect,
}

impl<'g> Paint<'g> {
    /// Wrap a grid and its damage tracker, confined to `clip`.
    ///
    /// The clip is intersected with the grid area here, so a caller cannot widen
    /// it by passing a larger rectangle.
    pub fn new(grid: &'g mut CellGrid, damage: &'g mut Damage, clip: Rect) -> Self {
        let clip = clip.intersection(grid.area());
        Self { grid, damage, clip }
    }

    /// The writable region.
    pub fn clip(&self) -> Rect {
        self.clip
    }

    /// The whole grid's area, regardless of the current clip.
    pub fn grid_area(&self) -> Rect {
        self.grid.area()
    }

    /// The grid dimensions as `(width, height)`.
    pub fn grid_size(&self) -> (u16, u16) {
        self.grid.size()
    }

    /// Read a cell from the underlying grid. Reads are not clipped, because
    /// "what is already painted there" is what transparent-background writes
    /// need to know.
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        self.grid.get(x, y)
    }

    /// Narrow the clip to `area` and yield a child paint context.
    ///
    /// Returns `None` when the intersection is empty, which lets callers skip
    /// work for a fully clipped-out child.
    pub fn fit<'a>(&'a mut self, area: Rect) -> Option<Paint<'a>> {
        let clip = area.intersection(self.clip);
        if clip.is_empty() {
            return None;
        }
        Some(Paint {
            grid: self.grid,
            damage: self.damage,
            clip,
        })
    }

    /// Whether `(x, y)` is inside the writable region.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.clip.contains(x, y)
    }

    /// Write one cell. Out-of-clip and out-of-grid writes are dropped.
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if !self.clip.contains(x, y) {
            return;
        }
        self.grid.set(self.damage, x, y, cell);
    }

    /// Write a string with a uniform style, wrapping or clipping at the clip's
    /// right edge. Returns the position after the last written column.
    pub fn put(&mut self, x: u16, y: u16, fit: Fit, style: Style, text: &str) -> Pos {
        self.grid
            .put_in(self.damage, x, y, fit, style, text, self.clip)
    }

    /// Fill `area` with blank cells of `style`, clipped.
    pub fn fill(&mut self, area: Rect, style: Style) {
        let area = area.intersection(self.clip);
        if area.is_empty() {
            return;
        }
        self.grid
            .fill_rect(self.damage, area.x, area.y, area.width, area.height, style);
    }

    /// Clear `area` to blank cells of `style` (an alias for [`Self::fill`] with
    /// the intent made explicit at the call site).
    pub fn clear(&mut self, area: Rect, style: Style) {
        self.fill(area, style);
    }

    /// Change one cell's style in place, keeping its symbol and width.
    ///
    /// This is the sanctioned read-modify-write: it cannot break the wide-glyph
    /// invariant because it never touches geometry, and it marks damage like any
    /// other write. Use it for dimming, selection highlights and progress
    /// shading, where the glyph is already correct.
    ///
    /// # Why this cannot go through `CellGrid::set`
    ///
    /// `set` exists to place a *glyph*, so it repairs the wide-glyph pairs a new
    /// glyph could straddle: landing on a continuation blanks the head to its
    /// left, and landing on a head blanks the continuation to its right. That is
    /// correct for a write that changes what a column shows, and wrong for one
    /// that only changes how it looks — `set` compares the whole cell, so a
    /// style-only change to a continuation reads as "a new cell landed here" and
    /// blanks its head, while a style-only change to a head blanks its
    /// continuation. Restyling every cell of a frame (which is exactly what
    /// `recess_backdrop(Recess::Dim)` does) then demotes every continuation in
    /// the frame to a narrow blank.
    ///
    /// That is not a cosmetic loss. A row is walked by advancing each head by
    /// [`crate::buffer::CellWidth::advance`], so a head whose continuation was demoted still
    /// advances two columns while the trailing column is now separately
    /// visitable: the row's painted width and its column count stop agreeing,
    /// the differ skips columns it believes the head already covers, and the
    /// terminal keeps showing the previous frame's pixels there. The retained
    /// grid and the screen are then permanently out of step.
    ///
    /// So the style is applied through [`CellGrid::restyle_cell`], the
    /// geometry-preserving entry point, rather than by handing `set` a whole
    /// cell.
    pub fn restyle(&mut self, x: u16, y: u16, f: impl FnOnce(&mut Style)) {
        if !self.clip.contains(x, y) {
            return;
        }
        self.grid.restyle_cell(self.damage, x, y, f);
    }
}

/// Something that can draw itself into a rectangular allocation.
///
/// The trait is object safe: `Box<dyn Widget>` is the type-erased form, and a
/// borrowed widget can be erased too because no implementation requires
/// `'static`. `render` takes `&mut self` so a widget may keep local state (a
/// scroll offset, a cached layout) without interior mutability.
pub trait Widget {
    /// Draw into `area`, confined to `paint`'s clip.
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>);

    /// How many rows this widget wants on the main axis when its parent can
    /// measure it, given the `cross` (width) it will receive in columns.
    ///
    /// `0` means "no preference": the parent gives it a share of the remaining
    /// space. This is how a `Spacer` and a `Paragraph` stop being treated
    /// identically by `Column`/`Row`.
    fn measure(&self, cross: u16) -> u16 {
        let _ = cross;
        0
    }

    /// Whether this widget absorbs leftover main-axis space.
    ///
    /// `measure` answers "how much do I need"; `grow` answers "may I take more".
    /// A `Paragraph` needs its content and may not grow; a
    /// [`crate::widgets::Spacer`] needs nothing and exists to absorb the
    /// remainder. Without this distinction a container has to guess, which is
    /// what made a spacer and a paragraph interchangeable.
    fn grow(&self) -> u16 {
        0
    }
}

impl<W: Widget + ?Sized> Widget for &mut W {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        (**self).render(area, paint);
    }
    fn measure(&self, cross: u16) -> u16 {
        (**self).measure(cross)
    }
    fn grow(&self) -> u16 {
        (**self).grow()
    }
}

impl<W: Widget + ?Sized> Widget for Box<W> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        (**self).render(area, paint);
    }
    fn measure(&self, cross: u16) -> u16 {
        (**self).measure(cross)
    }
    fn grow(&self) -> u16 {
        (**self).grow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Color, Style};

    fn pane(w: u16, h: u16) -> (CellGrid, Damage) {
        (CellGrid::new(w, h), Damage::new(h))
    }

    /// A deliberately misbehaving widget: it paints at the origin regardless of
    /// where it was given space. The clip must make the out-of-area write a
    /// no-op while its legitimate write lands.
    struct Painted;
    impl Widget for Painted {
        fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
            paint.put(0, 0, Fit::Clip, Style::RESET, "leaked");
            paint.set(area.x, area.y, Cell::narrow("X", Style::RESET));
        }
        fn measure(&self, _cross: u16) -> u16 {
            1
        }
    }

    #[test]
    fn writes_outside_the_clip_are_dropped() {
        let (mut grid, mut damage) = pane(10, 4);
        let area = Rect::new(4, 2, 3, 2);
        {
            let mut paint = Paint::new(&mut grid, &mut damage, area);
            let mut child = paint.fit(area).unwrap();
            Painted.render(area, &mut child);
        }
        // The off-area write never happened...
        assert_eq!(grid.get(0, 0).unwrap().symbol, " ");
        assert!(grid.get(1, 0).unwrap().symbol != "e");
        // ...and the in-area write did.
        assert_eq!(grid.get(4, 2).unwrap().symbol, "X");
        assert_eq!(damage.col_of(2), Some(4));
    }

    #[test]
    fn a_parent_clip_cannot_be_widened_by_a_child() {
        let (mut grid, mut damage) = pane(20, 5);
        let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(2, 1, 4, 3));
        let child = paint.fit(Rect::new(0, 0, 20, 5)).unwrap();
        assert_eq!(child.clip(), Rect::new(2, 1, 4, 3));
    }

    #[test]
    fn a_fully_clipped_out_child_is_not_constructed() {
        let (mut grid, mut damage) = pane(5, 5);
        let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 2, 2));
        assert!(paint.fit(Rect::new(3, 3, 2, 2)).is_none());
    }

    #[test]
    fn clone_is_intersected_with_the_grid_area() {
        let (mut grid, mut damage) = pane(4, 2);
        let paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 100, 100));
        assert_eq!(paint.clip(), Rect::new(0, 0, 4, 2));
    }

    #[test]
    fn put_stops_at_the_clip_edge_and_mark_matches_writes() {
        let (mut grid, mut damage) = pane(12, 2);
        let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(2, 0, 4, 2));
        let end = paint.put(2, 0, Fit::Clip, Style::RESET, "abcdefgh");
        // Clipped at column 6 (2 + 4).
        assert_eq!(end.x, 6);
        assert_eq!(grid.get(5, 0).unwrap().symbol, "d");
        assert_eq!(grid.get(6, 0).unwrap().symbol, " ");
        // Nothing before the clip was touched.
        assert_eq!(grid.get(1, 0).unwrap().symbol, " ");
        // Damage reflects what was written.
        assert_eq!(damage.col_of(0), Some(2));
    }

    #[test]
    fn restyle_changes_style_only_and_marks_damage() {
        let (mut grid, mut damage) = pane(6, 1);
        {
            let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 6, 1));
            paint.put(0, 0, Fit::Clip, Style::RESET, "中a");
        }
        damage.clear();
        let wide = Color::Rgb(9, 9, 9);
        {
            let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 6, 1));
            paint.restyle(0, 0, |s| s.bg = wide);
        }
        let cell = grid.get(0, 0).unwrap();
        assert_eq!(cell.symbol, "中");
        assert_eq!(cell.width, crate::buffer::CellWidth::Wide);
        assert_eq!(cell.style.bg, wide);
        assert_eq!(damage.col_of(0), Some(0));
    }

    #[test]
    fn restyle_preserves_the_wide_glyph_pair_invariant() {
        // a `Wide` head's right neighbour is always a
        // `Continuation`, and nothing else may occupy that column. `restyle` is
        // the sanctioned read-modify-write and its contract is "style only, the
        // glyph is already correct" — so it must not disturb the pair.
        //
        // This is the modal-recess path: `recess_backdrop(Recess::Dim)` restyles
        // *every* cell of the frame, so a row of CJK prose goes through this
        // function once per column. When `restyle` demoted continuations to
        // `Narrow`, the differ then walked such a row in single columns for the
        // trailing half of each glyph while the head still advanced two: the
        // row's painted width stopped matching its column count, the trailing
        // columns were never emitted, and the terminal kept showing whatever it
        // had there. On a dimmed backdrop (the cell carries `Reset` background)
        // that stale content is the terminal's own default background, which is
        // the grey block pattern in the report.
        let (mut grid, mut damage) = pane(6, 1);
        {
            let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 6, 1));
            paint.put(
                0,
                0,
                Fit::Clip,
                Style::default().bg(Color::Rgb(14, 15, 15)),
                "缺口",
            );
        }
        {
            let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 6, 1));
            for x in 0..6 {
                paint.restyle(x, 0, |s| s.bg = s.bg.scale(0.5));
            }
        }
        for (head, tail) in [(0u16, 1u16), (2, 3)] {
            let h = grid.get(head, 0).unwrap();
            assert_eq!(
                h.width,
                crate::buffer::CellWidth::Wide,
                "restyle must not change the head glyph's width at x={head}"
            );
            assert_eq!(
                grid.get(tail, 0).unwrap().width,
                crate::buffer::CellWidth::Continuation,
                "restyle must keep the wide head at x={head} paired with a Continuation at x={tail}"
            );
        }
    }

    #[test]
    fn transient_widgets_are_object_safe_and_boxable() {
        let (mut grid, mut damage) = pane(6, 2);
        let mut boxed: Box<dyn Widget> = Box::new(Painted);
        assert_eq!(boxed.measure(10), 1);
        let mut paint = Paint::new(&mut grid, &mut damage, Rect::new(0, 0, 6, 2));
        boxed.render(Rect::new(0, 0, 6, 2), &mut paint);
        assert_eq!(grid.get(0, 0).unwrap().symbol, "X");
    }
}
