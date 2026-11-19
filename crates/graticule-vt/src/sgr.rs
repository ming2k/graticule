//! Zero-allocation ECMA-48 / ITU-T T.416 SGR (Select Graphic Rendition) encoder.
//!
//! Combines all style changes for a single transition into one `\x1b[...m`
//! sequence inside a stack-allocated buffer and flushes it in one
//! [`Write::write_all`] call.

use std::io::{self, Write};

use crate::csi::StackBuf;

/// Full SGR attribute reset (`\x1b[0m`).
pub const SGR_RESET: &[u8] = b"\x1b[0m";

/// Wire-level color representation for SGR encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SgrColor {
    /// Reset foreground/background to the terminal default (`39` / `49`).
    #[default]
    Reset,
    /// ANSI black (`30` / `40`).
    Black,
    /// ANSI red (`31` / `41`).
    Red,
    /// ANSI green (`32` / `42`).
    Green,
    /// ANSI yellow (`33` / `43`).
    Yellow,
    /// ANSI blue (`34` / `44`).
    Blue,
    /// ANSI magenta (`35` / `45`).
    Magenta,
    /// ANSI cyan (`36` / `46`).
    Cyan,
    /// ANSI white / light gray (`37` / `47`).
    Gray,
    /// Bright black / dark gray (`90` / `100`).
    DarkGray,
    /// Bright red (`91` / `101`).
    LightRed,
    /// Bright green (`92` / `102`).
    LightGreen,
    /// Bright yellow (`93` / `103`).
    LightYellow,
    /// Bright blue (`94` / `104`).
    LightBlue,
    /// Bright magenta (`95` / `105`).
    LightMagenta,
    /// Bright cyan (`96` / `106`).
    LightCyan,
    /// Bright white (`97` / `107`).
    White,
    /// 8-bit 256-color palette index (`38;5;n` / `48;5;n`).
    Indexed(u8),
    /// 24-bit ITU-T T.416 Direct Color (`38;2;r;g;b` / `48;2;r;g;b`).
    Rgb(u8, u8, u8),
}

/// Stack-buffered builder that emits a single combined `\x1b[...m` sequence.
///
/// Capacity (`96` bytes) comfortably fits a worst-case combined sequence:
/// reset + 24-bit fg + 24-bit bg + all 6 text modifiers (`< 64` bytes).
pub struct SgrEncoder {
    buf: StackBuf<96>,
    has_param: bool,
}

impl Default for SgrEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SgrEncoder {
    /// Start a new empty SGR sequence.
    #[inline]
    pub const fn new() -> Self {
        Self {
            buf: StackBuf::new(),
            has_param: false,
        }
    }

    #[inline]
    fn begin_param(&mut self) {
        if !self.has_param {
            self.buf.push_bytes(b"\x1b[");
            self.has_param = true;
        } else {
            self.buf.push(b';');
        }
    }

    /// Push SGR `0` (reset all attributes and colors).
    #[inline]
    pub fn reset(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'0');
        self
    }

    /// Push a foreground color parameter.
    #[inline]
    pub fn fg(&mut self, color: SgrColor) -> &mut Self {
        self.push_color(color, false);
        self
    }

    /// Push a background color parameter.
    #[inline]
    pub fn bg(&mut self, color: SgrColor) -> &mut Self {
        self.push_color(color, true);
        self
    }

    /// Push SGR `1` (bold / increased intensity).
    #[inline]
    pub fn bold(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'1');
        self
    }

    /// Push SGR `2` (dim / faint).
    #[inline]
    pub fn dim(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'2');
        self
    }

    /// Push SGR `3` (italic).
    #[inline]
    pub fn italic(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'3');
        self
    }

    /// Push SGR `4` (underline).
    #[inline]
    pub fn underline(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'4');
        self
    }

    /// Push SGR `7` (reverse video).
    #[inline]
    pub fn reverse(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'7');
        self
    }

    /// Push SGR `9` (strikethrough / crossed out).
    #[inline]
    pub fn strikethrough(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push(b'9');
        self
    }

    /// Push SGR `22` (normal intensity: disables both bold and dim).
    #[inline]
    pub fn normal_intensity(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push_bytes(b"22");
        self
    }

    /// Push SGR `23` (not italic).
    #[inline]
    pub fn no_italic(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push_bytes(b"23");
        self
    }

    /// Push SGR `24` (not underlined).
    #[inline]
    pub fn no_underline(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push_bytes(b"24");
        self
    }

    /// Push SGR `27` (not reversed).
    #[inline]
    pub fn no_reverse(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push_bytes(b"27");
        self
    }

    /// Push SGR `29` (not crossed out).
    #[inline]
    pub fn no_strikethrough(&mut self) -> &mut Self {
        self.begin_param();
        self.buf.push_bytes(b"29");
        self
    }

    /// Whether no parameters have been appended.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        !self.has_param
    }

    /// Finalize the SGR sequence with `'m'` and write it to `out` in one call.
    /// Writes zero bytes if no parameters were appended.
    #[inline]
    pub fn finish<W: Write>(mut self, out: &mut W) -> io::Result<()> {
        if !self.has_param {
            return Ok(());
        }
        self.buf.push(b'm');
        out.write_all(self.buf.as_slice())
    }

    #[inline]
    fn push_color(&mut self, color: SgrColor, is_bg: bool) {
        self.begin_param();
        let offset = if is_bg { 10u8 } else { 0u8 };
        match color {
            SgrColor::Reset => self.buf.push_u8(39 + offset),
            SgrColor::Black => self.buf.push_u8(30 + offset),
            SgrColor::Red => self.buf.push_u8(31 + offset),
            SgrColor::Green => self.buf.push_u8(32 + offset),
            SgrColor::Yellow => self.buf.push_u8(33 + offset),
            SgrColor::Blue => self.buf.push_u8(34 + offset),
            SgrColor::Magenta => self.buf.push_u8(35 + offset),
            SgrColor::Cyan => self.buf.push_u8(36 + offset),
            SgrColor::Gray => self.buf.push_u8(37 + offset),
            SgrColor::DarkGray => self.buf.push_u8(90 + offset),
            SgrColor::LightRed => self.buf.push_u8(91 + offset),
            SgrColor::LightGreen => self.buf.push_u8(92 + offset),
            SgrColor::LightYellow => self.buf.push_u8(93 + offset),
            SgrColor::LightBlue => self.buf.push_u8(94 + offset),
            SgrColor::LightMagenta => self.buf.push_u8(95 + offset),
            SgrColor::LightCyan => self.buf.push_u8(96 + offset),
            SgrColor::White => self.buf.push_u8(97 + offset),
            SgrColor::Indexed(idx) => {
                self.buf.push_u8(38 + offset);
                self.buf.push_bytes(b";5;");
                self.buf.push_u8(idx);
            }
            SgrColor::Rgb(r, g, b) => {
                self.buf.push_u8(38 + offset);
                self.buf.push_bytes(b";2;");
                self.buf.push_u8(r);
                self.buf.push(b';');
                self.buf.push_u8(g);
                self.buf.push(b';');
                self.buf.push_u8(b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_encoder_writes_nothing() {
        let mut out = Vec::new();
        SgrEncoder::new().finish(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn encodes_combined_rgb_and_attributes_in_one_sequence() {
        let mut out = Vec::new();
        let mut enc = SgrEncoder::new();
        enc.reset()
            .fg(SgrColor::Rgb(200, 30, 40))
            .bg(SgrColor::Indexed(235))
            .bold()
            .italic()
            .underline();
        enc.finish(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[0;38;2;200;30;40;48;5;235;1;3;4m"
        );
    }

    #[test]
    fn encodes_all_16_ansi_colors_with_standard_ecma48_codes() {
        let cases = [
            (SgrColor::Reset, "\x1b[39;49m"),
            (SgrColor::Black, "\x1b[30;40m"),
            (SgrColor::Red, "\x1b[31;41m"),
            (SgrColor::Green, "\x1b[32;42m"),
            (SgrColor::Yellow, "\x1b[33;43m"),
            (SgrColor::Blue, "\x1b[34;44m"),
            (SgrColor::Magenta, "\x1b[35;45m"),
            (SgrColor::Cyan, "\x1b[36;46m"),
            (SgrColor::Gray, "\x1b[37;47m"),
            (SgrColor::DarkGray, "\x1b[90;100m"),
            (SgrColor::LightRed, "\x1b[91;101m"),
            (SgrColor::LightGreen, "\x1b[92;102m"),
            (SgrColor::LightYellow, "\x1b[93;103m"),
            (SgrColor::LightBlue, "\x1b[94;104m"),
            (SgrColor::LightMagenta, "\x1b[95;105m"),
            (SgrColor::LightCyan, "\x1b[96;106m"),
            (SgrColor::White, "\x1b[97;107m"),
        ];
        for (color, expected) in cases {
            let mut out = Vec::new();
            let mut enc = SgrEncoder::new();
            enc.fg(color).bg(color);
            enc.finish(&mut out).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), expected);
        }
    }

    #[test]
    fn worst_case_combined_sequence_fits_without_truncation() {
        let mut out = Vec::new();
        let mut enc = SgrEncoder::new();
        enc.reset()
            .fg(SgrColor::Rgb(255, 255, 255))
            .bg(SgrColor::Rgb(255, 255, 255))
            .bold()
            .dim()
            .italic()
            .underline()
            .reverse()
            .strikethrough();
        enc.finish(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[0;38;2;255;255;255;48;2;255;255;255;1;2;3;4;7;9m"
        );
    }
}
