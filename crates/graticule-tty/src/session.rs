//! Opt-in physical terminal session lifecycle (raw mode, alternate screen,
//! mouse tracking, bracketed paste, focus reporting, cursor style, and
//! keyboard enhancement).
//!
//! Pure-native implementation using `graticule-vt` escape sequences and direct
//! OS raw-mode syscalls (`termios` on Unix, Win32 Console API on Windows).
//! Restores terminal state idempotently on [`Drop`] and via [`restore_global`]
//! so panic hooks and signal guards return the terminal to a clean cooked mode.

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use graticule_vt::csi::{
    CURSOR_HIDE, CURSOR_SHOW, CursorStyle, DISABLE_BRACKETED_PASTE, DISABLE_FOCUS_EVENTS,
    ENABLE_BRACKETED_PASTE, ENABLE_FOCUS_EVENTS, ENTER_ALTERNATE_SCREEN, LEAVE_ALTERNATE_SCREEN,
    POP_KITTY_KEYBOARD, PUSH_KITTY_DISAMBIGUATE,
};
use graticule_vt::mouse::MouseMode;
use graticule_vt::osc;
use graticule_vt::sgr::SGR_RESET;

use crate::reader::ProbeHandle;
use crate::sys;

static RAW_MODE_ENABLED: AtomicBool = AtomicBool::new(false);
static ALT_SCREEN_ENABLED: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE_ENABLED: AtomicBool = AtomicBool::new(false);
static FOCUS_EVENTS_ENABLED: AtomicBool = AtomicBool::new(false);
static CURSOR_STYLE_MODIFIED: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT_ENABLED: AtomicBool = AtomicBool::new(false);
static ACTIVE_MOUSE_MODE: AtomicU8 = AtomicU8::new(0);

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
    /// Enable terminal focus reporting (`?1004h`) on start.
    pub focus_events: bool,
    /// Optional hardware cursor shape (`DECSCUSR`) to apply while active.
    pub cursor_style: Option<CursorStyle>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            raw_mode: true,
            alternate_screen: true,
            hide_cursor: true,
            mouse_mode: MouseMode::Button,
            bracketed_paste: true,
            focus_events: false,
            cursor_style: None,
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
    /// Coalesces all mode-enable escape sequences into a single atomic write
    /// and never blocks on terminal queries.
    pub fn enter(config: SessionConfig) -> io::Result<Self> {
        if config.raw_mode {
            sys::enable_raw_mode()?;
            RAW_MODE_ENABLED.store(true, Ordering::Release);
        }

        let mut seq = Vec::with_capacity(96);
        if config.alternate_screen {
            seq.extend_from_slice(ENTER_ALTERNATE_SCREEN);
        }
        if config.hide_cursor {
            seq.extend_from_slice(CURSOR_HIDE);
        }
        if let Some(style) = config.cursor_style {
            seq.extend_from_slice(style.as_bytes());
        }
        if config.mouse_mode != MouseMode::None {
            seq.extend_from_slice(config.mouse_mode.enable_bytes().as_bytes());
        }
        if config.bracketed_paste {
            seq.extend_from_slice(ENABLE_BRACKETED_PASTE);
        }
        if config.focus_events {
            seq.extend_from_slice(ENABLE_FOCUS_EVENTS);
        }

        if !seq.is_empty() {
            let mut stdout = io::stdout();
            if let Err(err) = stdout.write_all(&seq).and_then(|()| stdout.flush()) {
                restore_global();
                return Err(err);
            }
        }

        if config.alternate_screen {
            ALT_SCREEN_ENABLED.store(true, Ordering::Release);
        }
        if config.cursor_style.is_some() {
            CURSOR_STYLE_MODIFIED.store(true, Ordering::Release);
        }
        if config.mouse_mode != MouseMode::None {
            ACTIVE_MOUSE_MODE.store(encode_mouse(config.mouse_mode), Ordering::Release);
        }
        if config.bracketed_paste {
            BRACKETED_PASTE_ENABLED.store(true, Ordering::Release);
        }
        if config.focus_events {
            FOCUS_EVENTS_ENABLED.store(true, Ordering::Release);
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
        let prev = decode_mouse(ACTIVE_MOUSE_MODE.swap(encode_mouse(mode), Ordering::AcqRel));
        let mut seq = Vec::with_capacity(48);
        if prev != MouseMode::None {
            seq.extend_from_slice(prev.disable_bytes().as_bytes());
        }
        if mode != MouseMode::None {
            seq.extend_from_slice(mode.enable_bytes().as_bytes());
        }
        if !seq.is_empty() {
            let mut stdout = io::stdout();
            stdout.write_all(&seq)?;
            stdout.flush()?;
        }
        self.config.mouse_mode = mode;
        Ok(())
    }

    /// Update the hardware cursor shape (`DECSCUSR`) at runtime and mark it for
    /// restoration to [`CursorStyle::DefaultUserShape`] on exit.
    pub fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.write_all(style.as_bytes())?;
        stdout.flush()?;
        CURSOR_STYLE_MODIFIED.store(true, Ordering::Release);
        self.config.cursor_style = Some(style);
        Ok(())
    }

    /// Set the terminal window title via sanitized OSC 2 (`\x1b]2;{title}\x07`).
    pub fn set_window_title(&self, title: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        osc::write_window_title(&mut stdout, title)?;
        stdout.flush()
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
    if stdout.write_all(PUSH_KITTY_DISAMBIGUATE).is_ok() && stdout.flush().is_ok() {
        KEYBOARD_ENHANCEMENT_ENABLED.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

/// Idempotent global terminal restore safe to invoke from panic hooks and
/// signal handlers.
///
/// Coalesces all teardown escape sequences (Kitty pop, focus off, bracketed
/// paste off, mouse off, cursor shape reset, SGR reset, leave alternate screen,
/// show cursor) into a single write before restoring cooked mode.
pub fn restore_global() {
    let mut seq = Vec::with_capacity(96);

    if KEYBOARD_ENHANCEMENT_ENABLED.swap(false, Ordering::AcqRel) {
        seq.extend_from_slice(POP_KITTY_KEYBOARD);
    }

    if FOCUS_EVENTS_ENABLED.swap(false, Ordering::AcqRel) {
        seq.extend_from_slice(DISABLE_FOCUS_EVENTS);
    }

    if BRACKETED_PASTE_ENABLED.swap(false, Ordering::AcqRel) {
        seq.extend_from_slice(DISABLE_BRACKETED_PASTE);
    }

    let mouse = decode_mouse(ACTIVE_MOUSE_MODE.swap(MOUSE_NONE, Ordering::AcqRel));
    if mouse != MouseMode::None {
        seq.extend_from_slice(mouse.disable_bytes().as_bytes());
    }

    if CURSOR_STYLE_MODIFIED.swap(false, Ordering::AcqRel) {
        seq.extend_from_slice(CursorStyle::DefaultUserShape.as_bytes());
    }

    seq.extend_from_slice(SGR_RESET);

    if ALT_SCREEN_ENABLED.swap(false, Ordering::AcqRel) {
        seq.extend_from_slice(LEAVE_ALTERNATE_SCREEN);
    }

    seq.extend_from_slice(CURSOR_SHOW);

    let mut stdout = io::stdout();
    let _ = stdout.write_all(&seq);
    let _ = stdout.flush();

    if RAW_MODE_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = sys::disable_raw_mode();
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
    sys::terminal_size()
}

/// Whether the process is connected to an interactive terminal.
pub fn is_tty() -> bool {
    sys::is_tty()
}

/// Whether Graticule currently holds the terminal in raw mode.
pub fn is_raw_mode_enabled() -> bool {
    sys::is_raw_mode_enabled()
}

/// Install a process-wide panic hook that restores the terminal via
/// [`restore_global`] before delegating to the previously installed panic hook.
///
/// Idempotent: calling more than once is a no-op so hooks never chain
/// recursively.
pub fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_global();
            prev(info);
        }));
    });
}
