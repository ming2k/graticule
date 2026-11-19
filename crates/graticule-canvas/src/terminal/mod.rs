//! Terminal execution: the pure-VT backend, the commit loop, and the headless
//! test terminal.
//!
//! This layer sits *above* `render`: it encodes [`crate::render::DrawCmd`]s
//! into minimal VT/ECMA-48 escape sequences via `graticule-vt` and owns the
//! draw/commit loop. Raw mode, alternate screen, and OS TTY queries are
//! deliberately managed outside this crate by `graticule-tty` (or injected via
//! [`Terminal::with_size_query`]).

pub mod backend;
pub mod frame;

pub use backend::{Backend, Bce};
pub use frame::{CursorState, Frame, Terminal, TestTerminal};
