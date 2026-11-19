//! Declarative composition: `Column`, `Row`, `Stack`, `Container`, `Divider`,
//! `Spacer`, `Padding`.
//!
//! Every container in this module builds [`FlexItem`]s and hands them to the one
//! solver, [`Flex`]. Spacing is the solver's `gap`, so there is no
//! index arithmetic over injected constraint chunks; an unconstrained child is a
//! `FlexItem::auto()` whose main size comes from its own
//! [`Widget::measure`], which is what stops a `Spacer` and a `Paragraph` being
//! treated as interchangeable.
//!
//! Children are borrowed, not owned: `Column<'a>` holds
//! `Box<dyn Widget + 'a>`, so a `Paragraph<'_>` that borrows application strings
//! can be composed.

use crate::buffer::glyph::GlyphSet;
use crate::buffer::{Cell, Style};
use crate::layout::rect::{Constraint, Margin, Rect};
use crate::layout::{Flex, FlexItem, SolvedFlex};
use crate::render::paint::{Paint, Widget};

/// A type-erased owned widget, for a heterogeneous collection.
pub type AnyWidget = Box<dyn Widget>;

/// Four-sided spacing specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    /// Rows above.
    pub top: u16,
    /// Columns to the right.
    pub right: u16,
    /// Rows below.
    pub bottom: u16,
    /// Columns to the left.
    pub left: u16,
}

impl Padding {
    /// No padding.
    pub const fn new() -> Self {
        Self {
            top: 0,
            right: 0,
            bottom: 0,
            left: 0,
        }
    }
    /// The same value on all four sides.
    pub const fn all(v: u16) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
    /// Horizontal only.
    pub const fn horizontal(v: u16) -> Self {
        Self {
            top: 0,
            right: v,
            bottom: 0,
            left: v,
        }
    }
    /// Vertical only.
    pub const fn vertical(v: u16) -> Self {
        Self {
            top: v,
            right: 0,
            bottom: v,
            left: 0,
        }
    }
    /// The rect left after removing this padding, or a zero-sized rect when
    /// `rect` is too small to contain it.
    pub fn shrink(self, rect: Rect) -> Rect {
        let w = rect.width.saturating_sub(self.left + self.right);
        let h = rect.height.saturating_sub(self.top + self.bottom);
        if w == 0 || h == 0 {
            return Rect::new(
                rect.x.saturating_add(self.left),
                rect.y.saturating_add(self.top),
                0,
                0,
            );
        }
        Rect::new(
            rect.x.saturating_add(self.left),
            rect.y.saturating_add(self.top),
            w,
            h,
        )
    }
}

impl From<Margin> for Padding {
    fn from(m: Margin) -> Self {
        Self {
            top: m.vertical,
            right: m.horizontal,
            bottom: m.vertical,
            left: m.horizontal,
        }
    }
}

/// A vertical stack of children laid out by [`Flex`].
#[derive(Default)]
pub struct Column<'a> {
    children: Vec<(Option<Constraint>, Box<dyn Widget + 'a>)>,
    spacing: u16,
}

impl<'a> Column<'a> {
    /// An empty column.
    pub fn new() -> Self {
        Self::default()
    }
    /// Rows inserted between consecutive children.
    pub fn spacing(mut self, spacing: u16) -> Self {
        self.spacing = spacing;
        self
    }
    /// Append a child that measures itself.
    pub fn child(mut self, widget: impl Widget + 'a) -> Self {
        self.children.push((None, Box::new(widget)));
        self
    }
    /// Append a child with an explicit main-axis constraint.
    pub fn child_with_constraint(
        mut self,
        constraint: Constraint,
        widget: impl Widget + 'a,
    ) -> Self {
        self.children.push((Some(constraint), Box::new(widget)));
        self
    }
    /// Append an already-boxed child.
    pub fn child_boxed(mut self, widget: Box<dyn Widget + 'a>) -> Self {
        self.children.push((None, widget));
        self
    }
}

/// A horizontal stack of children laid out by [`Flex`].
#[derive(Default)]
pub struct Row<'a> {
    children: Vec<(Option<Constraint>, Box<dyn Widget + 'a>)>,
    spacing: u16,
}

impl<'a> Row<'a> {
    /// An empty row.
    pub fn new() -> Self {
        Self::default()
    }
    /// Columns inserted between consecutive children.
    pub fn spacing(mut self, spacing: u16) -> Self {
        self.spacing = spacing;
        self
    }
    /// Append a child that measures itself.
    pub fn child(mut self, widget: impl Widget + 'a) -> Self {
        self.children.push((None, Box::new(widget)));
        self
    }
    /// Append a child with an explicit main-axis constraint.
    pub fn child_with_constraint(
        mut self,
        constraint: Constraint,
        widget: impl Widget + 'a,
    ) -> Self {
        self.children.push((Some(constraint), Box::new(widget)));
        self
    }
    /// Append an already-boxed child.
    pub fn child_boxed(mut self, widget: Box<dyn Widget + 'a>) -> Self {
        self.children.push((None, widget));
        self
    }
}

/// The flex item a constraint maps onto. See the `layout::rect` module docs for
/// the table; this is the shared implementation of it.
fn item_for(constraint: Constraint, total: u16) -> FlexItem {
    match constraint {
        Constraint::Length(l) => FlexItem::fixed(l),
        Constraint::Percentage(p) => {
            FlexItem::fixed(((p as u32 * total as u32 + 50) / 100).min(u16::MAX as u32) as u16)
        }
        Constraint::Min(m) => FlexItem::fixed(m)
            .with_min_main(m)
            .with_grow(1)
            .with_shrink(1),
        Constraint::Max(m) => FlexItem::fixed(0).with_max_main(m).with_grow(1),
    }
}

/// The flex size of an unconstrained child.
///
/// The two axes are asked different questions, because they have different
/// answers:
///
/// - **Column**: the cross axis is the *width* the child will receive, so the
///   child can report the rows it needs (`Paragraph::measure(width)`). Surplus
///   goes to whoever declares [`Widget::grow`].
/// - **Row**: the cross axis is the height, from which no useful width follows.
///   An unconstrained child therefore *fills* the remaining width
///   (`flex: 1 1 auto`), and its content is clipped or wrapped inside.
fn auto_item(child: &dyn Widget, flex: &Flex, total_main: u16) -> FlexItem {
    match flex.direction {
        crate::layout::FlexDirection::Column => FlexItem::auto().with_grow(child.grow()),
        crate::layout::FlexDirection::Row => FlexItem::grow().grow(child.grow().max(1)).build(),
    }
    .with_max_main(total_main)
}

/// Shared body for `Column` and `Row`.
fn compose(
    children: &mut [(Option<Constraint>, Box<dyn Widget + '_>)],
    flex: Flex,
    area: Rect,
    total_main: u16,
    paint: &mut Paint<'_>,
) {
    if area.is_empty() || children.is_empty() {
        return;
    }
    let items: Vec<FlexItem> = children
        .iter()
        .map(|(c, w)| match c {
            Some(c) => item_for(*c, total_main),
            None => auto_item(w.as_ref(), &flex, total_main),
        })
        .collect();
    let measure = |i: usize, cross: u16| children.get(i).map_or(0, |(_, w)| w.measure(cross));
    let solved: SolvedFlex = flex.solve_with(area, &items, &measure);
    lay_out(children, &solved, paint);
}

/// Render each child into its solved rect, clipped to it.
fn lay_out(
    children: &mut [(Option<Constraint>, Box<dyn Widget + '_>)],
    solved: &SolvedFlex,
    paint: &mut Paint<'_>,
) {
    for (i, (_, child)) in children.iter_mut().enumerate() {
        if i >= solved.len() {
            break;
        }
        let rect = solved.rect(i);
        if rect.is_empty() {
            continue;
        }
        if let Some(mut child_paint) = paint.fit(rect) {
            child.render(rect, &mut child_paint);
        }
    }
}

/// The main-axis length a container can offer its children.
fn main_of(flex: &Flex, area: Rect) -> u16 {
    match flex.direction {
        crate::layout::FlexDirection::Column => area.height,
        crate::layout::FlexDirection::Row => area.width,
    }
}

impl Widget for Column<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        let flex = Flex::column().gap(self.spacing);
        compose(&mut self.children, flex, area, area.height, paint);
    }

    fn measure(&self, cross: u16) -> u16 {
        let mut total = 0u16;
        for (i, (c, child)) in self.children.iter().enumerate() {
            if i > 0 {
                total = total.saturating_add(self.spacing);
            }
            total = total.saturating_add(match c {
                Some(Constraint::Length(l)) => *l,
                Some(_) => 0,
                None => child.measure(cross),
            });
        }
        total
    }
}

impl Widget for Row<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        let flex = Flex::row().gap(self.spacing);
        compose(&mut self.children, flex, area, area.width, paint);
    }

    fn measure(&self, cross: u16) -> u16 {
        let mut total = 0u16;
        for (i, (c, child)) in self.children.iter().enumerate() {
            if i > 0 {
                total = total.saturating_add(self.spacing);
            }
            total = total.saturating_add(match c {
                Some(Constraint::Length(l)) => *l,
                Some(_) => 0,
                None => child.measure(cross),
            });
        }
        total
    }
}

/// Paints every child over the whole area, in order (later children on top).
#[derive(Default)]
pub struct Stack<'a> {
    children: Vec<Box<dyn Widget + 'a>>,
}

impl<'a> Stack<'a> {
    /// An empty stack.
    pub fn new() -> Self {
        Self::default()
    }
    /// Append a child, painted above the existing ones.
    pub fn child(mut self, widget: impl Widget + 'a) -> Self {
        self.children.push(Box::new(widget));
        self
    }
}

impl Widget for Stack<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        for child in &mut self.children {
            if let Some(mut child_paint) = paint.fit(area) {
                child.render(area, &mut child_paint);
            }
        }
    }

    fn measure(&self, cross: u16) -> u16 {
        self.children
            .iter()
            .map(|c| c.measure(cross))
            .max()
            .unwrap_or(0)
    }
}

/// Wraps one child with margin, a background fill and padding, in that order.
pub struct Container<'a> {
    child: Box<dyn Widget + 'a>,
    margin: Margin,
    padding: Padding,
    background: Option<Style>,
}

impl<'a> Container<'a> {
    /// Wrap `child`.
    pub fn new(child: impl Widget + 'a) -> Self {
        Self {
            child: Box::new(child),
            margin: Margin::default(),
            padding: Padding::default(),
            background: None,
        }
    }
    /// Space outside the background fill.
    pub fn margin(mut self, margin: Margin) -> Self {
        self.margin = margin;
        self
    }
    /// Space inside the background fill, around the child.
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }
    /// Fill the container's area with `style` before painting the child.
    pub fn background(mut self, style: Style) -> Self {
        self.background = Some(style);
        self
    }
}

impl Widget for Container<'_> {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        let area = area.inner(self.margin);
        if area.is_empty() {
            return;
        }
        let Some(mut paint) = paint.fit(area) else {
            return;
        };
        if let Some(bg) = self.background {
            paint.fill(area, bg);
        }
        let inner = self.padding.shrink(area);
        if inner.is_empty() {
            return;
        }
        if let Some(mut child_paint) = paint.fit(inner) {
            self.child.render(inner, &mut child_paint);
        }
    }

    fn measure(&self, cross: u16) -> u16 {
        let inner_cross = cross
            .saturating_sub(2 * self.margin.horizontal)
            .saturating_sub(self.padding.left + self.padding.right);
        self.padding
            .top
            .saturating_add(self.child.measure(inner_cross))
            .saturating_add(self.padding.bottom)
            .saturating_add(2 * self.margin.vertical)
    }
}

/// A widget that claims space and paints nothing.
///
/// It measures 0, so a parent gives it the share that is left over — the flexbox
/// `flex: 1 1 0` behaviour rather than an equal split with a content-bearing
/// sibling.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spacer;

impl Spacer {
    /// A spacer.
    pub const fn new() -> Self {
        Self
    }
}

impl Widget for Spacer {
    fn render(&mut self, _area: Rect, _paint: &mut Paint<'_>) {}
    fn grow(&self) -> u16 {
        1
    }
}

/// A single rule line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    /// Draw a horizontal rule (the default) or a vertical one.
    pub horizontal: bool,
    /// The rule's style.
    pub style: Style,
    /// Explicit glyph set, which selects the rule character.
    pub glyph_set: Option<&'static GlyphSet>,
    /// Override the rule character.
    pub glyph: Option<&'static str>,
}

impl Default for Divider {
    fn default() -> Self {
        Self {
            horizontal: true,
            style: Style::RESET,
            glyph_set: None,
            glyph: None,
        }
    }
}

impl Divider {
    /// A horizontal rule.
    pub fn horizontal() -> Self {
        Self::default()
    }
    /// A vertical rule.
    pub fn vertical() -> Self {
        Self {
            horizontal: false,
            ..Self::default()
        }
    }
    /// Set the rule's style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    /// Override the rule character.
    pub fn glyph(mut self, glyph: &'static str) -> Self {
        self.glyph = Some(glyph);
        self
    }
    /// Select the rule character from a glyph set.
    pub fn glyph_set(mut self, set: &'static GlyphSet) -> Self {
        self.glyph_set = Some(set);
        self
    }

    /// The rule character: an explicit override, else the glyph set's rule, else
    /// the light box-drawing default. The light default is deliberate: a divider
    /// is a light rule, while `GlyphSet::border_v` is a *panel frame's* glyph.
    fn symbol(&self) -> &'static str {
        if let Some(g) = self.glyph {
            return g;
        }
        match (self.glyph_set, self.horizontal) {
            (Some(set), true) => set.border_h,
            (Some(set), false) => set.border_v,
            (None, true) => "─",
            (None, false) => "│",
        }
    }
}

impl Widget for Divider {
    fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
        if area.is_empty() {
            return;
        }
        let symbol = self.symbol();
        if self.horizontal {
            let mut x = area.x;
            while x < area.right() {
                paint.set(x, area.y, Cell::narrow(symbol, self.style));
                x += 1;
            }
        } else {
            for y in area.y..area.bottom() {
                paint.set(area.x, y, Cell::narrow(symbol, self.style));
            }
        }
    }

    fn measure(&self, _cross: u16) -> u16 {
        1
    }
}

/// Extension methods for any widget.
pub trait WidgetExt: Widget + Sized + 'static {
    /// Erase this widget's type into an [`AnyWidget`].
    fn boxed(self) -> AnyWidget {
        Box::new(self)
    }
    /// Wrap this widget with padding.
    fn with_padding(self, padding: Padding) -> Container<'static> {
        Container::new(self).padding(padding)
    }
    /// Wrap this widget with a background fill.
    fn with_background(self, style: Style) -> Container<'static> {
        Container::new(self).background(style)
    }
}

impl<T: Widget + Sized + 'static> WidgetExt for T {}

/// The main-axis length a `Column`/`Row` offers its children. Exported for
/// callers that need to reason about the same number the solver used.
pub fn container_main(flex: &Flex, area: Rect) -> u16 {
    main_of(flex, area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{CellGrid, Damage};
    use crate::widgets::{Block, Borders, Paragraph};

    fn render(widget: &mut dyn Widget, w: u16, h: u16) -> CellGrid {
        let mut grid = CellGrid::new(w, h);
        let mut damage = Damage::new(h);
        let area = grid.area();
        let mut paint = Paint::new(&mut grid, &mut damage, area);
        widget.render(area, &mut paint);
        grid
    }

    fn col_text(grid: &CellGrid) -> Vec<String> {
        (0..grid.height())
            .map(|y| {
                (0..grid.width())
                    .map(|x| grid.get(x, y).unwrap().symbol.as_str().to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn constrained_children_split_the_column() {
        let mut col = Column::new()
            .child_with_constraint(Constraint::Length(1), Paragraph::new("Header"))
            .child(Paragraph::new("Body"));
        let grid = render(&mut col, 8, 4);
        assert_eq!(col_text(&grid)[0], "Header  ");
        assert_eq!(col_text(&grid)[1], "Body    ");
    }

    #[test]
    fn spacing_comes_from_the_solver_gap_not_index_arithmetic() {
        let mut col = Column::new()
            .child_with_constraint(Constraint::Length(1), Paragraph::new("a"))
            .child_with_constraint(Constraint::Length(1), Paragraph::new("b"))
            .spacing(1);
        let grid = render(&mut col, 3, 3);
        let rows = col_text(&grid);
        assert_eq!(rows[0], "a  ");
        assert_eq!(rows[1], "   ");
        assert_eq!(rows[2], "b  ");
    }

    #[test]
    fn a_spacer_yields_to_a_measured_sibling() {
        // The old behaviour split the remainder in equal shares, because every
        // unconstrained child became `Min(0)`. Now the paragraph reports the two
        // rows it needs and does not grow; the spacer needs nothing and does.
        let mut col = Column::new()
            .child(Paragraph::new("one\ntwo"))
            .child(Spacer::new());
        let grid = render(&mut col, 10, 6);
        let rows = col_text(&grid);
        assert_eq!(rows[0], "one       ");
        assert_eq!(rows[1], "two       ");
        assert_eq!(rows[2], "          ");
        // The spacer's own rect is the remainder, proved by painting into the
        // column with a marker child behind it.
        let mut marked = Column::new()
            .child(Paragraph::new("one\ntwo"))
            .child(Spacer::new())
            .child(Divider::horizontal());
        let grid = render(&mut marked, 10, 6);
        // The divider is the last constrained row, so the spacer took rows 2..5.
        assert_eq!(col_text(&grid)[5], "──────────");
    }

    #[test]
    fn row_lays_children_side_by_side() {
        let mut row = Row::new()
            .child_with_constraint(Constraint::Length(3), Paragraph::new("ab"))
            .child(Paragraph::new("cd"));
        let grid = render(&mut row, 6, 1);
        assert_eq!(col_text(&grid)[0], "ab cd ");
    }

    #[test]
    fn children_cannot_paint_outside_their_rect() {
        // A child that ignores its area still cannot write outside it: the
        // container narrows the clip.
        struct Greedy;
        impl Widget for Greedy {
            fn render(&mut self, area: Rect, paint: &mut Paint<'_>) {
                paint.put(
                    area.x,
                    area.y,
                    crate::buffer::grid::Fit::Clip,
                    Style::RESET,
                    "XXXXXXXX",
                );
            }
        }
        let mut row = Row::new()
            .child_with_constraint(Constraint::Length(2), Greedy)
            .child_with_constraint(Constraint::Length(2), Paragraph::new("zz"));
        let grid = render(&mut row, 4, 1);
        assert_eq!(col_text(&grid)[0], "XXzz");
    }

    #[test]
    fn stack_paints_in_order_and_measure_is_the_maximum() {
        let mut stack = Stack::new()
            .child(Paragraph::new("under"))
            .child(Paragraph::new("over"));
        let grid = render(&mut stack, 5, 1);
        // A stack overlays; it does not clear, so the last glyph of the
        // underlying child is still visible.
        assert_eq!(col_text(&grid)[0], "overr");
        assert_eq!(Stack::new().child(Paragraph::new("a")).measure(5), 1);
    }

    #[test]
    fn container_applies_margin_padding_and_background() {
        let bg = Style::default().bg(crate::buffer::Color::Rgb(1, 1, 1));
        let mut c = Container::new(Paragraph::new("x"))
            .margin(Margin::new(1, 0))
            .padding(Padding::all(1))
            .background(bg);
        let grid = render(&mut c, 6, 3);
        let rows = col_text(&grid);
        assert_eq!(rows[0], "      ");
        // One margin column plus one padding column before the content.
        assert_eq!(rows[1], "  x   ");
        // The background covers the padded area only.
        assert_eq!(
            grid.get(0, 1).unwrap().style.bg,
            crate::buffer::Color::Reset
        );
        assert_eq!(
            grid.get(1, 1).unwrap().style.bg,
            crate::buffer::Color::Rgb(1, 1, 1)
        );
    }

    #[test]
    fn divider_draws_a_full_rule() {
        let mut d = Divider::horizontal();
        let grid = render(&mut d, 5, 2);
        assert_eq!(col_text(&grid)[0], "─────");
        assert_eq!(col_text(&grid)[1], "     ");
        let mut v = Divider::vertical().glyph("|");
        let grid = render(&mut v, 3, 2);
        assert_eq!(col_text(&grid)[0], "|  ");
        assert_eq!(col_text(&grid)[1], "|  ");
    }

    #[test]
    fn boxed_widgets_compose_and_borrowed_content_is_allowed() {
        // The paragraph borrows a local String: composition no longer demands
        // 'static content.
        let owned = String::from("borrowed");
        let mut col = Column::new()
            .child(Paragraph::new(owned.as_str()))
            .child(Block::default().borders(Borders::ALL));
        let grid = render(&mut col, 10, 3);
        assert_eq!(col_text(&grid)[0], "borrowed  ");
    }

    #[test]
    fn measure_of_a_column_sums_children_and_gaps() {
        let col = Column::new()
            .child(Paragraph::new("a"))
            .child(Paragraph::new("b"))
            .spacing(1);
        assert_eq!(col.measure(10), 3);
        assert_eq!(container_main(&Flex::column(), Rect::new(0, 0, 4, 7)), 7);
    }
}
