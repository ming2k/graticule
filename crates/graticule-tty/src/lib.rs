//! Pure-native OS TTY host transport for Graticule.
//!
//! Owns physical terminal lifecycle ([`TerminalSession`], [`SessionConfig`],
//! [`terminal_size`], [`is_tty`]) and the dedicated single-reader input loop
//! ([`InputReader`], [`ProbeHandle`]). Re-exports the pure wire-protocol types
//! ([`Event`], [`VtParser`], [`MouseMode`], [`CursorStyle`]) from `graticule-vt`.
//!
//! Zero third-party terminal wrapper crates: Unix speaks directly to `termios`,
//! `TIOCGWINSZ`, `poll(2)`, and `SIGWINCH` via `libc` + `signal-hook`; Windows
//! speaks directly to the Win32 Console API via `windows-sys`.

#![warn(missing_docs)]
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod reader;
pub mod session;
mod sys;

pub use graticule_vt::{
    CursorStyle, DEFAULT_ESC_TIMEOUT, Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState,
    KeyModifiers, MAX_PASTE_BYTES, MAX_SEQUENCE_BYTES, MediaKeyCode, ModifierKeyCode, MouseButton,
    MouseEvent, MouseEventKind, MouseMode, ParsedItem, VtParser, event, mouse, parser,
};
pub use reader::{InputReader, KeyboardProbe, ProbeHandle};
pub use session::{
    SessionConfig, TerminalSession, install_panic_hook, is_raw_mode_enabled, is_tty,
    push_disambiguate_escape_codes, rearm_active_mouse, restore_global, terminal_size,
};
