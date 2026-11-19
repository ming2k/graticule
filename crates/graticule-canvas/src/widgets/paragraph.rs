//! The `Paragraph` widget: styled spans, wrapping, alignment and scrolling.

use std::borrow::Cow;

use crate::buffer::grid::Fit;
use crate::buffer::text::{str_len, wrap};
use crate::buffer::{Color, Style};
use crate::layout::Rect;
use crate::render::paint::{Paint, Widget};

use super::block::Block;

/// A styled string fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    /// The text.
    pub content: Cow<'a, str>,
    /// How it is presented.
    pub style: Style,
}

impl<'a> Span<'a> {
    /// An unstyled fragment.
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Self {
            content: content.into(),
            style: Style::RESET,
        }
    }
    /// A styled fragment.
    pub fn styled(content: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    /// The fragment's display width in columns.
    pub fn width(&self) -> usize {
        str_len(&self.content)
    }
}

impl<'a> From<Vec<Span<'a>>> for Line<'a> {
    fn from(spans: Vec<Span<'a>>) -> Self {
        Self {
            spans,
            ..Default::default()
        }
    }
}
impl<'a> From<Span<'a>> for Line<'a> {
    fn from(span: Span<'a>) -> Self {
        Self {
            spans: vec![span],
            ..Default::default()
        }
    }
}
impl<'a> From<&'a str> for Line<'a> {
    fn from(s: &'a str) -> Self {
        Line::raw(s)
    }
}
impl<'a> From<&'a str> for Span<'a> {
    fn from(s: &'a str) -> Self {
        Span::raw(s)
    }
}
impl<'a> From<String> for Span<'a> {
    fn from(s: String) -> Self {
        Span::raw(s)
    }
}

/// A line of spans, with an optional overall style and alignment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line<'a> {
    /// The line's fragments, in visual order.
    pub spans: Vec<Span<'a>>,
    /// Style applied under the spans' own styles.
    pub style: Style,
    /// This line's alignment, or `None` to inherit the paragraph's.
    pub alignment: Option<Alignment>,
}

impl<'a> Line<'a> {
    /// A line from a single unstyled fragment.
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Self {
            spans: vec![Span::raw(content)],
            ..Default::default()
        }
    }
    /// A line from a single fragment of any convertible type.
    ///
    /// Named `from_span` rather than `from` so it does not shadow the
    /// `From<Vec<Span>>` trait impl.
    pub fn from_span<T: Into<Span<'a>>>(span: T) -> Self {
        Self {
            spans: vec![span.into()],
            ..Default::default()
        }
    }
    /// A line from several fragments.
    pub fn from_spans(spans: Vec<Span<'a>>) -> Self {
        Self {
            spans,
            ..Default::default()
        }
    }
    /// Set the line's base style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    /// Override the paragraph's alignment for this line.
    pub fn alignment(mut self, a: Alignment) -> Self {
        self.alignment = Some(a);
        self
    }
    /// The total display width of this line.
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| str_len(&s.content)).sum()
    }
}

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Flush left.
    #[default]
    Left,
    /// Centered.
    Center,
    /// Flush right.
    Right,
}

/// Wrapping configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wrap {
    /// Drop leading whitespace from continuation lines (a wrapped line never
    /// starts with the spaces that belonged to the break).
    pub trim: bool,
}

impl Wrap {
    /// Wrap without trimming.
    pub const fn new() -> Self {
        Self { trim: false }
    }
    /// Wrap, trimming leading whitespace on continuation lines.
    pub const fn trimmed() -> Self {
        Self { trim: true }
    }
}

/// One or more lines with optional scrolling, wrapping, alignment and an
/// enclosing [`Block`].
#[derive(Debug, Clone, Default)]
pub struct Paragraph<'a> {
    /// The content lines.
    pub lines: Vec<Line<'a>>,
    /// `(rows, columns)` scrolled out of view.
    pub scroll: (u16, u16),
    /// Wrapping configuration; `None` renders each line on one row and clips.
    pub wrap: Option<Wrap>,
    /// The paragraph's alignment, used by lines that do not set their own.
    pub alignment: Alignment,
    /// An optional surrounding block.
    pub block: Option<Block<'a>>,
    /// Base style applied under every span.
    pub style: Style,
}

impl<'a> Paragraph<'a> {
    /// Build a paragraph from anything convertible into [`ParagraphLines`].
    pub fn new<T: Into<ParagraphLines<'a>>>(content: T) -> Self {
        Self {
            lines: content.into().0,
            ..Default::default()
        }
    }
    /// Scroll by `(rows, columns)`.
    pub fn scroll(mut self, row: u16, col: u16) -> Self {
        self.scroll = (row, col);
        self
    }
    /// Wrap to the available width.
    pub fn wrap(mut self, w: Wrap) -> Self {
        self.wrap = Some(w);
        self
    }
    /// Set the alignment used by lines that do not override it.
    pub fn alignment(mut self, a: Alignment) -> Self {
        self.alignment = a;
        self
    }
    /// Surround the paragraph with a block. The text is laid out inside the
    /// block's borders.
    pub fn block(mut self, b: Block<'a>) -> Self {
        self.block = Some(b);
        self
    }
    /// Set the base style.
    pub fn style(mut self, s: Style) -> Self {
        self.style = s;
        self
    }

    /// Lay the paragraph out for a given width, returning the display rows.
    fn layout_rows(&self, width: usize) -> Vec<Line<'a>> {
        let mut rows = Vec::new();
        for line in &self.lines {
            let wrapped = match self.wrap {
                Some(cfg) => wrap_line(line, width, cfg.trim),
                None => split_line_newlines(line),
            };
            rows.extend(wrapped);
        }
        rows
    }

    /// The content rect for `area`, accounting for the surrounding block.
    ///
    /// Uses [`Block::inner`] so top and bottom borders are subtracted too. The
    /// previous hand-rolled computation only removed left and right, which drew
    /// text straight over the top and bottom border rows.
    pub fn inner(&self, area: Rect) -> Rect {
        match &self.block {
            Some(block) => block.inner(area),
            None => area,
        }
    }
}

impl Widget for Paragraph<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        let inner = self.inner(area);
        if let Some(block) = &self.block {
            block.draw(area, paint);
        }
        if inner.is_empty() {
            return;
        }
        let Some(mut paint) = paint.fit(inner) else {
            return;
        };

        let max_width = inner.width as usize;
        let rows = self.layout_rows(max_width);
        let row_offset = self.scroll.0 as usize;
        let col_offset = self.scroll.1;

        for (i, row) in rows.iter().enumerate().skip(row_offset) {
            let y = inner.y.saturating_add((i - row_offset) as u16);
            if y >= inner.bottom() {
                return;
            }
            let alignment = row.alignment.unwrap_or(self.alignment);
            let base = merge_style(self.style, row.style);
            let row_width = row_width(row);
            let x = match alignment {
                Alignment::Left => inner.x,
                Alignment::Center => inner.x + (inner.width.saturating_sub(row_width)) / 2,
                Alignment::Right => inner.x + inner.width.saturating_sub(row_width),
            } + col_offset;

            // `Paint::put` clips at the clip's right edge, so a row longer than
            // the content rect is clipped there rather than spilling into the
            // border or the backdrop.
            let mut cx = x;
            for span in &row.spans {
                if cx >= inner.right() {
                    break;
                }
                let style = merge_style(base, span.style);
                let end = paint.put(cx, y, Fit::Clip, style, &span.content);
                cx = end.x.max(cx);
            }
        }
    }

    fn measure(&self, cross: u16) -> u16 {
        if cross == 0 {
            return 0;
        }
        let inner_width = self.inner(Rect::new(0, 0, cross, u16::MAX)).width as usize;
        if inner_width == 0 {
            return 0;
        }
        let rows = self.layout_rows(inner_width).len() as u16;
        // Add the block's own chrome, if any.
        match &self.block {
            Some(block) => rows.saturating_add(block.measure(cross)),
            None => rows,
        }
    }
}

/// Anything convertible into a paragraph's line list.
pub struct ParagraphLines<'a>(pub Vec<Line<'a>>);

impl<'a> From<Line<'a>> for ParagraphLines<'a> {
    fn from(l: Line<'a>) -> Self {
        ParagraphLines(vec![l])
    }
}
impl<'a> From<&'a str> for ParagraphLines<'a> {
    fn from(s: &'a str) -> Self {
        ParagraphLines(s.split('\n').map(Line::raw).collect())
    }
}
impl<'a> From<String> for ParagraphLines<'a> {
    fn from(s: String) -> Self {
        ParagraphLines(s.split('\n').map(|l| Line::raw(l.to_string())).collect())
    }
}
impl<'a> From<Vec<Line<'a>>> for ParagraphLines<'a> {
    fn from(v: Vec<Line<'a>>) -> Self {
        ParagraphLines(v)
    }
}

/// Overlay `over` on `base`: an explicit foreground/background wins, and
/// attributes accumulate.
fn merge_style(base: Style, over: Style) -> Style {
    Style {
        fg: if over.fg != Color::Reset {
            over.fg
        } else {
            base.fg
        },
        bg: if over.bg != Color::Reset {
            over.bg
        } else {
            base.bg
        },
        add: base.add | over.add,
    }
}

/// The display width of a laid-out row.
fn row_width(row: &Line<'_>) -> u16 {
    row.spans
        .iter()
        .map(|s| str_len(&s.content))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

/// Split a `Line` containing embedded newlines into independent `Line`s,
/// preserving each span's style and the parent's alignment and style.
fn split_line_newlines<'a>(line: &Line<'a>) -> Vec<Line<'a>> {
    if !line.spans.iter().any(|s| s.content.contains('\n')) {
        return vec![line.clone()];
    }
    let mut out = Vec::new();
    let mut current_spans = Vec::new();
    for span in &line.spans {
        for (i, part) in span.content.split('\n').enumerate() {
            if i > 0 {
                out.push(Line {
                    spans: std::mem::take(&mut current_spans),
                    style: line.style,
                    alignment: line.alignment,
                });
            }
            let clean = part.strip_suffix('\r').unwrap_or(part);
            if !clean.is_empty() {
                current_spans.push(Span::styled(clean.to_string(), span.style));
            }
        }
    }
    out.push(Line {
        spans: current_spans,
        style: line.style,
        alignment: line.alignment,
    });
    out
}

/// Wrap one line to `max_width` columns, re-splitting the wrapped byte ranges
/// back into spans so per-span styles survive a break.
///
/// With `trim`, a continuation row drops the leading spaces that belonged to the
/// break, which is what `Wrap::trim` has always claimed to do.
fn wrap_line<'a>(line: &Line<'a>, max_width: usize, trim: bool) -> Vec<Line<'a>> {
    let mut flat = String::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for span in &line.spans {
        let start = flat.len();
        flat.push_str(&span.content);
        ranges.push((start, flat.len()));
    }
    let wrapped = wrap(&flat, max_width);
    let mut out = Vec::with_capacity(wrapped.len());
    for (i, wl) in wrapped.iter().enumerate() {
        let mut lo = wl.start_byte;
        let hi = wl.end_byte;
        if trim && i > 0 {
            while lo < hi && flat.as_bytes()[lo] == b' ' {
                lo += 1;
            }
        }
        let mut spans: Vec<Span<'a>> = Vec::new();
        for (j, span) in line.spans.iter().enumerate() {
            let (s, e) = ranges[j];
            let a = lo.max(s);
            let b = hi.min(e);
            if a < b
                && let Some(sub) = span.content.get(a - s..b - s)
            {
                spans.push(Span::styled(sub.to_string(), span.style));
            }
        }
        out.push(Line {
            spans,
            style: line.style,
            alignment: line.alignment,
        });
    }
    if out.is_empty() {
        out.push(Line {
            spans: Vec::new(),
            style: line.style,
            alignment: line.alignment,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{CellGrid, Damage};

    /// Render a paragraph into a fresh grid and return it.
    fn render(p: &mut Paragraph<'_>, w: u16, h: u16) -> CellGrid {
        let mut grid = CellGrid::new(w, h);
        let mut damage = Damage::new(h);
        let area = grid.area();
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        p.render(area, &mut paint);
        grid
    }

    fn row_text(grid: &CellGrid, y: u16) -> String {
        (0..grid.width())
            .map(|x| grid.get(x, y).unwrap().symbol.as_str().to_string())
            .collect()
    }

    #[test]
    fn text_is_written_with_styles() {
        let style = Style::default().fg(Color::Rgb(1, 2, 3));
        let mut p = Paragraph::new(Line::from(Span::styled("hi", style)));
        let grid = render(&mut p, 4, 1);
        assert_eq!(row_text(&grid, 0), "hi  ");
        assert_eq!(grid.get(0, 0).unwrap().style.fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn a_surrounding_block_subtracts_all_four_borders() {
        // The regression this replaces: only left/right were subtracted, so text
        // was drawn straight over the top and bottom border rows.
        let mut p = Paragraph::new("HELLO")
            .block(Block::default().borders(super::super::block::Borders::ALL));
        let grid = render(&mut p, 10, 5);
        assert_eq!(row_text(&grid, 0), "╭────────╮");
        assert_eq!(row_text(&grid, 1), "│HELLO   │");
        assert_eq!(row_text(&grid, 4), "╰────────╯");
    }

    #[test]
    fn a_left_bar_block_leaves_the_full_height_for_text() {
        use super::super::block::Borders;
        let mut p = Paragraph::new("a\nb\nc").block(Block::default().borders(Borders::LEFT));
        let grid = render(&mut p, 4, 3);
        assert_eq!(row_text(&grid, 0), "│a  ");
        assert_eq!(row_text(&grid, 1), "│b  ");
        assert_eq!(row_text(&grid, 2), "│c  ");
    }

    #[test]
    fn alignment_centers_and_right_aligns_within_the_content_rect() {
        let mut centered = Paragraph::new("ab").alignment(Alignment::Center);
        assert_eq!(row_text(&render(&mut centered, 6, 1), 0), "  ab  ");
        let mut right = Paragraph::new("ab").alignment(Alignment::Right);
        assert_eq!(row_text(&render(&mut right, 6, 1), 0), "    ab");
        // A line's own alignment wins over the paragraph's.
        let mut per_line = Paragraph::new(vec![
            Line::raw("ab").alignment(Alignment::Right),
            Line::raw("cd"),
        ])
        .alignment(Alignment::Center);
        let grid = render(&mut per_line, 6, 2);
        assert_eq!(row_text(&grid, 0), "    ab");
        assert_eq!(row_text(&grid, 1), "  cd  ");
    }

    #[test]
    fn wrapping_reflows_and_styles_survive_the_break() {
        let red = Style::default().fg(Color::Red);
        let line = Line::from(vec![
            Span::styled("abc", red),
            Span::styled("def", Style::default().fg(Color::Blue)),
        ]);
        let mut p = Paragraph::new(line).wrap(Wrap::new());
        let grid = render(&mut p, 4, 2);
        assert_eq!(row_text(&grid, 0), "abcd");
        assert_eq!(grid.get(3, 0).unwrap().style.fg, Color::Blue);
        assert_eq!(row_text(&grid, 1), "ef  ");
    }

    #[test]
    fn trim_drops_the_leading_whitespace_of_a_continuation_row() {
        // `wrap` breaks before the glyph that overflows, so a run of spaces can
        // strand at the start of the next row. Only `Wrap::trim` removes it.
        let mut untrimmed = Paragraph::new("aaa  bb").wrap(Wrap::new());
        let grid = render(&mut untrimmed, 4, 2);
        assert_eq!(row_text(&grid, 0), "aaa ");
        assert_eq!(row_text(&grid, 1), " bb ");

        let mut trimmed = Paragraph::new("aaa  bb").wrap(Wrap::trimmed());
        let grid = render(&mut trimmed, 4, 2);
        assert_eq!(row_text(&grid, 0), "aaa ");
        assert_eq!(row_text(&grid, 1), "bb  ");
    }

    #[test]
    fn embedded_newlines_start_new_rows() {
        let mut p = Paragraph::new("ab\ncd");
        let grid = render(&mut p, 4, 2);
        assert_eq!(row_text(&grid, 0), "ab  ");
        assert_eq!(row_text(&grid, 1), "cd  ");
    }

    #[test]
    fn scroll_skips_rows_and_columns() {
        let mut p = Paragraph::new("aa\nbb\ncc").scroll(2, 1);
        let grid = render(&mut p, 3, 1);
        // Row 2 of the content, shifted one column right by the column scroll.
        assert_eq!(row_text(&grid, 0), " cc");
    }

    #[test]
    fn an_oversized_row_is_clipped_at_the_content_rect() {
        let mut p = Paragraph::new("abcdefgh");
        let grid = render(&mut p, 4, 1);
        assert_eq!(row_text(&grid, 0), "abcd");
    }

    #[test]
    fn a_reset_background_leaves_the_surface_visible() {
        let surface = Style::default().bg(Color::Rgb(7, 7, 7));
        let mut grid = CellGrid::new(4, 1);
        let mut damage = Damage::new(1);
        let area = grid.area();
        {
            let mut paint = Paint::new(&mut grid, &mut damage, area);
            paint.fill(area, surface);
        }
        let mut p = Paragraph::new("ab");
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        p.render(area, &mut paint);
        assert_eq!(grid.get(0, 0).unwrap().style.bg, Color::Rgb(7, 7, 7));
    }

    #[test]
    fn measure_reports_the_wrapped_row_count_plus_chrome() {
        // Unwrapped content is one row per embedded newline, whatever the width.
        assert_eq!(Paragraph::new("aaa bb").measure(4), 1);
        let wrapped = Paragraph::new("aaa bb").wrap(Wrap::new());
        assert_eq!(wrapped.measure(4), 2);
        assert_eq!(wrapped.measure(100), 1);
        let with_block = Paragraph::new("ab")
            .wrap(Wrap::new())
            .block(Block::default().borders(super::super::block::Borders::ALL));
        assert_eq!(with_block.measure(10), 1 + 2);
    }
}
