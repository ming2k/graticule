//! Rendering: the pure grid differ, the paint context and widget contract,
//! escape-sequence emission, and terminal capability profiles.
//!
//! The pipeline is one direction only, and each stage is independently testable:
//!
//! ```text
//! Widget::render(area, &mut Paint)   // clipped writes, damage marked at write time
//!         |
//!     CellGrid + Damage              // desired state + what changed
//!         |
//!     diff(&back, &damage, &front)   // pure: Draw commands, no I/O, no mutation
//!         |
//!     Backend::render(&DrawCmd)      // escape bytes; front grid is promoted only on success
//! ```
//!
//! There is no second retained mechanism: the drawing entry points are
//! [`paint::Widget`] (immediate) and [`crate::ui`] (retained nodes), and both
//! paint through [`paint::Paint`] so the clip rules cannot diverge.

pub mod diff;
pub mod driver;
pub mod paint;
pub mod profile;

pub use diff::{Draw, DrawCmd, diff, promote, promote_scrolled};
pub use driver::{Driver, EscapeEmitter, Palette, iter_attrs, to_ct_color};
pub use paint::{Paint, Widget};
pub use profile::{
    CharsetStandard, ColorStandard, TerminalProfile, quantize_to_ansi16, quantize_to_indexed256,
};
