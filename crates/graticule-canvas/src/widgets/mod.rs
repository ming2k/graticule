//! Declarative widgets: `Block`, `Paragraph`, `Clear`, `Line`, `Span`, and the
//! layout containers `Column`, `Row`, `Stack`, `Container`, `Divider`,
//! `Spacer`, `Padding`.
//!
//! Every widget implements [`crate::render::paint::Widget`], which lives in the
//! render layer — widgets do not depend on `terminal`.

pub mod block;
pub mod clear;
pub mod compose;
pub mod paragraph;

pub use block::{Block, BorderType, Borders};
pub use clear::Clear;
pub use compose::{AnyWidget, Column, Container, Divider, Padding, Row, Spacer, Stack, WidgetExt};
pub use paragraph::{Alignment, Line, Paragraph, ParagraphLines, Span, Wrap};
