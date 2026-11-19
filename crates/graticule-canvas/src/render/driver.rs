//! Escape-sequence emission: one capability-driven [`Driver`], plus the
//! [`EscapeEmitter`] extension point.
//!
//! # Design
//!
//! Capability *detection* lives in [`crate::render::profile`]. This module only
//! turns a requested [`Style`] into the minimal ECMA-48 / ITU-T T.416 SGR bytes
//! a given palette needs, and wraps frames in a DEC Mode 2026 synchronized-update
//! envelope when the terminal supports one.
//!
//! Every capability that affects emission is a field here, and every one of
//! them is honoured:
//!
//! - `palette` — how colors map onto the terminal (`Monochrome` strips them and
//!   expresses emphasis through reverse video).
//! - `italic` — when false, italic maps to underline instead of being emitted.
//! - `charset` — when [`CharsetStandard::Ascii`], [`EscapeEmitter::transliterate`]
//!   degrades any non-ASCII symbol to 7-bit, geometry preserved.

use std::borrow::Cow;
use std::io::{self, Write};

use graticule_vt::csi::{BEGIN_SYNC_UPDATE, END_SYNC_UPDATE};
use graticule_vt::sgr::{SGR_RESET, SgrColor, SgrEncoder};

use crate::buffer::glyph::to_ascii;
use crate::buffer::{Color, Modifier, Style};
use crate::render::profile::{
    CharsetStandard, ColorStandard, TerminalProfile, quantize_to_ansi16, quantize_to_indexed256,
};

/// How a style's colors are mapped onto the target terminal's palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    /// ITU-T T.416 Direct Color: 24-bit RGB, passed through untouched.
    #[default]
    TrueColor,
    /// 8-bit indexed palette: colors are quantized to `0..=255`.
    Indexed256,
    /// ECMA-48 + aixterm 16-color: colors are quantized to the named palette.
    Ansi16,
    /// DEC VT100 / getty: no color codes at all.
    Monochrome,
}

impl Palette {
    /// The color standard this palette implements.
    pub const fn standard(self) -> ColorStandard {
        match self {
            Palette::TrueColor => ColorStandard::DirectColor,
            Palette::Indexed256 => ColorStandard::Indexed256,
            Palette::Ansi16 => ColorStandard::Ansi16,
            Palette::Monochrome => ColorStandard::Monochrome,
        }
    }

    /// The palette that implements `standard`.
    pub const fn from_standard(standard: ColorStandard) -> Self {
        match standard {
            ColorStandard::DirectColor => Palette::TrueColor,
            ColorStandard::Indexed256 => Palette::Indexed256,
            ColorStandard::Ansi16 => Palette::Ansi16,
            ColorStandard::Monochrome => Palette::Monochrome,
        }
    }

    /// Map one color onto the palette. Monochrome has no colors, so it reports
    /// [`Color::Reset`] and lets the style-level mapping add reverse video.
    pub fn encode(self, color: Color) -> Color {
        match self {
            Palette::TrueColor => color,
            Palette::Indexed256 => quantize_to_indexed256(color),
            Palette::Ansi16 => quantize_to_ansi16(color),
            Palette::Monochrome => Color::Reset,
        }
    }

    /// Whether this palette can render chromatic distinction (i.e. is not
    /// monochrome).
    pub const fn supports_color(self) -> bool {
        !matches!(self, Palette::Monochrome)
    }
}

/// The escape-emission contract.
///
/// Implemented by [`Driver`]; a custom implementation can be handed to
/// [`crate::terminal::Backend`] to target an unusual protocol.
pub trait EscapeEmitter {
    /// Emit the bytes that turn the terminal's current style into `want`.
    fn apply_style<W: Write>(&mut self, want: Style, out: &mut W) -> io::Result<()>;
    /// Open a synchronized-update envelope, if the terminal supports one.
    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Close a synchronized-update envelope and flush.
    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Forget tracked style state and reset the terminal's SGR state.
    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()>;
    /// Translate a symbol into bytes this terminal can render.
    ///
    /// Returns [`Cow::Borrowed`] when the symbol is already representable, so a
    /// UTF-8 terminal pays nothing.
    fn transliterate<'a>(&self, symbol: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(symbol)
    }
}

/// The standard escape emitter: zero-allocation combined SGR delta emission
/// against a [`Palette`], with synchronized updates and symbol transliteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Driver {
    palette: Palette,
    style: Style,
    sync: bool,
    italic: bool,
    charset: CharsetStandard,
}

impl Default for Driver {
    /// A fully capable terminal: truecolor, italic, synchronized updates, UTF-8.
    fn default() -> Self {
        Self {
            palette: Palette::TrueColor,
            style: Style::RESET,
            sync: true,
            italic: true,
            charset: CharsetStandard::Utf8,
        }
    }
}

impl Driver {
    /// Build the driver a capability profile calls for.
    pub fn for_profile(profile: &TerminalProfile) -> Self {
        Self {
            palette: Palette::from_standard(profile.color_standard),
            style: Style::RESET,
            sync: profile.supports_sync_update,
            italic: profile.supports_italic,
            charset: profile.charset_standard,
        }
    }

    /// Build a driver for a palette, with every other capability at its most
    /// capable setting. Tests and explicit overrides use this.
    pub fn with_palette(palette: Palette) -> Self {
        Self {
            palette,
            ..Self::default()
        }
    }

    /// Override whether synchronized updates are used.
    pub const fn with_sync(mut self, sync: bool) -> Self {
        self.sync = sync;
        self
    }

    /// Override whether italic is emitted directly (false maps it to underline).
    pub const fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    /// Override the symbol character set.
    pub const fn with_charset(mut self, charset: CharsetStandard) -> Self {
        self.charset = charset;
        self
    }

    /// The palette in use.
    pub const fn palette(&self) -> Palette {
        self.palette
    }

    /// The color standard this driver implements.
    pub const fn color_standard(&self) -> ColorStandard {
        self.palette.standard()
    }

    /// Whether synchronized updates are enabled.
    pub const fn supports_sync_update(&self) -> bool {
        self.sync
    }

    /// The symbol character set in use.
    pub const fn charset_standard(&self) -> CharsetStandard {
        self.charset
    }

    /// The style the terminal is currently believed to be in.
    pub const fn current_style(&self) -> Style {
        self.style
    }

    /// Map a requested style onto this driver's capabilities, so the emitting
    /// code has no capability branches of its own.
    fn prepare(&self, mut want: Style) -> Style {
        match self.palette {
            Palette::Monochrome => {
                // No color codes may be emitted. Visual emphasis that was
                // expressed with a background or an explicit reverse is
                // preserved as reverse video, which every VT100 understands.
                let emphasis = want.bg != Color::Reset || want.add.contains(Modifier::REVERSE);
                want.fg = Color::Reset;
                want.bg = Color::Reset;
                if emphasis {
                    want.add.insert(Modifier::REVERSE);
                }
                // A physical VT100 has no dim; keeping the bit would emit an
                // attribute the terminal ignores or renders as bold.
                want.add.remove(Modifier::DIM);
            }
            _ => {
                want.fg = self.palette.encode(want.fg);
                want.bg = self.palette.encode(want.bg);
            }
        }
        if !self.italic && want.add.contains(Modifier::ITALIC) {
            want.add.remove(Modifier::ITALIC);
            want.add.insert(Modifier::UNDERLINE);
        }
        want
    }
}

impl EscapeEmitter for Driver {
    fn apply_style<W: Write>(&mut self, want: Style, out: &mut W) -> io::Result<()> {
        let want = self.prepare(want);
        if want == self.style {
            return Ok(());
        }
        let have = self.style;
        let dropped = have.add - want.add;
        let mut sgr = SgrEncoder::new();

        if dropped.is_empty() {
            if want.fg != have.fg {
                sgr.fg(to_sgr_color(want.fg));
            }
            if want.bg != have.bg {
                sgr.bg(to_sgr_color(want.bg));
            }
            push_modifiers(&mut sgr, want.add - have.add);
        } else {
            // SGR 0 clears everything (compatible all the way down to VT100);
            // combine the reset and re-applied style into a single sequence.
            sgr.reset();
            if want.fg != Color::Reset {
                sgr.fg(to_sgr_color(want.fg));
            }
            if want.bg != Color::Reset {
                sgr.bg(to_sgr_color(want.bg));
            }
            push_modifiers(&mut sgr, want.add);
        }

        sgr.finish(out)?;
        self.style = want;
        Ok(())
    }

    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.sync {
            out.write_all(BEGIN_SYNC_UPDATE)
        } else {
            Ok(())
        }
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.sync {
            out.write_all(END_SYNC_UPDATE)?;
        }
        out.flush()
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.write_all(SGR_RESET)?;
        self.style = Style::RESET;
        Ok(())
    }

    fn transliterate<'a>(&self, symbol: &'a str) -> Cow<'a, str> {
        match self.charset {
            CharsetStandard::Utf8 => Cow::Borrowed(symbol),
            CharsetStandard::Ascii => {
                if symbol.is_ascii() {
                    Cow::Borrowed(symbol)
                } else {
                    Cow::Owned(to_ascii(symbol))
                }
            }
        }
    }
}

/// Map a canvas [`Color`] onto the wire-level [`SgrColor`].
#[inline]
pub const fn to_sgr_color(c: Color) -> SgrColor {
    match c {
        Color::Reset => SgrColor::Reset,
        Color::Black => SgrColor::Black,
        Color::Red => SgrColor::Red,
        Color::Green => SgrColor::Green,
        Color::Yellow => SgrColor::Yellow,
        Color::Blue => SgrColor::Blue,
        Color::Magenta => SgrColor::Magenta,
        Color::Cyan => SgrColor::Cyan,
        Color::Gray => SgrColor::Gray,
        Color::DarkGray => SgrColor::DarkGray,
        Color::LightRed => SgrColor::LightRed,
        Color::LightGreen => SgrColor::LightGreen,
        Color::LightYellow => SgrColor::LightYellow,
        Color::LightBlue => SgrColor::LightBlue,
        Color::LightMagenta => SgrColor::LightMagenta,
        Color::LightCyan => SgrColor::LightCyan,
        Color::White => SgrColor::White,
        Color::Indexed(idx) => SgrColor::Indexed(idx),
        Color::Rgb(r, g, b) => SgrColor::Rgb(r, g, b),
    }
}

/// Push modifier bits in a deterministic, stable order with zero allocation.
#[inline]
fn push_modifiers(sgr: &mut SgrEncoder, m: Modifier) {
    if m.contains(Modifier::BOLD) {
        sgr.bold();
    }
    if m.contains(Modifier::DIM) {
        sgr.dim();
    }
    if m.contains(Modifier::ITALIC) {
        sgr.italic();
    }
    if m.contains(Modifier::UNDERLINE) {
        sgr.underline();
    }
    if m.contains(Modifier::REVERSE) {
        sgr.reverse();
    }
    if m.contains(Modifier::STRIKETHROUGH) {
        sgr.strikethrough();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(driver: &mut Driver, style: Style) -> String {
        let mut out = Vec::new();
        driver.apply_style(style, &mut out).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn sync_envelope_is_opt_in() {
        let mut plain = Driver::with_palette(Palette::TrueColor).with_sync(false);
        let mut out = Vec::new();
        plain.begin_sync(&mut out).unwrap();
        plain.end_sync(&mut out).unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("\x1b[?2026h"));

        let mut sync = Driver::with_palette(Palette::TrueColor).with_sync(true);
        let mut out = Vec::new();
        sync.begin_sync(&mut out).unwrap();
        sync.end_sync(&mut out).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[?2026h"), "must emit begin sync: {s:?}");
        assert!(s.contains("\x1b[?2026l"), "must emit end sync: {s:?}");
    }

    #[test]
    fn truecolor_passes_rgb_through_and_ansi16_quantizes() {
        let style = Style::default().fg(Color::Rgb(200, 30, 40));
        let mut direct = Driver::with_palette(Palette::TrueColor);
        assert_eq!(emit(&mut direct, style), "\x1b[38;2;200;30;40m");
        let mut ansi = Driver::with_palette(Palette::Ansi16);
        let out = emit(&mut ansi, style);
        assert!(
            !out.contains("38;2;"),
            "ansi16 must not emit a 24-bit sequence: {out:?}"
        );
        assert_eq!(out, "\x1b[31m");
    }

    #[test]
    fn monochrome_emits_no_color_codes_and_keeps_emphasis_as_reverse() {
        let mut mono = Driver::with_palette(Palette::Monochrome);
        let out = emit(
            &mut mono,
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .bg(Color::Rgb(4, 5, 6)),
        );
        assert_eq!(out, "\x1b[7m");
    }

    #[test]
    fn italic_maps_to_underline_only_when_italic_is_requested() {
        let italic_style = Style::default().add_modifier(Modifier::ITALIC);
        let mut no_italic = Driver::with_palette(Palette::Ansi16).with_italic(false);
        assert_eq!(emit(&mut no_italic, italic_style), "\x1b[4m");

        // Plain style on a non-italic driver must NOT receive underline.
        let mut no_italic_plain = Driver::with_palette(Palette::Ansi16).with_italic(false);
        assert_eq!(
            emit(&mut no_italic_plain, Style::default().fg(Color::Cyan)),
            "\x1b[36m"
        );

        let mut italic = Driver::with_palette(Palette::TrueColor).with_italic(true);
        assert_eq!(emit(&mut italic, italic_style), "\x1b[3m");
    }

    #[test]
    fn dropping_an_attribute_resets_then_reapplies_the_style_in_one_sequence() {
        let mut d = Driver::with_palette(Palette::TrueColor);
        assert_eq!(
            emit(
                &mut d,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            "\x1b[31;1m"
        );
        assert_eq!(emit(&mut d, Style::default().fg(Color::Red)), "\x1b[0;31m");
    }

    #[test]
    fn ascii_charset_transliterates_and_preserves_borrowed_utf8() {
        let utf8 = Driver::with_palette(Palette::TrueColor);
        assert!(matches!(utf8.transliterate("┃"), Cow::Borrowed(_)));
        let ascii = Driver::with_palette(Palette::TrueColor).with_charset(CharsetStandard::Ascii);
        assert_eq!(ascii.transliterate("┃"), "|");
        assert_eq!(ascii.transliterate("a"), "a");
        assert_eq!(ascii.transliterate("漢"), "?");
        assert!(matches!(ascii.transliterate("a"), Cow::Borrowed(_)));
    }
}
