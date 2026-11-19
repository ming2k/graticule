//! Outbound CSI and DEC private-mode escape sequences.
//!
//! Every parameterized encoder formats directly into a small stack buffer and
//! issues a single [`Write::write_all`] call with zero heap allocation and zero
//! `core::fmt` overhead.

use std::io::{self, Write};

// ---- Cursor visibility (DECTCEM) -------------------------------------------

/// Hide the hardware cursor (`DEC` private mode 25 reset).
pub const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

/// Show the hardware cursor (`DEC` private mode 25 set).
pub const CURSOR_SHOW: &[u8] = b"\x1b[?25h";

// ---- Line wrap (DECAWM) ----------------------------------------------------

/// Disable automatic line wrap at the right margin (`DEC` private mode 7 reset).
pub const DISABLE_LINE_WRAP: &[u8] = b"\x1b[?7l";

/// Re-enable automatic line wrap at the right margin (`DEC` private mode 7 set).
pub const ENABLE_LINE_WRAP: &[u8] = b"\x1b[?7h";

// ---- Screen and line erasure (ED / EL) -------------------------------------

/// Erase from cursor to end of current line (`EL 0`).
pub const CLEAR_UNTIL_NEWLINE: &[u8] = b"\x1b[K";

/// Erase the entire current line (`EL 2`).
pub const CLEAR_LINE: &[u8] = b"\x1b[2K";

/// Erase from cursor to end of screen (`ED 0`).
pub const CLEAR_FROM_CURSOR_DOWN: &[u8] = b"\x1b[J";

/// Erase the entire visible screen (`ED 2`).
pub const CLEAR_ALL: &[u8] = b"\x1b[2J";

/// Erase the entire visible screen and scrollback buffer (`ED 3`).
pub const CLEAR_ALL_AND_SCROLLBACK: &[u8] = b"\x1b[3J";

// ---- Scroll regions (DECSTBM) ----------------------------------------------

/// Reset scrolling margins back to the full viewport (`DECSTBM` with no args).
pub const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";

// ---- Terminal session modes ------------------------------------------------

/// Switch to the alternate screen buffer and save cursor (`DEC` mode 1049 set).
pub const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

/// Leave the alternate screen buffer and restore cursor (`DEC` mode 1049 reset).
pub const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

/// Open a DEC Mode 2026 synchronized-update envelope.
pub const BEGIN_SYNC_UPDATE: &[u8] = b"\x1b[?2026h";

/// Close a DEC Mode 2026 synchronized-update envelope.
pub const END_SYNC_UPDATE: &[u8] = b"\x1b[?2026l";

/// Enable bracketed paste (`ESC[200~ ... ESC[201~`).
pub const ENABLE_BRACKETED_PASTE: &[u8] = b"\x1b[?2004h";

/// Disable bracketed paste.
pub const DISABLE_BRACKETED_PASTE: &[u8] = b"\x1b[?2004l";

/// Enable terminal focus reporting (`ESC[I` / `ESC[O`).
pub const ENABLE_FOCUS_EVENTS: &[u8] = b"\x1b[?1004h";

/// Disable terminal focus reporting.
pub const DISABLE_FOCUS_EVENTS: &[u8] = b"\x1b[?1004l";

// ---- Capability queries & Kitty keyboard protocol --------------------------

/// Query active Kitty keyboard enhancement flags (`CSI ? u`).
pub const QUERY_KITTY_KEYBOARD: &[u8] = b"\x1b[?u";

/// Query Primary Device Attributes (`DA1`, `CSI c`).
pub const QUERY_PRIMARY_DA: &[u8] = b"\x1b[c";

/// Combined Kitty keyboard query followed by DA1 sentinel.
pub const PROBE_KITTY_AND_DA1: &[u8] = b"\x1b[?u\x1b[c";

/// Push Kitty `DISAMBIGUATE_ESCAPE_CODES` flag (`1`) onto the keyboard stack.
pub const PUSH_KITTY_DISAMBIGUATE: &[u8] = b"\x1b[>1u";

/// Pop one entry from the Kitty keyboard protocol stack.
pub const POP_KITTY_KEYBOARD: &[u8] = b"\x1b[<u";

// ---- Cursor shape (DECSCUSR) -----------------------------------------------

/// Hardware cursor shape (`DECSCUSR`, `CSI Ps SP q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorStyle {
    /// Restore the terminal user's default cursor shape (`0`).
    #[default]
    DefaultUserShape,
    /// Blinking block cursor (`1`).
    BlinkingBlock,
    /// Steady block cursor (`2`).
    SteadyBlock,
    /// Blinking underline cursor (`3`).
    BlinkingUnderline,
    /// Steady underline cursor (`4`).
    SteadyUnderline,
    /// Blinking vertical bar cursor (`5`).
    BlinkingBar,
    /// Steady vertical bar cursor (`6`).
    SteadyBar,
}

impl CursorStyle {
    /// Exact DECSCUSR byte sequence for this cursor shape.
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::DefaultUserShape => b"\x1b[0 q",
            Self::BlinkingBlock => b"\x1b[1 q",
            Self::SteadyBlock => b"\x1b[2 q",
            Self::BlinkingUnderline => b"\x1b[3 q",
            Self::SteadyUnderline => b"\x1b[4 q",
            Self::BlinkingBar => b"\x1b[5 q",
            Self::SteadyBar => b"\x1b[6 q",
        }
    }
}

/// Move cursor to 0-based `(col, row)` using ECMA-48 `CUP` (`\x1b[{row+1};{col+1}H`).
#[inline]
pub fn write_move_to<W: Write>(out: &mut W, col: u16, row: u16) -> io::Result<()> {
    let mut buf = StackBuf::<24>::new();
    buf.push_bytes(b"\x1b[");
    buf.push_u16(row.saturating_add(1));
    buf.push(b';');
    buf.push_u16(col.saturating_add(1));
    buf.push(b'H');
    out.write_all(buf.as_slice())
}

/// Set scrolling margins (`DECSTBM`, `\x1b[{top};{bottom}r`) using 1-based inclusive rows.
#[inline]
pub fn write_scroll_region<W: Write>(out: &mut W, top: u16, bottom: u16) -> io::Result<()> {
    let mut buf = StackBuf::<24>::new();
    buf.push_bytes(b"\x1b[");
    buf.push_u16(top);
    buf.push(b';');
    buf.push_u16(bottom);
    buf.push(b'r');
    out.write_all(buf.as_slice())
}

/// Scroll viewport content up by `amount` rows (`SU`, `\x1b[{amount}S`).
#[inline]
pub fn write_scroll_up<W: Write>(out: &mut W, amount: u16) -> io::Result<()> {
    let mut buf = StackBuf::<16>::new();
    buf.push_bytes(b"\x1b[");
    buf.push_u16(amount);
    buf.push(b'S');
    out.write_all(buf.as_slice())
}

/// Scroll viewport content down by `amount` rows (`SD`, `\x1b[{amount}T`).
#[inline]
pub fn write_scroll_down<W: Write>(out: &mut W, amount: u16) -> io::Result<()> {
    let mut buf = StackBuf::<16>::new();
    buf.push_bytes(b"\x1b[");
    buf.push_u16(amount);
    buf.push(b'T');
    out.write_all(buf.as_slice())
}

/// Push an arbitrary Kitty keyboard protocol flag bitmask (`\x1b[>{flags}u`).
#[inline]
pub fn write_push_kitty_flags<W: Write>(out: &mut W, flags: u16) -> io::Result<()> {
    let mut buf = StackBuf::<16>::new();
    buf.push_bytes(b"\x1b[>");
    buf.push_u16(flags);
    buf.push(b'u');
    out.write_all(buf.as_slice())
}

/// Write `count` ASCII space characters without heap allocation.
#[inline]
pub fn write_spaces<W: Write>(out: &mut W, mut count: usize) -> io::Result<()> {
    const SPACES: [u8; 64] = [b' '; 64];
    while count > 0 {
        let chunk = count.min(SPACES.len());
        out.write_all(&SPACES[..chunk])?;
        count -= chunk;
    }
    Ok(())
}

/// Tiny fixed-capacity stack buffer for zero-allocation escape formatting.
pub(crate) struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, byte: u8) {
        if self.len < N {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }

    #[inline]
    pub(crate) fn push_bytes(&mut self, bytes: &[u8]) {
        let avail = N.saturating_sub(self.len);
        let copy_len = bytes.len().min(avail);
        self.buf[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len += copy_len;
    }

    #[inline]
    pub(crate) fn push_u8(&mut self, mut n: u8) {
        if n >= 100 {
            self.push(b'0' + (n / 100));
            n %= 100;
            self.push(b'0' + (n / 10));
            self.push(b'0' + (n % 10));
        } else if n >= 10 {
            self.push(b'0' + (n / 10));
            self.push(b'0' + (n % 10));
        } else {
            self.push(b'0' + n);
        }
    }

    #[inline]
    pub(crate) fn push_u16(&mut self, mut n: u16) {
        let mut digits = [0u8; 5];
        let mut i = 0;
        loop {
            digits[i] = b'0' + (n % 10) as u8;
            i += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        while i > 0 {
            i -= 1;
            self.push(digits[i]);
        }
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cup_and_scroll_format_exact_sequences() {
        let mut out = Vec::new();
        write_move_to(&mut out, 0, 0).unwrap();
        write_move_to(&mut out, 5, 7).unwrap();
        write_scroll_region(&mut out, 2, 23).unwrap();
        write_scroll_up(&mut out, 3).unwrap();
        write_scroll_down(&mut out, 1).unwrap();
        write_push_kitty_flags(&mut out, 15).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[1;1H\x1b[8;6H\x1b[2;23r\x1b[3S\x1b[1T\x1b[>15u"
        );
    }

    #[test]
    fn write_spaces_handles_zero_small_and_large_counts() {
        let mut out = Vec::new();
        write_spaces(&mut out, 0).unwrap();
        assert!(out.is_empty());

        write_spaces(&mut out, 5).unwrap();
        assert_eq!(out, b"     ");

        out.clear();
        write_spaces(&mut out, 200).unwrap();
        assert_eq!(out.len(), 200);
        assert!(out.iter().all(|&b| b == b' '));
    }

    #[test]
    fn cursor_styles_emit_standard_decscusr() {
        assert_eq!(CursorStyle::DefaultUserShape.as_bytes(), b"\x1b[0 q");
        assert_eq!(CursorStyle::SteadyBar.as_bytes(), b"\x1b[6 q");
    }
}
