//! The `Block` widget: a bordered, background-filled rectangular panel, with an
//! optional title on its top border.

use crate::buffer::cell::Cell;
use crate::buffer::glyph::GlyphSet;
use crate::buffer::grid::Fit;
use crate::buffer::{Color, Style};
use crate::layout::Rect;
use crate::render::paint::{Paint, Widget};

use super::paragraph::{Alignment, Line};

bitflags::bitflags! {
    /// Which sides of a [`Block`] are drawn.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Borders: u16 {
        /// Left vertical border.
        const LEFT   = 1;
        /// Right vertical border.
        const RIGHT  = 2;
        /// Top horizontal border.
        const TOP    = 4;
        /// Bottom horizontal border.
        const BOTTOM = 8;
        /// No borders.
        const NONE = 0;
        /// Every side.
        const ALL = Self::LEFT.bits() | Self::RIGHT.bits() | Self::TOP.bits() | Self::BOTTOM.bits();
    }
}

/// Which glyph family a border is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderType {
    /// Rounded single-line box drawing (`╭─╮`).
    #[default]
    Plain,
    /// Heavy box drawing (`┏━┓`).
    Thick,
    /// 7-bit ASCII (`+-+`), for terminals without box drawing.
    Ascii,
}

impl BorderType {
    /// The six glyphs this border type draws with, as
    /// `(vertical, horizontal, top-left, top-right, bottom-left, bottom-right)`.
    pub const fn glyphs(
        self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    ) {
        match self {
            BorderType::Plain => ("│", "─", "╭", "╮", "╰", "╯"),
            BorderType::Thick => ("┃", "━", "┏", "┓", "┗", "┛"),
            BorderType::Ascii => ("|", "-", "+", "+", "+", "+"),
        }
    }
}

/// A bordered panel with an optional background and title.
#[derive(Debug, Clone, Default)]
pub struct Block<'a> {
    /// Background fill style for the whole block area.
    pub style: Style,
    /// Which sides are drawn.
    pub borders: Borders,
    /// Glyph family for the border.
    pub border_type: BorderType,
    /// Style for the border glyphs. A `Reset` foreground falls back to
    /// [`Block::style`]'s foreground; the background always follows
    /// [`Block::style`] so the border sits on the panel surface.
    pub border_style: Style,
    /// Text drawn on the top border.
    pub title: Option<Line<'a>>,
    /// Where the title sits on the top border.
    pub title_alignment: Alignment,
    /// Override the vertical bar glyph (for a custom rule character).
    pub glyph_v: Option<&'static str>,
    /// Explicit glyph set, which overrides [`Block::border_type`].
    pub glyph_set: Option<&'static GlyphSet>,
}

impl<'a> Block<'a> {
    /// Set the background fill style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    /// Choose which sides are drawn.
    pub fn borders(mut self, b: Borders) -> Self {
        self.borders = b;
        self
    }
    /// Choose the border glyph family.
    pub fn border_type(mut self, t: BorderType) -> Self {
        self.border_type = t;
        self
    }
    /// Style the border glyphs independently of the background.
    pub fn border_style(mut self, s: Style) -> Self {
        self.border_style = s;
        self
    }
    /// Draw a title on the top border.
    pub fn title(mut self, title: impl Into<Line<'a>>) -> Self {
        self.title = Some(title.into());
        self
    }
    /// Place the title left, centered or right on the top border.
    pub fn title_alignment(mut self, a: Alignment) -> Self {
        self.title_alignment = a;
        self
    }
    /// Override the vertical bar glyph.
    pub fn glyph_v(mut self, sym: &'static str) -> Self {
        self.glyph_v = Some(sym);
        self
    }
    /// Use an explicit glyph set, overriding [`Block::border_type`].
    pub fn glyph_set(mut self, set: &'static GlyphSet) -> Self {
        self.glyph_set = Some(set);
        self
    }

    /// The content rect inside the configured borders.
    ///
    /// Returns a zero-sized rect at the top-left of `area` when the area is too
    /// small to contain its own borders, so a caller that renders into it
    /// produces nothing rather than spilling over the border.
    pub fn inner(&self, area: Rect) -> Rect {
        let left = u16::from(self.borders.contains(Borders::LEFT));
        let right = u16::from(self.borders.contains(Borders::RIGHT));
        let top = u16::from(self.borders.contains(Borders::TOP));
        let bottom = u16::from(self.borders.contains(Borders::BOTTOM));
        Rect {
            x: area.x.saturating_add(left),
            y: area.y.saturating_add(top),
            width: area.width.saturating_sub(left + right),
            height: area.height.saturating_sub(top + bottom),
        }
    }

    /// The six border glyphs in use.
    fn glyphs(
        &self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    ) {
        match self.glyph_set {
            Some(gs) => (
                gs.border_v,
                gs.border_h,
                gs.corner_tl,
                gs.corner_tr,
                gs.corner_bl,
                gs.corner_br,
            ),
            None => self.border_type.glyphs(),
        }
    }

    /// The style the border glyphs are drawn with.
    fn bar_style(&self) -> Style {
        Style {
            fg: if self.border_style.fg != Color::Reset {
                self.border_style.fg
            } else {
                self.style.fg
            },
            // The border sits *on* the panel surface, so the panel background
            // always wins over the border style's.
            bg: self.style.bg,
            add: self.style.add | self.border_style.add,
        }
    }
}

impl Block<'_> {
    /// Draw the block.
    ///
    /// Takes `&self` so a container widget (a `Paragraph` with a surrounding
    /// block) can draw it without cloning or a mutable borrow.
    pub fn draw(&self, area: Rect, paint: &mut Paint<'_>) {
        if area.is_empty() {
            return;
        }
        let Some(mut paint) = paint.fit(area) else {
            return;
        };
        // Background first, so borders and title sit on it.
        paint.fill(area, self.style);

        let (v, h, tl, tr, bl, br) = self.glyphs();
        let sym_v = self.glyph_v.unwrap_or(v);
        let bar = self.bar_style();
        let left = area.x;
        let right = area.right().saturating_sub(1);
        let top = area.y;
        let bottom = area.bottom().saturating_sub(1);
        // A border needs a second row/column to be a border rather than a
        // single cell of line.
        let has_right = self.borders.contains(Borders::RIGHT) && area.width > 1;
        let has_bottom = self.borders.contains(Borders::BOTTOM) && area.height > 1;

        if self.borders.contains(Borders::TOP) {
            for x in area.x..area.right() {
                paint.set(x, top, Cell::narrow(h, bar));
            }
        }
        if has_bottom {
            for x in area.x..area.right() {
                paint.set(x, bottom, Cell::narrow(h, bar));
            }
        }
        if self.borders.contains(Borders::LEFT) {
            for y in area.y..area.bottom() {
                paint.set(left, y, Cell::narrow(sym_v, bar));
            }
        }
        if has_right {
            for y in area.y..area.bottom() {
                paint.set(right, y, Cell::narrow(sym_v, bar));
            }
        }

        // Corners, wherever two sides meet.
        if self.borders.contains(Borders::TOP) && self.borders.contains(Borders::LEFT) {
            paint.set(left, top, Cell::narrow(tl, bar));
        }
        if self.borders.contains(Borders::TOP) && has_right {
            paint.set(right, top, Cell::narrow(tr, bar));
        }
        if has_bottom && self.borders.contains(Borders::LEFT) {
            paint.set(left, bottom, Cell::narrow(bl, bar));
        }
        if has_bottom && has_right {
            paint.set(right, bottom, Cell::narrow(br, bar));
        }

        self.render_title(area, &mut paint);
    }
}

impl Widget for Block<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        self.draw(area, paint);
    }

    fn measure(&self, _cross: u16) -> u16 {
        // Chrome only: everything inside is the caller's business.
        match (
            self.borders.contains(Borders::TOP),
            self.borders.contains(Borders::BOTTOM),
        ) {
            (true, true) => 2,
            (true, false) | (false, true) => 1,
            (false, false) => 0,
        }
    }
}

impl Block<'_> {
    /// Draw the title onto the top border row.
    ///
    /// The title overwrites the border glyphs it covers — it does not splice
    /// them — so an over-long title is clipped by the border's own width and can
    /// never shorten the panel.
    fn render_title(&self, area: Rect, paint: &mut Paint<'_>) {
        let Some(title) = self.title.as_ref() else {
            return;
        };
        if !self.borders.contains(Borders::TOP) || area.height == 0 {
            return;
        }
        let (_, _, tl, ..) = self.glyphs();
        let _ = tl;
        let bar = self.bar_style();
        let width: usize = title
            .spans
            .iter()
            .map(|s| crate::buffer::text::str_len(&s.content))
            .sum();
        // One column of padding on each side of the text, inside the corners.
        let inner_left = area.x.saturating_add(1);
        let inner_width = area.width.saturating_sub(2) as usize;
        if inner_width == 0 {
            return;
        }
        let avail = inner_width.saturating_sub(2).max(1);
        let start_offset = match self.title_alignment {
            Alignment::Left => 0,
            Alignment::Center => inner_width.saturating_sub(width) / 2,
            Alignment::Right => inner_width.saturating_sub(width + 1),
        };
        let mut x = inner_left.saturating_add(start_offset as u16);
        let right = area.right().saturating_sub(1);
        let mut drawn = 0usize;
        for span in &title.spans {
            let style = Style {
                fg: if span.style.fg != Color::Reset {
                    span.style.fg
                } else {
                    bar.fg
                },
                bg: self.style.bg,
                add: bar.add | span.style.add,
            };
            let mut remaining = span.content.as_ref();
            while !remaining.is_empty() && drawn < avail {
                let piece = crate::buffer::text::graphemes(remaining).next();
                let Some(piece) = piece else { break };
                if x >= right {
                    return;
                }
                paint.put(x, area.y, Fit::Clip, style, piece.text);
                x = x.saturating_add(piece.width.max(1) as u16);
                drawn += piece.width.max(1) as usize;
                remaining = &remaining[piece.text.len()..];
            }
            if drawn >= avail {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::CellGrid;
    use crate::buffer::Damage;

    fn render(block: &mut Block<'_>, w: u16, h: u16) -> CellGrid {
        let mut grid = CellGrid::new(w, h);
        let mut damage = Damage::new(h);
        let area = grid.area();
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        let clip = paint.clip();
        block.render(clip, &mut paint);
        grid
    }

    fn row_text(grid: &CellGrid, y: u16) -> String {
        (0..grid.width())
            .map(|x| grid.get(x, y).unwrap().symbol.as_str().to_string())
            .collect()
    }

    #[test]
    fn borders_are_drawn_on_every_side_including_corners() {
        let mut block = Block::default().borders(Borders::ALL);
        let grid = render(&mut block, 6, 3);
        assert_eq!(row_text(&grid, 0), "╭────╮");
        assert_eq!(row_text(&grid, 1), "│    │");
        assert_eq!(row_text(&grid, 2), "╰────╯");
    }

    #[test]
    fn a_single_row_or_column_is_not_overdrawn() {
        // A right border with no second column must not eat the only column.
        let mut block = Block::default().borders(Borders::ALL);
        let grid = render(&mut block, 1, 1);
        assert_eq!(row_text(&grid, 0), "╭");
    }

    #[test]
    fn ascii_view_switch_uses_7bit_glyphs() {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii);
        let grid = render(&mut block, 4, 3);
        assert_eq!(row_text(&grid, 0), "+--+");
        assert_eq!(row_text(&grid, 1), "|  |");
    }

    #[test]
    fn title_is_drawn_on_the_top_border_and_clipped_to_it() {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(Line::raw("hi"));
        let grid = render(&mut block, 8, 3);
        assert_eq!(row_text(&grid, 0), "╭hi────╮");

        // Centered and right-aligned placement.
        let mut centered = Block::default()
            .borders(Borders::ALL)
            .title(Line::raw("hi"))
            .title_alignment(Alignment::Center);
        assert_eq!(row_text(&render(&mut centered, 8, 3), 0), "╭──hi──╮");
        let mut right = Block::default()
            .borders(Borders::ALL)
            .title(Line::raw("hi"))
            .title_alignment(Alignment::Right);
        assert_eq!(row_text(&render(&mut right, 8, 3), 0), "╭───hi─╮");

        // An over-long title stops at the right corner instead of moving it.
        let mut long = Block::default()
            .borders(Borders::ALL)
            .title(Line::raw("a very long title"));
        let grid = render(&mut long, 8, 3);
        assert_eq!(row_text(&grid, 0).chars().count(), 8);
        assert!(row_text(&grid, 0).ends_with('╮'));
    }

    #[test]
    fn a_title_without_a_top_border_is_not_drawn() {
        let mut block = Block::default()
            .borders(Borders::LEFT)
            .title(Line::raw("hi"));
        let grid = render(&mut block, 4, 2);
        assert_eq!(row_text(&grid, 0), "│   ");
    }

    #[test]
    fn inner_rect_subtracts_only_the_configured_sides() {
        let all = Block::default().borders(Borders::ALL);
        assert_eq!(all.inner(Rect::new(2, 3, 10, 6)), Rect::new(3, 4, 8, 4));
        let left_bar = Block::default().borders(Borders::LEFT);
        assert_eq!(
            left_bar.inner(Rect::new(2, 3, 10, 6)),
            Rect::new(3, 3, 9, 6)
        );
        // Too small to contain its own borders: empty, so content cannot spill.
        assert_eq!(all.inner(Rect::new(0, 0, 1, 1)).width, 0);
        assert_eq!(all.inner(Rect::new(0, 0, 1, 1)).height, 0);
    }

    #[test]
    fn background_fill_covers_the_whole_area() {
        let bg = Style::default().bg(Color::Rgb(4, 4, 4));
        let mut block = Block::default().borders(Borders::ALL).style(bg);
        let grid = render(&mut block, 4, 3);
        assert!(
            grid.cells()
                .iter()
                .all(|c| c.style.bg == Color::Rgb(4, 4, 4))
        );
    }

    #[test]
    fn measure_reports_only_its_chrome() {
        assert_eq!(Block::default().borders(Borders::ALL).measure(20), 2);
        assert_eq!(Block::default().borders(Borders::TOP).measure(20), 1);
        assert_eq!(Block::default().measure(20), 0);
    }
}
