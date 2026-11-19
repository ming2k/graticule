//! A retained-mode terminal canvas and differential rendering engine.
//!
//! The engine knows only about cells, styles, grids, layout and terminal
//! capabilities. It carries no application vocabulary and zero OS dependencies,
//! so it is independently testable (feed two grids, assert the delta) and
//! reusable across native terminals, SSH/PTY servers, and WebAssembly hosts.
//!
//! # The model in one paragraph
//!
//! A [`CellGrid`] is the single source of truth for what the application wants on
//! screen. Writes mark the touched line dirty at write time ([`Damage`]), so
//! nothing rescans the frame; [`render::diff::diff`] compares the back grid
//! against the front grid (what the terminal currently shows) and emits a
//! run-length packed command stream; [`terminal::Backend`] turns those commands
//! into the minimal escape-code delta via `graticule-vt`; and the changed cells
//! are promoted into the front grid **only after the bytes have been flushed**,
//! so the retained model can never run ahead of the physical terminal.
//!
//! Wide (CJK) glyphs own their trailing column — the writer fills it with the
//! head's background — so no terminal or multiplexer can ghost it.
//!
//! # Subsystems
//!
//! Dependency direction is one way, top to bottom:
//!
//! ```text
//! ui        (retained scene graph + runtime)
//! widgets   (Block, Paragraph, Clear, Column, Row, Stack, Container, ...)
//! layout    (Rect, Flex, constraints, anchoring)     render (paint, diff, driver, profile)
//! buffer    (CellGrid, Damage, Cell, Style, text metrics, glyphs)
//! ```
//!
//! - [`buffer`]: the retained grid, its damage tracker, cells, styles, text
//!   metrics and glyph sets.
//! - [`layout`]: geometry, the one flexbox solver, and anchored positioning.
//! - [`render`]: the pure differ, the paint context and `Widget` contract,
//!   escape emission, and terminal capability profiles.
//! - [`terminal`]: the pure escape-emitting backend and the draw/commit loop.
//!   Raw mode and the alternate screen are managed explicitly by `graticule-tty`
//!   or the caller ([`Terminal::writer`]).
//! - [`widgets`]: declarative primitives and containers.
//! - [`ui`]: the retained scene graph and component runtime, for applications
//!   that want identity, local state, focus and input barriers.

#![warn(missing_docs)]
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod buffer;
pub mod layout;
pub mod render;
pub mod terminal;
pub mod ui;
pub mod widgets;

// Buffer primitives
pub use crate::buffer::{
    ASCII_FALLBACKS, ASCII_GLYPHS, BandRotation, Cell, CellGrid, CellWidth, Color, CompactSymbol,
    Damage, Fit, GlyphSet, Modifier, Piece, Pos, Style, UNICODE_GLYPHS, ascii_fallback,
    cursor_column, floor_grapheme_boundary, grapheme_width, graphemes, inclusive_grapheme_end,
    indexed_to_rgb, is_hard_break, prohibited_line_end, prohibited_line_start, str_len, to_ascii,
    wrap,
};

// Layout primitives
pub use crate::layout::{
    AlignItem, AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, AnchoredBox,
    Basis, Constraint, Direction, Flex, FlexDirection, FlexItem, Justify, Layout, Margin, Rect,
    SolvedFlex, compute_anchored_rect,
};

// Render primitives
pub use crate::render::{
    CharsetStandard, ColorStandard, Draw, DrawCmd, Driver, EscapeEmitter, Paint, Palette,
    TerminalProfile, Widget, promote, promote_scrolled, quantize_to_ansi16, quantize_to_indexed256,
    to_sgr_color,
};

// Terminal primitives
pub use crate::terminal::{Backend, Bce, CursorState, Frame, Terminal, TestTerminal};
pub use graticule_vt::CursorStyle;

// Widget primitives
pub use crate::widgets::{
    Alignment, AnyWidget, Block, BorderType, Borders, Clear, Column, Container, Divider, Line,
    Padding, Paragraph, ParagraphLines, Row, Spacer, Span, Stack, WidgetExt, Wrap,
};

// UI primitives
pub use crate::ui::{
    Component, InputPolicy, LayoutBox, Lifecycle, NodeId, NodeLayout, PointerPolicy, Scene,
    UiError, UiRuntime,
};
