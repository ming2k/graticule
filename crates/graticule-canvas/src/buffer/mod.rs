//! Buffer primitives: the retained cell grid, its damage bookkeeping, cell
//! representation, style attributes, grapheme clustering, and glyph sets.

pub mod cell;
pub mod glyph;
pub mod grid;
pub mod text;

pub use cell::{Cell, CellWidth, Color, CompactSymbol, Modifier, Style, indexed_to_rgb};
pub use glyph::{
    ASCII_FALLBACKS, ASCII_GLYPHS, GlyphSet, UNICODE_GLYPHS, ascii_fallback, to_ascii,
};
pub use grid::{BandRotation, CellGrid, Damage, Fit, Pos};
pub use text::{
    Piece, cursor_column, floor_grapheme_boundary, grapheme_width, graphemes,
    inclusive_grapheme_end, is_hard_break, prohibited_line_end, prohibited_line_start, str_len,
    wrap,
};
