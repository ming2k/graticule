//! Opt-in physical terminal session lifecycle (raw mode, alternate screen,
//! mouse tracking, bracketed paste, and keyboard enhancement).
//!
//! Restores terminal state idempotently on [`Drop`] and via [`restore_global`]
//! so panic hooks and signal guards return the terminal to a clean cooked mode.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};

use crate::mouse::MouseMode;
use crate::reader::ProbeHandle;

static RAW_MODE_ENABLED: AtomicBool = AtomicBool::new(false);
static ALT_SCREEN_ENABLED: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE_ENABLED: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT_ENABLED: AtomicBool = AtomicBool::new(false);
static ACTIVE_MOUSE_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

const MOUSE_NONE: u8 = 0;
const MOUSE_BUTTON: u8 = 1;
const MOUSE_ALL_MOTION: u8 = 2;

const fn encode_mouse(mode: MouseMode) -> u8 {
    match mode {
        MouseMode::None => MOUSE_NONE,
        MouseMode::Button => MOUSE_BUTTON,
        MouseMode::AllMotion => MOUSE_ALL_MOTION,
    }
}

const fn decode_mouse(raw: u8) -> MouseMode {
    match raw {
        MOUSE_BUTTON => MouseMode::Button,
        MOUSE_ALL_MOTION => MouseMode::AllMotion,
        _ => MouseMode::None,
    }
}

/// Configuration for [`TerminalSession::enter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// Enter raw mode on start and restore cooked mode on exit.
    pub raw_mode: bool,
    /// Enter the alternate screen buffer on start and leave on exit.
    pub alternate_screen: bool,
    /// Hide the hardware cursor initially (renderers manage cursor visibility).
    pub hide_cursor: bool,
    /// Mouse-tracking mode to negotiate.
    pub mouse_mode: MouseMode,
    /// Enable bracketed paste (`?2004h`) on start.
    pub bracketed_paste: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            raw_mode: true,
            alternate_screen: true,
            hide_cursor: true,
            mouse_mode: MouseMode::Button,
            bracketed_paste: true,
        }
    }
}

/// RAII guard owning the terminal's interactive mode configuration.
///
/// Entering never performs blocking terminal queries (`ESC[?u` / `ESC[c`) so
/// the caller can paint its first frame immediately. Call
/// [`Self::negotiate_keyboard_enhancement`] afterwards off the critical path.
pub struct TerminalSession {
    config: SessionConfig,
    restored: bool,
}

impl TerminalSession {
    /// Enter interactive terminal modes according to `config`.
    ///
    /// Never blocks on terminal queries.
    pub fn enter(config: SessionConfig) -> io::Result<Self> {
        if config.raw_mode {
            terminal::enable_raw_mode()?;
            RAW_MODE_ENABLED.store(true, Ordering::Release);
        }

        let mut stdout = io::stdout();
        if config.alternate_screen {
            if let Err(err) = execute!(stdout, EnterAlternateScreen) {
                restore_global();
                return Err(err);
            }
            ALT_SCREEN_ENABLED.store(true, Ordering::Release);
        }

        if config.hide_cursor {
            let _ = execute!(stdout, cursor::Hide);
        }

        if config.mouse_mode != MouseMode::None {
            let bytes = config.mouse_mode.enable_bytes();
            if stdout.write_all(bytes.as_bytes()).is_ok() && stdout.flush().is_ok() {
                ACTIVE_MOUSE_MODE.store(encode_mouse(config.mouse_mode), Ordering::Release);
            }
        }

        if config.bracketed_paste
            && stdout.write_all(b"\x1b[?2004h").is_ok()
            && stdout.flush().is_ok()
        {
            BRACKETED_PASTE_ENABLED.store(true, Ordering::Release);
        }

        Ok(Self {
            config,
            restored: false,
        })
    }

    /// Re-arm the configured mouse mode (e.g. after a terminal resize in
    /// multiplexers that reset tracking state).
    pub fn rearm_mouse(&self) {
        if self.config.mouse_mode != MouseMode::None {
            let mut stdout = io::stdout();
            let _ = stdout.write_all(self.config.mouse_mode.enable_bytes().as_bytes());
            let _ = stdout.flush();
        }
    }

    /// Update the active mouse tracking mode at runtime.
    pub fn set_mouse_mode(&mut self, mode: MouseMode) -> io::Result<()> {
        let mut stdout = io::stdout();
        let prev = decode_mouse(ACTIVE_MOUSE_MODE.swap(encode_mouse(mode), Ordering::AcqRel));
        if prev != MouseMode::None {
            stdout.write_all(prev.disable_bytes().as_bytes())?;
        }
        if mode != MouseMode::None {
            stdout.write_all(mode.enable_bytes().as_bytes())?;
        }
        stdout.flush()?;
        self.config.mouse_mode = mode;
        Ok(())
    }

    /// Probe and push Kitty `DISAMBIGUATE_ESCAPE_CODES` (`\x1b[>1u`) off the
    /// first-frame path via `probe`.
    pub fn negotiate_keyboard_enhancement(probe: &ProbeHandle, timeout: Duration) -> bool {
        if probe.probe_keyboard_enhancement(timeout) {
            push_disambiguate_escape_codes()
        } else {
            false
        }
    }

    /// Restore the terminal immediately. Idempotent; also runs on [`Drop`].
    pub fn restore(&mut self) {
        if !self.restored {
            self.restored = true;
            restore_global();
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Push Kitty `DISAMBIGUATE_ESCAPE_CODES` (`\x1b[>1u`) directly and record it
/// for cleanup on restore.
pub fn push_disambiguate_escape_codes() -> bool {
    let mut stdout = io::stdout();
    if stdout.write_all(b"\x1b[>1u").is_ok() && stdout.flush().is_ok() {
        KEYBOARD_ENHANCEMENT_ENABLED.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

/// Idempotent global terminal restore safe to invoke from panic hooks and
/// signal handlers.
pub fn restore_global() {
    let mut stdout = io::stdout();

    if KEYBOARD_ENHANCEMENT_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = stdout.write_all(b"\x1b[<u");
    }

    if BRACKETED_PASTE_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = stdout.write_all(b"\x1b[?2004l");
    }

    let mouse = decode_mouse(ACTIVE_MOUSE_MODE.swap(MOUSE_NONE, Ordering::AcqRel));
    if mouse != MouseMode::None {
        let _ = stdout.write_all(mouse.disable_bytes().as_bytes());
    }

    if ALT_SCREEN_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = execute!(stdout, LeaveAlternateScreen);
    }

    let _ = execute!(stdout, cursor::Show);
    let _ = stdout.flush();

    if RAW_MODE_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Re-arm whichever [`MouseMode`] is currently active on the global session
/// (e.g. after a terminal resize in multiplexers that reset tracking state).
pub fn rearm_active_mouse() {
    let mode = decode_mouse(ACTIVE_MOUSE_MODE.load(Ordering::Acquire));
    if mode != MouseMode::None {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(mode.enable_bytes().as_bytes());
        let _ = stdout.flush();
    }
}

/// Query the current terminal size in `(columns, rows)`.
pub fn terminal_size() -> io::Result<(u16, u16)> {
    crate::sys::terminal_size()
}
