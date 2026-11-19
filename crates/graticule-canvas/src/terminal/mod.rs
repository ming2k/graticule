//! Terminal execution: the crossterm backend, the commit loop, and the headless
//! test terminal.
//!
//! This layer sits *above* `render`: it owns I/O and the draw/commit loop, and it
//! is the only place `crossterm` appears. Raw mode and the alternate screen are
//! deliberately not managed here — see `Terminal::writer`.

pub mod backend;
pub mod frame;

pub use backend::{Backend, Bce};
pub use frame::{CursorState, Frame, Terminal, TestTerminal};
