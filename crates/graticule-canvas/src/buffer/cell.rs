//! Display primitives: [`Color`], [`Style`], [`Modifier`], [`CellWidth`], and
//! [`Cell`].
//!
//! A [`Cell`] is the unit a grid stores per `(row, col)`. It carries one
//! grapheme cluster (which may be multi-codepoint, e.g. `e\u{0301}`), how many
//! columns that cluster occupies ([`CellWidth`]), and its presentation
//! ([`Style`]).
//!
//! Wide (CJK) glyphs own a trailing cell: the column to the right of a
//! [`CellWidth::Wide`] cell is a [`CellWidth::Continuation`] carrying the head's
//! background, so a stale multiplexer grid can never ghost it. The trailing
//! column is a *distinct state*, not "a cell whose width happens to be 0",
//! preventing the differ from confusing wide continuations with zero-width cells.

use std::fmt;

/// A terminal color.
///
/// `Reset` means "the terminal's default" and is intentionally distinct from
/// any RGB value: it is what blank padding and cleared cells resolve to when
/// the terminal (or multiplexer) paints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    /// The terminal's default color, distinct from any RGB value.
    Reset,
    /// A 24-bit truecolor value.
    Rgb(u8, u8, u8),
    /// 8-bit indexed palette color (0..255).
    Indexed(u8),
    /// Named ANSI black.
    Black,
    /// Named ANSI red.
    Red,
    /// Named ANSI green.
    Green,
    /// Named ANSI yellow.
    Yellow,
    /// Named ANSI blue.
    Blue,
    /// Named ANSI magenta.
    Magenta,
    /// Named ANSI cyan.
    Cyan,
    /// Named ANSI light grey (index 7).
    Gray,
    /// Named ANSI dark grey (index 8).
    DarkGray,
    /// Named ANSI bright red.
    LightRed,
    /// Named ANSI bright green.
    LightGreen,
    /// Named ANSI bright yellow.
    LightYellow,
    /// Named ANSI bright blue.
    LightBlue,
    /// Named ANSI bright magenta.
    LightMagenta,
    /// Named ANSI bright cyan.
    LightCyan,
    /// Named ANSI white.
    White,
}

impl Color {
    /// The RGB triple if this color has an exact RGB representation
    /// ([`Color::Rgb`] and [`Color::Indexed`]), else `None`.
    pub fn as_rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Indexed(idx) => Some(indexed_to_rgb(idx)),
            _ => None,
        }
    }

    /// Scale this color's channels toward black by `factor`
    /// (`0.0` = black, `1.0` = unchanged).
    ///
    /// Every color except [`Color::Reset`] participates: named and indexed
    /// colors are resolved through [`Color::to_rgb_approx`] first, so dimming a
    /// named color visibly dims it rather than silently doing nothing.
    /// `Reset` is a hole (there is nothing to scale) and passes through.
    pub fn scale(self, factor: f32) -> Color {
        if self == Color::Reset {
            return Color::Reset;
        }
        let f = factor.clamp(0.0, 1.0);
        let (r, g, b) = self.to_rgb_approx();
        Color::Rgb(
            (r as f32 * f).round() as u8,
            (g as f32 * f).round() as u8,
            (b as f32 * f).round() as u8,
        )
    }

    /// Relative luminance in `[0, 255]`, used to pick a contrast foreground for
    /// a colored fill (dark text on light backgrounds).
    pub fn luminance(self) -> f32 {
        let (r, g, b) = self.to_rgb_approx();
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    }

    /// Approximate RGB triple for any color variant. Named colors use the
    /// xterm-256 defaults, so contrast/scale/blend heuristics work on them too.
    pub fn to_rgb_approx(self) -> (u8, u8, u8) {
        match self {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(idx) => indexed_to_rgb(idx),
            Color::Reset | Color::Black => (0, 0, 0),
            Color::Red | Color::LightRed => (224, 108, 117),
            Color::Green | Color::LightGreen => (127, 216, 143),
            Color::Yellow | Color::LightYellow => (229, 192, 123),
            Color::Blue | Color::LightBlue => (137, 180, 250),
            Color::Magenta | Color::LightMagenta => (203, 166, 247),
            Color::Cyan | Color::LightCyan => (86, 182, 194),
            Color::Gray => (128, 128, 128),
            Color::DarkGray => (64, 64, 64),
            Color::White => (255, 255, 255),
        }
    }

    /// Black on light fills, white on dark fills.
    pub fn contrast_fg(self) -> Color {
        if self.luminance() > 140.0 {
            Color::Black
        } else {
            Color::White
        }
    }

    /// Linearly interpolate between two colors in RGB space.
    ///
    /// `t = 0.0` returns `self` unchanged, `t = 1.0` returns `other` unchanged.
    /// Named colors are resolved to their approximations first. [`Color::Reset`]
    /// is a hole rather than black: if exactly one side is `Reset`, the other
    /// side is returned; if both are, `Reset` is returned.
    pub fn blend(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        // Identity endpoints, checked before the Reset-hole case so the blend
        // contract holds exactly at the ends.
        if t == 0.0 {
            return self;
        }
        if t == 1.0 {
            return other;
        }
        if self == Color::Reset && other == Color::Reset {
            return Color::Reset;
        }
        if self == Color::Reset {
            return other;
        }
        if other == Color::Reset {
            return self;
        }
        let (r1, g1, b1) = self.to_rgb_approx();
        let (r2, g2, b2) = other.to_rgb_approx();
        Color::Rgb(
            (r1 as f32 + (r2 as f32 - r1 as f32) * t).round() as u8,
            (g1 as f32 + (g2 as f32 - g1 as f32) * t).round() as u8,
            (b1 as f32 + (b2 as f32 - b1 as f32) * t).round() as u8,
        )
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::Reset => f.write_str("reset"),
            Color::Rgb(r, g, b) => write!(f, "#{r:02X}{g:02X}{b:02X}"),
            Color::Indexed(idx) => write!(f, "idx({idx})"),
            Color::Black => f.write_str("#000"),
            Color::White => f.write_str("#FFF"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Translate an 8-bit color index (0..255) to standard (r, g, b) values.
pub const fn indexed_to_rgb(idx: u8) -> (u8, u8, u8) {
    if idx < 16 {
        const ANSI16_RGB: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (170, 0, 0),
            (0, 170, 0),
            (170, 85, 0),
            (0, 0, 170),
            (170, 0, 170),
            (0, 170, 170),
            (170, 170, 170),
            (85, 85, 85),
            (255, 85, 85),
            (85, 255, 85),
            (255, 255, 85),
            (85, 85, 255),
            (255, 85, 255),
            (85, 255, 255),
            (255, 255, 255),
        ];
        ANSI16_RGB[idx as usize]
    } else if idx < 232 {
        // 6x6x6 color cube: indices 16..231
        let val = idx - 16;
        let b = val % 6;
        let g = (val / 6) % 6;
        let r = val / 36;
        const fn to_cube_channel(c: u8) -> u8 {
            if c == 0 { 0 } else { 55 + c * 40 }
        }
        (to_cube_channel(r), to_cube_channel(g), to_cube_channel(b))
    } else {
        // 24 grayscale steps: indices 232..255
        let gray = 8 + (idx - 232) * 10;
        (gray, gray, gray)
    }
}

impl From<(u8, u8, u8)> for Color {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Color::Rgb(r, g, b)
    }
}

bitflags::bitflags! {
    /// Text attributes: the SGR (Select Graphic Rendition) bits a terminal can
    /// render. Kept as `bitflags` so styles compose with `|` and a diff can ask
    /// "which bits changed" with `^`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct Modifier: u16 {
        /// `SGR 1`: increased intensity.
        const BOLD          = 1 << 0;
        /// `SGR 2`: decreased intensity.
        const DIM           = 1 << 1;
        /// `SGR 3`: italic (mapped to underline where unsupported).
        const ITALIC        = 1 << 2;
        /// `SGR 4`: underline.
        const UNDERLINE     = 1 << 3;
        /// `SGR 7`: reverse video.
        const REVERSE       = 1 << 4;
        /// `SGR 9`: crossed out.
        const STRIKETHROUGH = 1 << 5;
    }
}

/// The full presentation of one cell: foreground/background color and
/// attributes. Two styles diff to "what SGR sequence turns the old into the
/// new", which is what the backend emits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Attributes added to the cell.
    pub add: Modifier,
}

impl Style {
    /// This style with a different foreground.
    pub fn fg(self, fg: Color) -> Self {
        Self { fg, ..self }
    }
    /// This style with a different background.
    pub fn bg(self, bg: Color) -> Self {
        Self { bg, ..self }
    }
    /// This style with `add` attributes added.
    pub fn add_modifier(self, add: Modifier) -> Self {
        Self {
            add: self.add | add,
            ..self
        }
    }
    /// This style with `remove` attributes removed.
    pub fn remove_modifier(self, remove: Modifier) -> Self {
        Self {
            add: self.add - remove,
            ..self
        }
    }

    /// The default "nothing rendered" style: terminal-default fg and bg, no
    /// attributes. A cell styled this way reads as the terminal's default.
    pub const RESET: Style = Style {
        fg: Color::Reset,
        bg: Color::Reset,
        add: Modifier::empty(),
    };

    /// A style that only sets the background (fg stays default).
    pub const fn fill(bg: Color) -> Self {
        Style {
            fg: Color::Reset,
            bg,
            add: Modifier::empty(),
        }
    }

    /// This style with every color resolution removed, keeping attributes.
    pub fn without_color(self) -> Self {
        Self {
            fg: Color::Reset,
            bg: Color::Reset,
            ..self
        }
    }
}

/// How many columns a [`Cell`] occupies, and whether it is the trailing half of
/// a wide glyph.
///
/// The four states are mutually exclusive by construction. That is the whole
/// point: the previous representation overloaded a `u8` width, so a
/// continuation, the undrawn sentinel, and a zero-column grapheme were all
/// "width 0" and the differ could not tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[repr(u8)]
pub enum CellWidth {
    /// Never drawn. Only equal to another `Empty` cell, so a fresh front grid
    /// differs from every real cell and the first frame repaints it all.
    #[default]
    Empty = 0,
    /// The trailing column of the [`CellWidth::Wide`] cell to the left. Carries
    /// that head's background so the column can never ghost.
    Continuation = 1,
    /// A single-column glyph.
    Narrow = 2,
    /// A double-column (CJK/emoji) glyph; its right neighbour is
    /// [`CellWidth::Continuation`].
    Wide = 3,
}

impl CellWidth {
    /// Columns painted by this cell's own glyph. A continuation paints nothing
    /// (the head's glyph spills into it).
    pub const fn columns(self) -> u8 {
        match self {
            CellWidth::Empty | CellWidth::Continuation => 0,
            CellWidth::Narrow => 1,
            CellWidth::Wide => 2,
        }
    }

    /// Grid slots consumed when scanning a row left to right. Unlike
    /// [`Self::columns`], a continuation consumes its own slot: the head
    /// advances past both columns.
    pub const fn advance(self) -> u16 {
        match self {
            CellWidth::Wide => 2,
            CellWidth::Empty | CellWidth::Continuation | CellWidth::Narrow => 1,
        }
    }

    /// Whether this is the trailing column of a wide glyph.
    pub const fn is_continuation(self) -> bool {
        matches!(self, CellWidth::Continuation)
    }

    /// Whether this cell has never been drawn.
    pub const fn is_empty(self) -> bool {
        matches!(self, CellWidth::Empty)
    }

    /// Classify a measured display width (as returned by
    /// [`crate::buffer::text::grapheme_width`]).
    pub const fn from_columns(cols: u8) -> Option<Self> {
        match cols {
            1 => Some(CellWidth::Narrow),
            2 => Some(CellWidth::Wide),
            _ => None,
        }
    }
}

/// An inline-first compact UTF-8 buffer for terminal cell symbols.
///
/// Stores grapheme clusters up to 22 bytes inline without heap allocation,
/// covering ASCII, CJK glyphs, emoji modifiers, and common ZWJ sequences. Falls
/// back to `Box<str>` only for unusually long multi-codepoint sequences. This is
/// why a plain ASCII/CJK frame performs zero per-cell heap allocation.
#[derive(Clone, Eq, Hash)]
pub enum CompactSymbol {
    /// The cluster fits the inline buffer.
    Inline {
        /// The UTF-8 bytes.
        buf: [u8; 22],
        /// How many of `buf`'s bytes are used.
        len: u8,
    },
    /// The cluster is longer than the inline capacity.
    Heap(Box<str>),
}

impl CompactSymbol {
    /// The empty symbol.
    pub const fn new() -> Self {
        Self::Inline {
            buf: [0; 22],
            len: 0,
        }
    }

    /// Borrow the symbol as a string slice.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Inline { buf, len } => {
                let bytes = &buf[..*len as usize];
                // SAFETY: every constructor of `Inline` copies bytes from a
                // `&str` (via `From<&str>`, `From<char>`, or `push_str` given a
                // `&str`), and `len` is that string's byte length, so the slice
                // is always valid UTF-8. `clear` only ever sets `len` to 0.
                #[allow(unsafe_code)]
                let s = unsafe { std::str::from_utf8_unchecked(bytes) };
                s
            }
            Self::Heap(s) => s.as_ref(),
        }
    }

    /// Whether this symbol is empty.
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }

    /// Truncate to the empty symbol, keeping the inline variant.
    pub fn clear(&mut self) {
        match self {
            Self::Inline { len, .. } => *len = 0,
            Self::Heap(_) => *self = Self::new(),
        }
    }

    /// Append a string, moving to the heap if the result exceeds the inline
    /// capacity.
    pub fn push_str(&mut self, s: &str) {
        let current = self.as_str();
        let total_len = current.len() + s.len();
        if total_len <= 22 {
            let mut buf = [0u8; 22];
            let cur_bytes = current.as_bytes();
            buf[..cur_bytes.len()].copy_from_slice(cur_bytes);
            buf[cur_bytes.len()..total_len].copy_from_slice(s.as_bytes());
            *self = Self::Inline {
                buf,
                len: total_len as u8,
            };
        } else {
            let mut next = String::with_capacity(total_len);
            next.push_str(current);
            next.push_str(s);
            *self = Self::Heap(next.into_boxed_str());
        }
    }
}

impl Default for CompactSymbol {
    fn default() -> Self {
        Self::from(" ")
    }
}

impl std::ops::Deref for CompactSymbol {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for CompactSymbol {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for CompactSymbol {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for CompactSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CompactSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl From<&str> for CompactSymbol {
    fn from(s: &str) -> Self {
        let bytes = s.as_bytes();
        if bytes.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..bytes.len()].copy_from_slice(bytes);
            Self::Inline {
                buf,
                len: bytes.len() as u8,
            }
        } else {
            Self::Heap(s.into())
        }
    }
}

impl From<String> for CompactSymbol {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&String> for CompactSymbol {
    fn from(s: &String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<char> for CompactSymbol {
    fn from(c: char) -> Self {
        let mut buf = [0u8; 22];
        let len = c.encode_utf8(&mut buf[..4]).len() as u8;
        Self::Inline { buf, len }
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for CompactSymbol {
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

/// A single grid cell: one grapheme cluster, its column occupancy, and its
/// presentation.
///
/// A [`CellWidth::Wide`] cell is the *head* of a wide glyph; the column to its
/// right must be a [`CellWidth::Continuation`] carrying the same background.
/// Writers maintain that pair and the differ relies on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster to render. Multi-codepoint for combining sequences
    /// and ZWJ emoji; compared by equality when diffing.
    pub symbol: CompactSymbol,
    /// The columns this cell occupies and whether it is a continuation.
    pub width: CellWidth,
    /// The cell's presentation.
    pub style: Style,
}

impl Cell {
    /// Build a cell from an already-measured symbol.
    pub fn new(symbol: impl Into<CompactSymbol>, width: CellWidth, style: Style) -> Self {
        Self {
            symbol: symbol.into(),
            width,
            style,
        }
    }

    /// The undrawn sentinel. Its symbol is empty and its width is
    /// [`CellWidth::Empty`], so it never matches a real drawn cell (not even a
    /// blank space) and therefore forces a repaint wherever it survives.
    pub fn uninitialized() -> Self {
        Self {
            symbol: CompactSymbol::new(),
            width: CellWidth::Empty,
            style: Style::RESET,
        }
    }

    /// A blank cell: one space column, terminal-default style. This is what an
    /// empty grid is filled with and what `clr_eol` conceptually writes.
    pub fn blank() -> Self {
        Self {
            symbol: CompactSymbol::from(" "),
            width: CellWidth::Narrow,
            style: Style::RESET,
        }
    }

    /// A blank cell pre-styled with a background — used to fill a band with a
    /// surface color (the app background, a panel background).
    pub fn blank_styled(style: Style) -> Self {
        Self {
            symbol: CompactSymbol::from(" "),
            width: CellWidth::Narrow,
            style,
        }
    }

    /// The trailing cell of a wide glyph. It renders nothing of its own (the
    /// head's glyph spills into this column) but owns the head's background so
    /// the column can never ghost.
    pub fn wide_continuation(head_style: Style) -> Self {
        Self {
            symbol: CompactSymbol::from(" "),
            width: CellWidth::Continuation,
            style: Style {
                bg: head_style.bg,
                ..Style::RESET
            },
        }
    }

    /// A single-column glyph with a style.
    pub fn narrow(symbol: impl Into<CompactSymbol>, style: Style) -> Self {
        Self {
            symbol: symbol.into(),
            width: CellWidth::Narrow,
            style,
        }
    }

    /// A double-column glyph with a style.
    pub fn wide(symbol: impl Into<CompactSymbol>, style: Style) -> Self {
        Self {
            symbol: symbol.into(),
            width: CellWidth::Wide,
            style,
        }
    }

    /// Whether this cell is the trailing half of a wide glyph.
    pub fn is_wide_continuation(&self) -> bool {
        self.width.is_continuation()
    }

    /// The columns this cell paints (0 for a continuation or an empty cell).
    pub fn columns(&self) -> u8 {
        self.width.columns()
    }

    /// The background this cell exposes to its right neighbour when a wide glyph
    /// is written over it.
    pub fn bg(&self) -> Color {
        self.style.bg
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_are_identity() {
        let a = Color::Rgb(10, 20, 30);
        let b = Color::Rgb(200, 100, 50);
        assert_eq!(a.blend(b, 0.0), a);
        assert_eq!(a.blend(b, 1.0), b);
    }

    #[test]
    fn blend_midpoint_is_average() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 60);
        assert_eq!(a.blend(b, 0.5), Color::Rgb(50, 100, 30));
    }

    #[test]
    fn blend_clamps_out_of_range_t() {
        let a = Color::Rgb(10, 20, 30);
        let b = Color::Rgb(200, 100, 50);
        assert_eq!(a.blend(b, -1.0), a);
        assert_eq!(a.blend(b, 2.0), b);
    }

    #[test]
    fn blend_named_color_uses_approximate_rgb() {
        assert_eq!(
            Color::White.blend(Color::Black, 0.5),
            Color::Rgb(128, 128, 128)
        );
    }

    #[test]
    fn blend_treats_reset_as_a_hole() {
        let rgb = Color::Rgb(1, 2, 3);
        assert_eq!(Color::Reset.blend(rgb, 0.5), rgb);
        assert_eq!(rgb.blend(Color::Reset, 0.5), rgb);
        assert_eq!(Color::Reset.blend(Color::Reset, 0.5), Color::Reset);
        assert_eq!(Color::Rgb(9, 9, 9).blend(Color::Reset, 1.0), Color::Reset);
    }

    #[test]
    fn scale_moves_every_color_except_reset() {
        assert_eq!(Color::Rgb(100, 100, 100).scale(0.5), Color::Rgb(50, 50, 50));
        // A named color participates by approximation instead of silently
        // ignoring the factor.
        assert_eq!(Color::White.scale(0.0), Color::Rgb(0, 0, 0));
        assert_ne!(Color::White.scale(0.5), Color::White);
        // Reset is a hole: there is nothing to scale.
        assert_eq!(Color::Reset.scale(0.5), Color::Reset);
    }

    #[test]
    fn cell_width_states_are_disjoint() {
        // The bug this replaces: a continuation, the sentinel and a zero-column
        // grapheme were all indistinguishable.
        assert!(CellWidth::Empty.is_empty());
        assert!(!CellWidth::Empty.is_continuation());
        assert!(CellWidth::Continuation.is_continuation());
        assert!(!CellWidth::Continuation.is_empty());
        assert_eq!(CellWidth::Empty.columns(), 0);
        assert_eq!(CellWidth::Continuation.columns(), 0);
        assert_eq!(CellWidth::Narrow.columns(), 1);
        assert_eq!(CellWidth::Wide.columns(), 2);
        // Every state still consumes a grid slot when scanning, and a wide
        // glyph consumes two.
        assert_eq!(CellWidth::Empty.advance(), 1);
        assert_eq!(CellWidth::Continuation.advance(), 1);
        assert_eq!(CellWidth::Narrow.advance(), 1);
        assert_eq!(CellWidth::Wide.advance(), 2);
        // A zero-column measurement is not representable as a drawn cell.
        assert_eq!(CellWidth::from_columns(0), None);
        assert_eq!(CellWidth::from_columns(1), Some(CellWidth::Narrow));
        assert_eq!(CellWidth::from_columns(2), Some(CellWidth::Wide));
        assert_ne!(CellWidth::Empty, CellWidth::Continuation);
    }

    #[test]
    fn sentinel_never_equals_a_drawn_cell() {
        let sentinel = Cell::uninitialized();
        assert_ne!(sentinel, Cell::blank());
        assert_ne!(sentinel, Cell::narrow("x", Style::RESET));
        assert_ne!(sentinel, Cell::wide_continuation(Style::RESET));
    }

    #[test]
    fn continuation_carries_the_head_background_only() {
        let head = Style {
            fg: Color::Red,
            bg: Color::Rgb(1, 2, 3),
            add: Modifier::BOLD,
        };
        let cont = Cell::wide_continuation(head);
        assert!(cont.is_wide_continuation());
        assert_eq!(cont.style.bg, head.bg);
        // The head's foreground and attributes do not spill into the column.
        assert_eq!(cont.style.fg, Color::Reset);
        assert!(cont.style.add.is_empty());
    }

    #[test]
    fn compact_symbol_stays_inline_up_to_capacity() {
        let mut s = CompactSymbol::new();
        s.push_str("abcdefghijklmnopqrstuv");
        assert!(matches!(s, CompactSymbol::Inline { .. }));
        assert_eq!(s.as_str(), "abcdefghijklmnopqrstuv");
        s.push_str("w");
        assert!(matches!(s, CompactSymbol::Heap(_)));
        assert_eq!(s.as_str(), "abcdefghijklmnopqrstuvw");
        // A multi-byte cluster within capacity stays inline and round-trips.
        let cjk = CompactSymbol::from("漢");
        assert!(matches!(cjk, CompactSymbol::Inline { .. }));
        assert_eq!(cjk.as_str(), "漢");
    }
}
