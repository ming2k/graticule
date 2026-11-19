//! Geometry primitives: `Rect`, `Margin`, `Constraint`, `Direction`, and
//! `Layout`.
//!
//! [`Layout`] is a thin front for the one real layout solver,
//! [`crate::layout::flex`]. `Constraint` is constructor shorthand for
//! a `FlexItem`, not a second algorithm:
//!
//! | constraint      | flex item                                    |
//! |-----------------|----------------------------------------------|
//! | `Length(l)`     | basis `l`, grow 0, shrink 0                  |
//! | `Percentage(p)` | basis `p%` of the total, rounded half-up     |
//! | `Min(m)`        | basis `m`, **minimum `m`**, grow 1, shrink 1 |
//! | `Max(m)`        | basis 0, **maximum `m`**, grow 1             |
//!
//! `Min` and `Max` are bounds, not hints: an item never ends up smaller than its
//! `Min` while a sibling can still absorb the deficit, and never larger than its
//! `Max` while another grow child can take the surplus.

/// A rectangular region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Left edge, in columns.
    pub x: u16,
    /// Top edge, in rows.
    pub y: u16,
    /// Width, in columns.
    pub width: u16,
    /// Height, in rows.
    pub height: u16,
}

impl Rect {
    /// A rectangle at `(x, y)` with the given size.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns a rect that has been shrunk by `margin.horizontal` on each
    /// side and `margin.vertical` on top and bottom.
    pub fn inner(self, margin: Margin) -> Rect {
        if self.width < 2 * margin.horizontal || self.height < 2 * margin.vertical {
            return Rect::new(self.x, self.y, 0, 0);
        }
        Rect::new(
            self.x + margin.horizontal,
            self.y + margin.vertical,
            self.width - 2 * margin.horizontal,
            self.height - 2 * margin.vertical,
        )
    }

    /// Clamp a point to be inside this rect. Used by the app to normalize
    /// mouse coordinates.
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Intersection in terminal coordinates, including empty rectangles.
    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self::new(
            x,
            y,
            self.right().min(other.right()).saturating_sub(x),
            self.bottom().min(other.bottom()).saturating_sub(y),
        )
    }

    /// Area (width × height) in cells.
    pub const fn area(self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Whether this rectangle covers zero area.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Right edge x coordinate (exclusive).
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge y coordinate (exclusive).
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

/// A margin to apply when computing an inner rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Margin {
    /// Columns removed from each side.
    pub horizontal: u16,
    /// Rows removed from the top and bottom.
    pub vertical: u16,
}

impl Margin {
    /// A symmetric margin.
    pub const fn new(horizontal: u16, vertical: u16) -> Self {
        Self {
            horizontal,
            vertical,
        }
    }
}

/// A layout constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    /// Fill all remaining space.
    Min(u16),
    /// A fixed length.
    Length(u16),
    /// A percentage of the available space (0–100), rounded half-up.
    Percentage(u16),
    /// Fill the remaining space, but never more than this.
    Max(u16),
}

/// Layout direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    /// Stacked top to bottom.
    Vertical,
    /// Laid out left to right.
    Horizontal,
}

/// A layout entry point: given a `Rect` and a list of `Constraint`s, `split`
/// solves the sub-rects using the [`crate::layout::flex`] solver.
///
/// Each constraint maps to a flex item (see module documentation for the table).
/// `Min` constraints enforce their floor and distribute remaining space; `Max`
/// constraints cap expansion; and the solver iterates so capped surplus is
/// redistributed rather than stranded.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// The axis the constraints are applied along.
    pub direction: Direction,
    /// The constraints, in visual order.
    pub constraints: Vec<Constraint>,
}

impl Layout {
    /// Set the layout axis.
    pub fn direction(mut self, dir: Direction) -> Self {
        self.direction = dir;
        self
    }

    /// Set the constraints.
    pub fn constraints(mut self, cs: impl IntoIterator<Item = Constraint>) -> Self {
        self.constraints = cs.into_iter().collect();
        self
    }

    /// Split `area` into sub-rects according to the constraints.
    ///
    /// One rect per constraint, in order. A zero width or height yields an empty
    /// list. See the module docs for the constraint-to-flex mapping.
    pub fn split(self, area: Rect) -> Vec<Rect> {
        if self.constraints.is_empty() || area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        let total = match self.direction {
            Direction::Vertical => area.height,
            Direction::Horizontal => area.width,
        };
        let items: Vec<crate::layout::flex::FlexItem> = self
            .constraints
            .iter()
            .map(|c| match *c {
                Constraint::Length(l) => crate::layout::flex::FlexItem::fixed(l),
                // Rounded half-up, so 50% of an odd total does not silently
                // undershoot.
                Constraint::Percentage(p) => crate::layout::flex::FlexItem::fixed(
                    ((p as u32 * total as u32 + 50) / 100) as u16,
                ),
                // The floor is real: `min_main` freezes the item once it reaches
                // its minimum, so the deficit is redistributed to the siblings
                // that can still absorb it.
                Constraint::Min(m) => crate::layout::flex::FlexItem::fixed(m)
                    .with_min_main(m)
                    .with_grow(1)
                    .with_shrink(1),
                // Fills the remainder, never beyond `m`.
                Constraint::Max(m) => crate::layout::flex::FlexItem::fixed(0)
                    .with_max_main(m)
                    .with_grow(1),
            })
            .collect();
        let flex = crate::layout::flex::Flex {
            direction: match self.direction {
                Direction::Vertical => crate::layout::flex::FlexDirection::Column,
                Direction::Horizontal => crate::layout::flex::FlexDirection::Row,
            },
            ..crate::layout::flex::Flex::default()
        };
        flex.solve_with(area, &items, &|_, _| 0)
            .iter()
            .copied()
            .collect()
    }
}
