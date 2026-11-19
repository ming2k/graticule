//! The `Clear` widget: reset a region to blank cells.

use crate::buffer::Style;
use crate::layout::Rect;
use crate::render::paint::{Paint, Widget};

/// Blanks a region.
///
/// With no style the region becomes terminal-default blanks, which is what an
/// application wants when removing a popup: the surface underneath must not show
/// through the cleared area. With a style the blanks carry that style, which is
/// what filling a panel's interior wants. One type covers both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clear {
    /// The style the blank cells carry, or `None` for terminal-default blanks.
    pub style: Option<Style>,
}

impl Clear {
    /// Clear to terminal-default blanks.
    pub const fn new() -> Self {
        Self { style: None }
    }

    /// Clear to blanks carrying `style`.
    pub const fn styled(style: Style) -> Self {
        Self { style: Some(style) }
    }
}

impl Widget for Clear {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        paint.clear(area, self.style.unwrap_or(Style::RESET));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{CellGrid, Color, Damage};
    use crate::layout::Rect;

    #[test]
    fn clear_blanks_its_area_and_leaves_the_rest_alone() {
        let mut grid = CellGrid::new(6, 2);
        let mut damage = Damage::new(2);
        let area = grid.area();
        {
            let mut paint = Paint::new(&mut grid, &mut damage, area);
            paint.put(0, 0, crate::buffer::grid::Fit::Clip, Style::RESET, "abcdef");
        }
        damage.clear();
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        Clear::new().render(Rect::new(1, 0, 3, 1), &mut paint);

        let row: String = (0..6)
            .map(|x| grid.get(x, 0).unwrap().symbol.as_str().to_string())
            .collect();
        assert_eq!(row, "a   ef");
        assert_eq!(damage.col_of(0), Some(1));
    }

    #[test]
    fn styled_clear_paints_the_style() {
        let mut grid = CellGrid::new(4, 1);
        let mut damage = Damage::new(1);
        let area = grid.area();
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        let bg = Color::Rgb(1, 2, 3);
        Clear::styled(Style::fill(bg)).render(Rect::new(0, 0, 2, 1), &mut paint);
        assert_eq!(grid.get(0, 0).unwrap().style.bg, bg);
        assert_eq!(grid.get(2, 0).unwrap().style.bg, Color::Reset);
    }
}
