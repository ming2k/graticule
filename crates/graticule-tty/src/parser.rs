//! Incremental, timeout-aware VT / ECMA-48 / Kitty / SGR input parser.
//!
//! Pure state machine: consumes arbitrary byte chunks alongside an `Instant`
//! clock and emits [`ParsedItem`]s without performing syscalls or allocations
//! per idle tick.
//!
//! Key guarantees over naive `read_count == buffer_len` heuristics:
//! - Bare `ESC` (`0x1b`) waits up to [`DEFAULT_ESC_TIMEOUT`] (20 ms default)
//!   before resolving to `KeyCode::Esc`, so an SGR mouse report or CSI sequence
//!   split right after `\x1b` across `read(2)` calls never leaks `[<35;...M`
//!   as printable text or fires a phantom `Esc`.
//! - Double `ESC` (`\x1b\x1b`) flushes the first `Esc` immediately and retains
//!   the second `\x1b` as a fresh pending escape, so `\x1b\x1b[A` decodes as
//!   `Esc` + `Up` instead of swallowing the second `\x1b`.
//! - Unknown `CSI` sequences terminate cleanly on their ECMA-48 final byte
//!   (`0x40..=0x7e`) instead of wedging subsequent input, and malformed bytes
//!   abort back to idle while re-processing any valid leading byte (such as a
//!   new `\x1b`).
//! - Pending CSI and bracketed-paste buffers are strictly bounded.

use std::time::{Duration, Instant};

use crate::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};

/// Default disambiguation deadline between a bare `Esc` press and the start
/// of an escape sequence (`ESC [`, `ESC O`, `Alt+key`).
pub const DEFAULT_ESC_TIMEOUT: Duration = Duration::from_millis(20);

/// Maximum bytes retained for a single CSI / SS3 / UTF-8 sequence before
/// aborting back to idle.
pub const MAX_SEQUENCE_BYTES: usize = 256;

/// Maximum bytes retained for a single bracketed paste (`ESC[200~ ... ESC[201~`).
pub const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;

/// Output item produced by [`VtParser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedItem {
    /// A user-facing terminal event.
    Event(Event),
    /// Response to `ESC [ ? u` (Kitty keyboard enhancement query).
    KeyboardEnhancementFlags(u16),
    /// Response to `ESC [ c` (Primary Device Attributes).
    PrimaryDeviceAttributes,
}

#[derive(Debug)]
enum State {
    Idle,
    /// Saw `0x1b`, waiting for sequence continuation or timeout.
    Esc {
        since: Instant,
    },
    /// Saw `ESC` followed by a multi-byte UTF-8 lead byte (`Alt+<char>`).
    AltUtf8 {
        buf: [u8; 4],
        len: u8,
        expected: u8,
    },
    /// Saw `ESC O` (SS3).
    Ss3,
    /// Saw `ESC [` (CSI), accumulating bytes after `[` up to and including the final byte.
    Csi(Vec<u8>),
    /// Saw `ESC [ [` (Linux VT function keys `ESC [ [ A..E`).
    CsiDoubleBracket,
    /// Saw `ESC [ M` (X10 normal mouse: 3 raw bytes following `M`).
    X10Mouse {
        buf: [u8; 3],
        len: u8,
    },
    /// Accumulating multi-byte UTF-8 character from idle.
    Utf8 {
        buf: [u8; 4],
        len: u8,
        expected: u8,
    },
    /// Accumulating payload inside `ESC [ 200 ~ ... ESC [ 201 ~`.
    BracketedPaste(Vec<u8>),
}

/// Incremental VT byte-stream parser.
#[derive(Debug)]
pub struct VtParser {
    state: State,
    esc_timeout: Duration,
    max_paste_bytes: usize,
    raw_mode: bool,
}

impl Default for VtParser {
    fn default() -> Self {
        Self::new()
    }
}

impl VtParser {
    /// Construct a parser with the default 20 ms ESC timeout and raw-mode newline rules.
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            esc_timeout: DEFAULT_ESC_TIMEOUT,
            max_paste_bytes: MAX_PASTE_BYTES,
            raw_mode: true,
        }
    }

    /// Override the bare-ESC disambiguation timeout.
    pub fn with_esc_timeout(mut self, timeout: Duration) -> Self {
        self.esc_timeout = timeout;
        self
    }

    /// Override the maximum bracketed paste buffer size in bytes.
    pub fn with_max_paste_bytes(mut self, max_bytes: usize) -> Self {
        self.max_paste_bytes = max_bytes;
        self
    }

    /// Configure whether raw mode is active (when true, `\n` decodes as `Ctrl+j`;
    /// when false, `\n` decodes as `Enter`).
    pub fn set_raw_mode(&mut self, raw_mode: bool) {
        self.raw_mode = raw_mode;
    }

    /// Earliest instant at which a pending bare `ESC` should flush as `KeyCode::Esc`.
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.state {
            State::Esc { since } => Some(since + self.esc_timeout),
            _ => None,
        }
    }

    /// Whether the parser is holding an incomplete sequence.
    pub fn has_pending(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Feed a chunk of bytes read at `now`, pushing decoded items to `out`.
    pub fn feed(&mut self, bytes: &[u8], now: Instant, out: &mut Vec<ParsedItem>) {
        for &byte in bytes {
            self.step(byte, now, out);
        }
    }

    /// Flush a pending bare `ESC` if its deadline has elapsed by `now`.
    pub fn flush_timeout(&mut self, now: Instant, out: &mut Vec<ParsedItem>) {
        if let State::Esc { since } = self.state
            && now.saturating_duration_since(since) >= self.esc_timeout
        {
            self.state = State::Idle;
            out.push(ParsedItem::Event(Event::Key(KeyCode::Esc.into())));
        }
    }

    /// Reset internal state back to idle, discarding any half-formed sequence.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    fn step(&mut self, byte: u8, now: Instant, out: &mut Vec<ParsedItem>) {
        // Take ownership of current state so transitions cannot accidentally leave stale state.
        let cur = std::mem::replace(&mut self.state, State::Idle);
        match cur {
            State::Idle => self.step_idle(byte, now, out),
            State::Esc { since: _ } => self.step_esc(byte, now, out),
            State::AltUtf8 {
                mut buf,
                len,
                expected,
            } => {
                if (byte & 0b1100_0000) == 0b1000_0000 && (len as usize) < buf.len() {
                    buf[len as usize] = byte;
                    let new_len = len + 1;
                    if new_len == expected {
                        if let Ok(s) = std::str::from_utf8(&buf[..new_len as usize])
                            && let Some(ch) = s.chars().next()
                        {
                            out.push(ParsedItem::Event(Event::Key(char_key_with_mods(
                                ch,
                                KeyModifiers::ALT,
                            ))));
                        }
                    } else {
                        self.state = State::AltUtf8 {
                            buf,
                            len: new_len,
                            expected,
                        };
                    }
                } else {
                    // Broken UTF-8 after ESC: discard broken prefix and re-process byte.
                    self.step_idle(byte, now, out);
                }
            }
            State::Ss3 => self.step_ss3(byte, now, out),
            State::CsiDoubleBracket => {
                if let b'A'..=b'E' = byte {
                    let f = 1 + (byte - b'A');
                    out.push(ParsedItem::Event(Event::Key(KeyCode::F(f).into())));
                } else {
                    self.step_idle(byte, now, out);
                }
            }
            State::X10Mouse { mut buf, len } => {
                buf[len as usize] = byte;
                let new_len = len + 1;
                if new_len == 3 {
                    if let Some(mouse) = decode_x10_mouse(buf) {
                        out.push(ParsedItem::Event(Event::Mouse(mouse)));
                    }
                } else {
                    self.state = State::X10Mouse { buf, len: new_len };
                }
            }
            State::Csi(mut buf) => self.step_csi(&mut buf, byte, now, out),
            State::Utf8 {
                mut buf,
                len,
                expected,
            } => {
                if (byte & 0b1100_0000) == 0b1000_0000 && (len as usize) < buf.len() {
                    buf[len as usize] = byte;
                    let new_len = len + 1;
                    if new_len == expected {
                        if let Ok(s) = std::str::from_utf8(&buf[..new_len as usize])
                            && let Some(ch) = s.chars().next()
                        {
                            out.push(ParsedItem::Event(Event::Key(char_key_with_mods(
                                ch,
                                KeyModifiers::NONE,
                            ))));
                        }
                    } else {
                        self.state = State::Utf8 {
                            buf,
                            len: new_len,
                            expected,
                        };
                    }
                } else {
                    // Broken UTF-8 continuation: discard invalid prefix and re-process byte.
                    self.step_idle(byte, now, out);
                }
            }
            State::BracketedPaste(mut buf) => {
                const END_MARKER: &[u8] = b"\x1b[201~";
                if buf.len() >= self.max_paste_bytes {
                    // Bound memory: flush what we have so far and discard overflow until idle.
                    let paste = String::from_utf8_lossy(&buf).into_owned();
                    out.push(ParsedItem::Event(Event::Paste(paste)));
                    return;
                }
                buf.push(byte);
                if buf.ends_with(END_MARKER) {
                    let payload_end = buf.len() - END_MARKER.len();
                    let paste = String::from_utf8_lossy(&buf[..payload_end]).into_owned();
                    out.push(ParsedItem::Event(Event::Paste(paste)));
                } else {
                    self.state = State::BracketedPaste(buf);
                }
            }
        }
    }

    fn step_idle(&mut self, byte: u8, now: Instant, out: &mut Vec<ParsedItem>) {
        match byte {
            b'\x1b' => {
                self.state = State::Esc { since: now };
            }
            b'\r' => out.push(ParsedItem::Event(Event::Key(KeyCode::Enter.into()))),
            b'\n' => {
                if self.raw_mode {
                    out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                        KeyCode::Char('j'),
                        KeyModifiers::CONTROL,
                    ))));
                } else {
                    out.push(ParsedItem::Event(Event::Key(KeyCode::Enter.into())));
                }
            }
            b'\t' => out.push(ParsedItem::Event(Event::Key(KeyCode::Tab.into()))),
            b'\x7f' => out.push(ParsedItem::Event(Event::Key(KeyCode::Backspace.into()))),
            b'\0' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::CONTROL,
            )))),
            c @ b'\x01'..=b'\x1a' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char((c - 0x01 + b'a') as char),
                KeyModifiers::CONTROL,
            )))),
            c @ b'\x1c'..=b'\x1f' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char((c - 0x1c + b'4') as char),
                KeyModifiers::CONTROL,
            )))),
            0x20..=0x7e => out.push(ParsedItem::Event(Event::Key(char_key_with_mods(
                byte as char,
                KeyModifiers::NONE,
            )))),
            b => {
                if let Some(expected) = utf8_expected_len(b) {
                    let mut buf = [0u8; 4];
                    buf[0] = b;
                    self.state = State::Utf8 {
                        buf,
                        len: 1,
                        expected,
                    };
                }
            }
        }
    }

    fn step_esc(&mut self, byte: u8, now: Instant, out: &mut Vec<ParsedItem>) {
        match byte {
            b'[' => {
                self.state = State::Csi(Vec::with_capacity(32));
            }
            b'O' => {
                self.state = State::Ss3;
            }
            b'\x1b' => {
                // Flush the first Esc immediately, and keep the second 0x1b pending
                // so either a double-Esc or Esc followed by CSI/SS3 works cleanly.
                out.push(ParsedItem::Event(Event::Key(KeyCode::Esc.into())));
                self.state = State::Esc { since: now };
            }
            b'\r' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::ALT,
            )))),
            b'\n' => {
                let ev = if self.raw_mode {
                    KeyEvent::new(
                        KeyCode::Char('j'),
                        KeyModifiers::ALT | KeyModifiers::CONTROL,
                    )
                } else {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
                };
                out.push(ParsedItem::Event(Event::Key(ev)));
            }
            b'\t' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::ALT,
            )))),
            b'\x7f' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::ALT,
            )))),
            b'\0' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            )))),
            c @ b'\x01'..=b'\x1a' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char((c - 0x01 + b'a') as char),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            )))),
            c @ b'\x1c'..=b'\x1f' => out.push(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Char((c - 0x1c + b'4') as char),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            )))),
            0x20..=0x7e => out.push(ParsedItem::Event(Event::Key(char_key_with_mods(
                byte as char,
                KeyModifiers::ALT,
            )))),
            b => {
                if let Some(expected) = utf8_expected_len(b) {
                    let mut buf = [0u8; 4];
                    buf[0] = b;
                    self.state = State::AltUtf8 {
                        buf,
                        len: 1,
                        expected,
                    };
                }
            }
        }
    }

    fn step_ss3(&mut self, byte: u8, now: Instant, out: &mut Vec<ParsedItem>) {
        let ev = match byte {
            b'A' => Some(KeyCode::Up.into()),
            b'B' => Some(KeyCode::Down.into()),
            b'C' => Some(KeyCode::Right.into()),
            b'D' => Some(KeyCode::Left.into()),
            b'H' => Some(KeyCode::Home.into()),
            b'F' => Some(KeyCode::End.into()),
            b'P'..=b'S' => Some(KeyCode::F(1 + (byte - b'P')).into()),
            // rxvt / urxvt Ctrl+Arrows
            b'a' => Some(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            b'b' => Some(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL)),
            b'c' => Some(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)),
            b'd' => Some(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            _ => None,
        };
        if let Some(key) = ev {
            out.push(ParsedItem::Event(Event::Key(key)));
        } else {
            self.step_idle(byte, now, out);
        }
    }

    fn step_csi(&mut self, buf: &mut Vec<u8>, byte: u8, now: Instant, out: &mut Vec<ParsedItem>) {
        if buf.is_empty() {
            if byte == b'[' {
                self.state = State::CsiDoubleBracket;
                return;
            }
            if byte == b'M' {
                self.state = State::X10Mouse {
                    buf: [0; 3],
                    len: 0,
                };
                return;
            }
        }

        // ECMA-48 CSI parameter/intermediate/private bytes: 0x20..=0x3F
        if (0x20..=0x3f).contains(&byte) {
            if buf.len() >= MAX_SEQUENCE_BYTES {
                // Drop oversized malformed CSI.
                return;
            }
            buf.push(byte);
            self.state = State::Csi(std::mem::take(buf));
            return;
        }

        // ECMA-48 CSI final byte: 0x40..=0x7E
        if (0x40..=0x7e).contains(&byte) {
            buf.push(byte);
            if buf.as_slice() == b"200~" {
                self.state = State::BracketedPaste(Vec::with_capacity(256));
                return;
            }
            if let Some(item) = decode_csi(buf) {
                out.push(item);
            }
            return;
        }

        // Unexpected byte inside CSI (e.g. control byte or new ESC): abort CSI
        // and re-process the byte from Idle so valid input is never swallowed.
        self.step_idle(byte, now, out);
    }
}

fn utf8_expected_len(lead: u8) -> Option<u8> {
    match lead {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn char_key_with_mods(ch: char, mut modifiers: KeyModifiers) -> KeyEvent {
    if ch.is_uppercase() {
        modifiers |= KeyModifiers::SHIFT;
    }
    KeyEvent::new(KeyCode::Char(ch), modifiers)
}

fn decode_csi(buf: &[u8]) -> Option<ParsedItem> {
    let (&final_byte, body) = buf.split_last()?;

    // SGR-1006 mouse: `ESC [ < Cb ; Cx ; Cy M/m` -> body starts with b'<'
    if let Some(sgr_body) = body.strip_prefix(b"<") {
        if final_byte == b'M' || final_byte == b'm' {
            return decode_sgr_mouse(sgr_body, final_byte == b'm')
                .map(|m| ParsedItem::Event(Event::Mouse(m)));
        }
        return None;
    }

    // Private `?` responses (Kitty keyboard enhancement query & DA1)
    if let Some(priv_body) = body.strip_prefix(b"?") {
        if final_byte == b'u' {
            let flags = std::str::from_utf8(priv_body).ok()?.parse::<u16>().ok()?;
            return Some(ParsedItem::KeyboardEnhancementFlags(flags));
        }
        if final_byte == b'c' {
            return Some(ParsedItem::PrimaryDeviceAttributes);
        }
        return None;
    }

    // Simple 1-byte CSI sequences (body empty)
    if body.is_empty() {
        return match final_byte {
            b'A' => Some(ParsedItem::Event(Event::Key(KeyCode::Up.into()))),
            b'B' => Some(ParsedItem::Event(Event::Key(KeyCode::Down.into()))),
            b'C' => Some(ParsedItem::Event(Event::Key(KeyCode::Right.into()))),
            b'D' => Some(ParsedItem::Event(Event::Key(KeyCode::Left.into()))),
            b'H' => Some(ParsedItem::Event(Event::Key(KeyCode::Home.into()))),
            b'F' => Some(ParsedItem::Event(Event::Key(KeyCode::End.into()))),
            b'Z' => Some(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT,
            )))),
            b'I' => Some(ParsedItem::Event(Event::FocusGained)),
            b'O' => Some(ParsedItem::Event(Event::FocusLost)),
            b'P' => Some(ParsedItem::Event(Event::Key(KeyCode::F(1).into()))),
            b'Q' => Some(ParsedItem::Event(Event::Key(KeyCode::F(2).into()))),
            b'S' => Some(ParsedItem::Event(Event::Key(KeyCode::F(4).into()))),
            // rxvt / urxvt Shift+Arrows
            b'a' => Some(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Up,
                KeyModifiers::SHIFT,
            )))),
            b'b' => Some(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::SHIFT,
            )))),
            b'c' => Some(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::SHIFT,
            )))),
            b'd' => Some(ParsedItem::Event(Event::Key(KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::SHIFT,
            )))),
            _ => None,
        };
    }

    let body_str = std::str::from_utf8(body).ok()?;

    // Reject other private prefixes (`>`, `=`) cleanly without misinterpreting them as keys.
    if body_str.starts_with('>') || body_str.starts_with('=') {
        return None;
    }

    match final_byte {
        b'u' => decode_csi_u(body_str).map(|k| ParsedItem::Event(Event::Key(k))),
        b'~' => decode_tilde_key(body_str).map(|k| ParsedItem::Event(Event::Key(k))),
        b'M' => decode_rxvt_mouse(body_str).map(|m| ParsedItem::Event(Event::Mouse(m))),
        b'A' | b'B' | b'C' | b'D' | b'F' | b'H' | b'P' | b'Q' | b'R' | b'S' => {
            decode_modified_cursor_or_fn(body_str, final_byte)
                .map(|k| ParsedItem::Event(Event::Key(k)))
        }
        _ => None,
    }
}

fn parse_mods_and_kind(param: Option<&str>) -> (KeyModifiers, KeyEventKind, KeyEventState) {
    let Some(s) = param else {
        return (KeyModifiers::NONE, KeyEventKind::Press, KeyEventState::NONE);
    };
    if s.is_empty() {
        return (KeyModifiers::NONE, KeyEventKind::Press, KeyEventState::NONE);
    }
    let mut parts = s.split(':');
    let mod_mask = parts.next().and_then(|p| p.parse::<u8>().ok()).unwrap_or(1);
    let kind_code = parts.next().and_then(|p| p.parse::<u8>().ok()).unwrap_or(1);
    (
        decode_modifier_mask(mod_mask),
        decode_key_event_kind(kind_code),
        decode_modifier_state(mod_mask),
    )
}

fn decode_modifier_mask(mask_plus_one: u8) -> KeyModifiers {
    let m = mask_plus_one.saturating_sub(1);
    let mut mods = KeyModifiers::NONE;
    if m & 1 != 0 {
        mods |= KeyModifiers::SHIFT;
    }
    if m & 2 != 0 {
        mods |= KeyModifiers::ALT;
    }
    if m & 4 != 0 {
        mods |= KeyModifiers::CONTROL;
    }
    if m & 8 != 0 {
        mods |= KeyModifiers::SUPER;
    }
    if m & 16 != 0 {
        mods |= KeyModifiers::HYPER;
    }
    if m & 32 != 0 {
        mods |= KeyModifiers::META;
    }
    mods
}

fn decode_modifier_state(mask_plus_one: u8) -> KeyEventState {
    let m = mask_plus_one.saturating_sub(1);
    let mut state = KeyEventState::NONE;
    if m & 64 != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if m & 128 != 0 {
        state |= KeyEventState::NUM_LOCK;
    }
    state
}

fn decode_key_event_kind(code: u8) -> KeyEventKind {
    match code {
        2 => KeyEventKind::Repeat,
        3 => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    }
}

fn decode_csi_u(body: &str) -> Option<KeyEvent> {
    let mut params = body.split(';');
    let mut codepoints = params.next()?.split(':');
    let codepoint = codepoints.next()?.parse::<u32>().ok()?;
    let (mut modifiers, kind, state_from_mods) = parse_mods_and_kind(params.next());

    let (mut keycode, state_from_code) = if let Some(special) = kitty_functional_key(codepoint) {
        special
    } else {
        let ch = char::from_u32(codepoint)?;
        let code = match ch {
            '\x1b' => KeyCode::Esc,
            '\r' => KeyCode::Enter,
            '\t' => {
                if modifiers.contains(KeyModifiers::SHIFT) {
                    KeyCode::BackTab
                } else {
                    KeyCode::Tab
                }
            }
            '\x7f' => KeyCode::Backspace,
            _ => KeyCode::Char(ch),
        };
        (code, KeyEventState::NONE)
    };

    if let KeyCode::Modifier(m) = keycode {
        match m {
            ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => {
                modifiers.insert(KeyModifiers::ALT)
            }
            ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => {
                modifiers.insert(KeyModifiers::CONTROL)
            }
            ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => {
                modifiers.insert(KeyModifiers::SHIFT)
            }
            ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => {
                modifiers.insert(KeyModifiers::SUPER)
            }
            ModifierKeyCode::LeftHyper | ModifierKeyCode::RightHyper => {
                modifiers.insert(KeyModifiers::HYPER)
            }
            ModifierKeyCode::LeftMeta | ModifierKeyCode::RightMeta => {
                modifiers.insert(KeyModifiers::META)
            }
            _ => {}
        }
    }

    // Only fold shifted alternate key into the character and drop SHIFT when
    // Shift is the *sole* modifier (e.g. `Shift+1 -> '!'`). When combined with
    // Ctrl/Alt/Super (e.g. `Ctrl+Shift+P` reported as `112:80;6u`), preserving
    // `SHIFT` is mandatory so keymaps distinguish `Ctrl+Shift+P` from `Ctrl+P`.
    let only_shift = modifiers == KeyModifiers::SHIFT;
    if only_shift
        && let Some(shifted) = codepoints
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok())
            .and_then(char::from_u32)
    {
        keycode = KeyCode::Char(shifted);
        modifiers.remove(KeyModifiers::SHIFT);
    }

    Some(KeyEvent::new_with_state(
        keycode,
        modifiers,
        kind,
        state_from_code | state_from_mods,
    ))
}

fn decode_tilde_key(body: &str) -> Option<KeyEvent> {
    let mut params = body.split(';');
    let first = params.next()?.parse::<u16>().ok()?;

    // xterm modifyOtherKeys: `CSI 27 ; <mods> ; <codepoint> ~`
    if first == 27 {
        let mod_param = params.next();
        if let Some(cp_str) = params.next() {
            let (modifiers, kind, state) = parse_mods_and_kind(mod_param);
            let cp = cp_str.parse::<u32>().ok()?;
            let ch = char::from_u32(cp)?;
            let code = match ch {
                '\x1b' => KeyCode::Esc,
                '\r' => KeyCode::Enter,
                '\t' => {
                    if modifiers.contains(KeyModifiers::SHIFT) {
                        KeyCode::BackTab
                    } else {
                        KeyCode::Tab
                    }
                }
                '\x7f' => KeyCode::Backspace,
                _ => KeyCode::Char(ch),
            };
            return Some(KeyEvent::new_with_state(code, modifiers, kind, state));
        }
    }

    let (modifiers, kind, state) = parse_mods_and_kind(params.next());
    let keycode = match first {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        v @ 11..=15 => KeyCode::F((v - 10) as u8),
        v @ 17..=21 => KeyCode::F((v - 11) as u8),
        v @ 23..=26 => KeyCode::F((v - 12) as u8),
        v @ 28..=29 => KeyCode::F((v - 15) as u8),
        v @ 31..=34 => KeyCode::F((v - 17) as u8),
        _ => return None,
    };

    Some(KeyEvent::new_with_state(keycode, modifiers, kind, state))
}

fn decode_modified_cursor_or_fn(body: &str, final_byte: u8) -> Option<KeyEvent> {
    let mut params = body.split(';');
    let first = params.next();
    let second = params.next();

    // `CSI <row> ; <col> R` is a Cursor Position Report (CPR), whereas
    // `CSI 1 ; <mods> R` is modified F3. Reject any `R` whose first parameter
    // is not `"1"` (or that has no modifier parameter) so CPR never masquerades
    // as F3.
    if final_byte == b'R' && (first != Some("1") || second.is_none()) {
        return None;
    }

    let (modifiers, kind, state) = if second.is_some() {
        parse_mods_and_kind(second)
    } else if first.is_some_and(|f| f != "1" && !f.is_empty()) {
        // Legacy terminals that omit the leading `1;` (e.g. `CSI 5 A` for Ctrl+Up).
        parse_mods_and_kind(first)
    } else {
        (KeyModifiers::NONE, KeyEventKind::Press, KeyEventState::NONE)
    };

    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return None,
    };

    Some(KeyEvent::new_with_state(code, modifiers, kind, state))
}

fn decode_sgr_mouse(body: &[u8], is_release: bool) -> Option<MouseEvent> {
    let s = std::str::from_utf8(body).ok()?;
    let mut parts = s.split(';');
    let cb = parts.next()?.parse::<u8>().ok()?;
    let cx = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let cy = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);

    let (mut kind, modifiers) = decode_mouse_cb(cb)?;
    if is_release && let MouseEventKind::Down(btn) = kind {
        kind = MouseEventKind::Up(btn);
    }
    Some(MouseEvent {
        kind,
        column: cx,
        row: cy,
        modifiers,
    })
}

fn decode_rxvt_mouse(body: &str) -> Option<MouseEvent> {
    let mut parts = body.split(';');
    let cb = parts.next()?.parse::<u8>().ok()?.checked_sub(32)?;
    let cx = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let cy = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let (kind, modifiers) = decode_mouse_cb(cb)?;
    Some(MouseEvent {
        kind,
        column: cx,
        row: cy,
        modifiers,
    })
}

fn decode_x10_mouse(buf: [u8; 3]) -> Option<MouseEvent> {
    let cb = buf[0].checked_sub(32)?;
    let cx = u16::from(buf[1].saturating_sub(32)).saturating_sub(1);
    let cy = u16::from(buf[2].saturating_sub(32)).saturating_sub(1);
    let (kind, modifiers) = decode_mouse_cb(cb)?;
    Some(MouseEvent {
        kind,
        column: cx,
        row: cy,
        modifiers,
    })
}

fn decode_mouse_cb(cb: u8) -> Option<(MouseEventKind, KeyModifiers)> {
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let motion = (cb & 0b0010_0000) != 0;

    let kind = match (button_number, motion) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };

    let mut modifiers = KeyModifiers::NONE;
    if (cb & 0b0000_0100) != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if (cb & 0b0000_1000) != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if (cb & 0b0001_0000) != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some((kind, modifiers))
}

fn kitty_functional_key(codepoint: u32) -> Option<(KeyCode, KeyEventState)> {
    let keypad = match codepoint {
        57399 => Some(KeyCode::Char('0')),
        57400 => Some(KeyCode::Char('1')),
        57401 => Some(KeyCode::Char('2')),
        57402 => Some(KeyCode::Char('3')),
        57403 => Some(KeyCode::Char('4')),
        57404 => Some(KeyCode::Char('5')),
        57405 => Some(KeyCode::Char('6')),
        57406 => Some(KeyCode::Char('7')),
        57407 => Some(KeyCode::Char('8')),
        57408 => Some(KeyCode::Char('9')),
        57409 => Some(KeyCode::Char('.')),
        57410 => Some(KeyCode::Char('/')),
        57411 => Some(KeyCode::Char('*')),
        57412 => Some(KeyCode::Char('-')),
        57413 => Some(KeyCode::Char('+')),
        57414 => Some(KeyCode::Enter),
        57415 => Some(KeyCode::Char('=')),
        57416 => Some(KeyCode::Char(',')),
        57417 => Some(KeyCode::Left),
        57418 => Some(KeyCode::Right),
        57419 => Some(KeyCode::Up),
        57420 => Some(KeyCode::Down),
        57421 => Some(KeyCode::PageUp),
        57422 => Some(KeyCode::PageDown),
        57423 => Some(KeyCode::Home),
        57424 => Some(KeyCode::End),
        57425 => Some(KeyCode::Insert),
        57426 => Some(KeyCode::Delete),
        57427 => Some(KeyCode::KeypadBegin),
        _ => None,
    };
    if let Some(code) = keypad {
        return Some((code, KeyEventState::KEYPAD));
    }

    let code = match codepoint {
        57358 => KeyCode::CapsLock,
        57359 => KeyCode::ScrollLock,
        57360 => KeyCode::NumLock,
        57361 => KeyCode::PrintScreen,
        57362 => KeyCode::Pause,
        57363 => KeyCode::Menu,
        57376..=57398 => KeyCode::F((codepoint - 57376 + 13) as u8),
        57428 => KeyCode::Media(MediaKeyCode::Play),
        57429 => KeyCode::Media(MediaKeyCode::Pause),
        57430 => KeyCode::Media(MediaKeyCode::PlayPause),
        57431 => KeyCode::Media(MediaKeyCode::Reverse),
        57432 => KeyCode::Media(MediaKeyCode::Stop),
        57433 => KeyCode::Media(MediaKeyCode::FastForward),
        57434 => KeyCode::Media(MediaKeyCode::Rewind),
        57435 => KeyCode::Media(MediaKeyCode::TrackNext),
        57436 => KeyCode::Media(MediaKeyCode::TrackPrevious),
        57437 => KeyCode::Media(MediaKeyCode::Record),
        57438 => KeyCode::Media(MediaKeyCode::LowerVolume),
        57439 => KeyCode::Media(MediaKeyCode::RaiseVolume),
        57440 => KeyCode::Media(MediaKeyCode::MuteVolume),
        57441 => KeyCode::Modifier(ModifierKeyCode::LeftShift),
        57442 => KeyCode::Modifier(ModifierKeyCode::LeftControl),
        57443 => KeyCode::Modifier(ModifierKeyCode::LeftAlt),
        57444 => KeyCode::Modifier(ModifierKeyCode::LeftSuper),
        57445 => KeyCode::Modifier(ModifierKeyCode::LeftHyper),
        57446 => KeyCode::Modifier(ModifierKeyCode::LeftMeta),
        57447 => KeyCode::Modifier(ModifierKeyCode::RightShift),
        57448 => KeyCode::Modifier(ModifierKeyCode::RightControl),
        57449 => KeyCode::Modifier(ModifierKeyCode::RightAlt),
        57450 => KeyCode::Modifier(ModifierKeyCode::RightSuper),
        57451 => KeyCode::Modifier(ModifierKeyCode::RightHyper),
        57452 => KeyCode::Modifier(ModifierKeyCode::RightMeta),
        57453 => KeyCode::Modifier(ModifierKeyCode::IsoLevel3Shift),
        57454 => KeyCode::Modifier(ModifierKeyCode::IsoLevel5Shift),
        _ => return None,
    };
    Some((code, KeyEventState::NONE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events_from(items: Vec<ParsedItem>) -> Vec<Event> {
        items
            .into_iter()
            .filter_map(|i| match i {
                ParsedItem::Event(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sgr_mouse_split_at_every_possible_byte_boundary_never_leaks() {
        let seq = b"\x1b[<35;52;28M";
        let expected = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 51,
            row: 27,
            modifiers: KeyModifiers::NONE,
        });

        for split in 1..seq.len() {
            let mut parser = VtParser::new();
            let t0 = Instant::now();
            let mut out = Vec::new();
            parser.feed(&seq[..split], t0, &mut out);
            assert!(out.is_empty(), "split at {split} leaked early: {out:?}");
            // Next chunk arrives 2 ms later (well within 20 ms ESC timeout).
            parser.feed(&seq[split..], t0 + Duration::from_millis(2), &mut out);
            assert_eq!(events_from(out), vec![expected.clone()]);
            assert!(!parser.has_pending());
        }
    }

    #[test]
    fn bare_esc_flushes_only_after_timeout() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        parser.feed(b"\x1b", t0, &mut out);
        assert!(out.is_empty());
        assert_eq!(parser.next_deadline(), Some(t0 + DEFAULT_ESC_TIMEOUT));

        // Before deadline: must stay pending.
        parser.flush_timeout(t0 + Duration::from_millis(10), &mut out);
        assert!(out.is_empty());

        // At deadline: flushes as KeyCode::Esc.
        parser.flush_timeout(t0 + Duration::from_millis(20), &mut out);
        assert_eq!(events_from(out), vec![Event::Key(KeyCode::Esc.into())]);
        assert!(parser.next_deadline().is_none());
    }

    #[test]
    fn double_esc_and_esc_before_csi_do_not_swallow_second_esc() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Double Esc followed immediately by an Up arrow in one read.
        parser.feed(b"\x1b\x1b[A", t0, &mut out);
        assert_eq!(
            events_from(out.clone()),
            vec![
                Event::Key(KeyCode::Esc.into()),
                Event::Key(KeyCode::Up.into()),
            ]
        );
        assert!(!parser.has_pending());

        // Pure double Esc across reads, then timeout.
        out.clear();
        parser.feed(b"\x1b\x1b", t0, &mut out);
        assert_eq!(
            events_from(out.clone()),
            vec![Event::Key(KeyCode::Esc.into())]
        );
        out.clear();
        parser.flush_timeout(t0 + DEFAULT_ESC_TIMEOUT, &mut out);
        assert_eq!(events_from(out), vec![Event::Key(KeyCode::Esc.into())]);
    }

    #[test]
    fn kitty_csi_u_with_modifiers_and_kinds() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Ctrl+Enter (`13;5u`), Esc (`27u`), Shift+a release (`97;2:3u`), shifted key (`97:65;2u`)
        parser.feed(b"\x1b[13;5u\x1b[27u\x1b[97;2:3u\x1b[97:65;2u", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                Event::Key(KeyCode::Esc.into()),
                Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('a'),
                    KeyModifiers::SHIFT,
                    KeyEventKind::Release,
                )),
                Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)),
            ]
        );
    }

    #[test]
    fn bracketed_paste_and_utf8_split_across_reads() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Split paste and multi-byte UTF-8 "中" (E4 B8 AD)
        parser.feed(b"\x1b[200~hello\n", t0, &mut out);
        assert!(out.is_empty());
        parser.feed(b"world\x1b[201~\xe4\xb8", t0, &mut out);
        assert_eq!(
            events_from(out.clone()),
            vec![Event::Paste("hello\nworld".to_string())]
        );
        out.clear();
        parser.feed(b"\xad", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![Event::Key(KeyCode::Char('中').into())]
        );
    }

    #[test]
    fn unknown_private_csi_does_not_wedge_subsequent_keys() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Unknown `ESC [ ? 2004 ; 1 $ y` (DECRPM) followed by `'a'`
        parser.feed(b"\x1b[?2004;1$ya", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![Event::Key(KeyCode::Char('a').into())]
        );
    }

    #[test]
    fn probe_replies_are_routed_as_internal_items() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        parser.feed(b"\x1b[?1u\x1b[?64;1;2;22c", t0, &mut out);
        assert_eq!(
            out,
            vec![
                ParsedItem::KeyboardEnhancementFlags(1),
                ParsedItem::PrimaryDeviceAttributes,
            ]
        );
    }

    #[test]
    fn alt_keys_and_control_chars_decode_cleanly() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        parser.feed(b"\x1bx\x1b\x7f\r\n\x03", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
                Event::Key(KeyCode::Enter.into()),
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ]
        );
    }

    #[test]
    fn ctrl_shift_chords_preserve_shift_under_kitty_alternate_keys_and_modify_other_keys() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Ctrl+Shift+P via CSI-u (`112;6u`), CSI-u with shifted alternate (`112:80;6u`),
        // and xterm modifyOtherKeys (`27;6;112~`), plus Cmd+C (`99;9u`) and Shift+Enter (`13;2u`).
        parser.feed(
            b"\x1b[112;6u\x1b[112:80;6u\x1b[27;6;112~\x1b[99;9u\x1b[13;2u\x1b[27;2;13~",
            t0,
            &mut out,
        );
        assert_eq!(
            events_from(out),
            vec![
                Event::Key(KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                )),
                Event::Key(KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                )),
                Event::Key(KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                )),
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            ]
        );
    }

    #[test]
    fn cpr_is_ignored_while_modified_f3_and_f4_decode() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // CPR (`24;80R`) must not emit F3; `1;5R` is Ctrl+F3; `ESC O R` is F3; `ESC O S` / `ESC [ S` is F4.
        parser.feed(b"\x1b[24;80R\x1b[1;5R\x1bOR\x1bOS\x1b[S", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![
                Event::Key(KeyEvent::new(KeyCode::F(3), KeyModifiers::CONTROL)),
                Event::Key(KeyCode::F(3).into()),
                Event::Key(KeyCode::F(4).into()),
                Event::Key(KeyCode::F(4).into()),
            ]
        );
    }

    #[test]
    fn alt_brackets_and_backtab_decode_in_legacy_and_kitty_modes() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Legacy Alt+], Kitty Alt+[, Kitty Alt+], legacy BackTab, Kitty Shift+Tab.
        parser.feed(b"\x1b]\x1b[91;3u\x1b[93;3u\x1b[Z\x1b[9;2u", t0, &mut out);
        assert_eq!(
            events_from(out),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            ]
        );
    }

    #[test]
    fn sgr_mouse_burst_and_high_coordinates_decode_losslessly() {
        let mut parser = VtParser::new();
        let t0 = Instant::now();
        let mut out = Vec::new();

        let mut wire = Vec::new();
        for col in 1u16..=50 {
            wire.extend_from_slice(format!("\x1b[<35;{};120M", col + 250).as_bytes());
        }
        // Append left press, drag, release, and scroll up/down with Ctrl modifier.
        wire.extend_from_slice(
            b"\x1b[<0;300;40M\x1b[<32;301;40M\x1b[<0;301;40m\x1b[<80;10;5M\x1b[<81;10;5M",
        );

        // Feed 7 bytes at a time to force splits across every phase of the state machine.
        for chunk in wire.chunks(7) {
            parser.feed(chunk, t0, &mut out);
        }
        let events = events_from(out);
        assert_eq!(events.len(), 55);
        assert_eq!(
            events[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 250,
                row: 119,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            events[50],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 299,
                row: 39,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            events[51],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 300,
                row: 39,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            events[52],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 300,
                row: 39,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            events[53],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 9,
                row: 4,
                modifiers: KeyModifiers::CONTROL,
            })
        );
        assert_eq!(
            events[54],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 9,
                row: 4,
                modifiers: KeyModifiers::CONTROL,
            })
        );
    }

    #[test]
    fn oversized_csi_and_paste_are_bounded_and_recover() {
        let mut parser = VtParser::new().with_max_paste_bytes(16);
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Oversized CSI (> 256 parameter bytes) followed by real char 'z'.
        let mut bad_csi = vec![b'\x1b', b'['];
        bad_csi.resize(MAX_SEQUENCE_BYTES + 10, b'1');
        bad_csi.push(b'z');
        parser.feed(&bad_csi, t0, &mut out);
        // After dropping the oversized CSI, subsequent valid input still parses.
        out.clear();
        parser.feed(b"ok", t0, &mut out);
        assert_eq!(
            events_from(out.clone()),
            vec![
                Event::Key(KeyCode::Char('o').into()),
                Event::Key(KeyCode::Char('k').into()),
            ]
        );

        // Oversized paste flushes bounded payload instead of growing unboundedly.
        out.clear();
        parser.feed(b"\x1b[200~0123456789abcdefOVERFLOW", t0, &mut out);
        assert_eq!(
            events_from(out)[0],
            Event::Paste("0123456789abcdef".to_string())
        );
    }
}
