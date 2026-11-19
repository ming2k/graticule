//! Graticule: retained-mode terminal UI engine and physical TTY host transport.
//!
//! Combines two cleanly separated layers under one cohesive entry point:
//!
//! - [`graticule_canvas`]: pure-memory cell grid, damage tracking, Flexbox layout,
//!   differential escape emitter, widgets, and retained `ui` scene graph.
//!   Always available; disables all OS session/tty dependencies when built
//!   with `default-features = false`.
//! - [`tty`] (enabled by the default `tty` feature): incremental, timeout-aware
//!   VT input parser, [`Event`] model, [`MouseMode`], [`InputReader`], and
//!   opt-in [`TerminalSession`] guard.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub use graticule_canvas::*;

#[cfg(feature = "tty")]
pub use graticule_tty as tty;

#[cfg(feature = "tty")]
pub use graticule_tty::event;

#[cfg(feature = "tty")]
pub use graticule_tty::{
    DEFAULT_ESC_TIMEOUT, Event, InputReader, KeyCode, KeyEvent, KeyEventKind, KeyEventState,
    KeyModifiers, KeyboardProbe, MAX_PASTE_BYTES, MAX_SEQUENCE_BYTES, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind, MouseMode, ParsedItem, ProbeHandle,
    SessionConfig, TerminalSession, VtParser, push_disambiguate_escape_codes, rearm_active_mouse,
    restore_global, terminal_size,
};

#[cfg(feature = "tty")]
/// Derive a [`SessionConfig`] aligned with a detected [`TerminalProfile`].
pub fn session_config_for_profile(
    profile: &TerminalProfile,
    mouse_mode: MouseMode,
) -> SessionConfig {
    SessionConfig {
        mouse_mode: if profile.supports_mouse {
            mouse_mode
        } else {
            MouseMode::None
        },
        ..SessionConfig::default()
    }
}
