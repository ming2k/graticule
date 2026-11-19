//! Physical TTY host transport for Graticule.
//!
//! Owns inbound byte-stream decoding ([`VtParser`], [`InputReader`], [`Event`])
//! and opt-in terminal mode lifecycle ([`TerminalSession`], [`MouseMode`]).
//!
//! Kept deliberately independent of `graticule-canvas` so pure rendering/layout
//! crates never link OS tty polling or raw-mode drivers, while applications
//! get a cohesive, crossterm-free public API.

#![warn(missing_docs)]
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod event;
pub mod mouse;
pub mod parser;
pub mod reader;
pub mod session;
mod sys;

pub use event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};
pub use mouse::MouseMode;
pub use parser::{DEFAULT_ESC_TIMEOUT, MAX_PASTE_BYTES, MAX_SEQUENCE_BYTES, ParsedItem, VtParser};
pub use reader::{InputReader, KeyboardProbe, ProbeHandle};
pub use session::{
    SessionConfig, TerminalSession, push_disambiguate_escape_codes, rearm_active_mouse,
    restore_global, terminal_size,
};
