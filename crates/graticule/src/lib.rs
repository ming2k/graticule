//! Graticule: pure-native retained-mode terminal UI engine and OS TTY transport.
//!
//! Combines three cleanly separated layers under one cohesive entry point with
//! zero third-party terminal wrapper dependencies:
//!
//! - [`vt`] (`graticule-vt`): pure-Rust VT / ECMA-48 / DEC / xterm / Kitty wire
//!   protocol — zero-allocation outbound escape encoding ([`vt::csi`],
//!   [`vt::sgr`], [`vt::osc`], [`vt::mouse`]) and incremental inbound
//!   [`VtParser`] + [`Event`] model. Zero OS dependencies.
//! - [`graticule_canvas`] (re-exported at root): pure-memory cell grid, damage
//!   tracking, Flexbox layout, differential escape emitter, widgets, and
//!   retained `ui` scene graph. Zero OS dependencies; compiles cleanly with
//!   `default-features = false`.
//! - [`tty`] (`graticule-tty`, enabled by default `tty` feature): pure-native
//!   OS TTY host transport (`termios` / `TIOCGWINSZ` / `poll(2)` / `SIGWINCH`
//!   on Unix; Win32 Console API on Windows) providing [`TerminalSession`],
//!   [`InputReader`], [`ProbeHandle`], [`terminal_size`], and [`is_tty`].

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub use graticule_canvas::*;
pub use graticule_vt as vt;

#[cfg(feature = "tty")]
pub use graticule_tty as tty;

pub use graticule_vt::{
    DEFAULT_ESC_TIMEOUT, Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
    MAX_PASTE_BYTES, MAX_SEQUENCE_BYTES, MediaKeyCode, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind, MouseMode, ParsedItem, SgrColor, SgrEncoder, VtParser, event,
};

#[cfg(feature = "tty")]
pub use graticule_tty::{
    InputReader, KeyboardProbe, ProbeHandle, SessionConfig, TerminalSession, install_panic_hook,
    is_raw_mode_enabled, is_tty, push_disambiguate_escape_codes, rearm_active_mouse,
    restore_global, terminal_size,
};

#[cfg(feature = "tty")]
/// Construct a live [`Terminal`] whose viewport automatically tracks the host
/// TTY via [`terminal_size`] (falling back to `80x24` when detached).
pub fn live_terminal<W: std::io::Write, E: EscapeEmitter>(
    backend: Backend<W, E>,
) -> Terminal<W, E> {
    Terminal::with_size_query(backend, terminal_size)
}

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
