//! Escape-sequence emission: one capability-driven [`Driver`], plus the
//! [`EscapeEmitter`] extension point.
//!
//! # Design
//!
//! Capability *detection* lives in [`crate::render::profile`]. This module only
//! turns a requested [`Style`] into the SGR bytes a given palette needs, and
//! wraps frames in a synchronized-update envelope when the terminal supports one.
//!
//! The four protocol variants this used to be (four structs plus a dispatch
//! enum, with the SGR delta logic copied three times) are now one struct and one
//! [`Palette`] discriminant, because the palette is the *only* thing that
//! differed. Every capability that affects emission is a field here, and every
//! one of them is honoured:
//!
//! - `palette` — how colors map onto the terminal (`Monochrome` strips them and
//!   expresses emphasis through reverse video).
//! - `italic` — when false, italic maps to underline instead of being emitted.
//! - `ascii_only` — when true, [`EscapeEmitter::transliterate`] degrades any
//!   non-ASCII symbol to 7-bit, geometry preserved.

use std::borrow::Cow;
use std::io::{self, Write};

use crossterm::{
    QueueableCommand,
    style::{Attribute, Color as CtColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};

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

    /// Whether this palette can render achromatic distinction (i.e. is not
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

/// The standard escape emitter: SGR delta emission against a [`Palette`], with
/// synchronized updates and symbol transliteration.
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
        if !self.italic {
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

        if want.fg != have.fg {
            out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
        }
        if want.bg != have.bg {
            out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
        }

        // SGR 0 clears *everything*, including the colors just set, so a frame
        // that drops an attribute resets and then re-applies the full style.
        let dropped = have.add - want.add;
        let added = want.add - have.add;
        if dropped.is_empty() {
            for attr in iter_attrs(added) {
                out.queue(SetAttribute(attr))?;
            }
        } else {
            out.queue(SetAttribute(Attribute::Reset))?;
            if want.fg != Color::Reset {
                out.queue(SetForegroundColor(to_ct_color(want.fg)))?;
            }
            if want.bg != Color::Reset {
                out.queue(SetBackgroundColor(to_ct_color(want.bg)))?;
            }
            for attr in iter_attrs(want.add) {
                out.queue(SetAttribute(attr))?;
            }
        }

        self.style = want;
        Ok(())
    }

    fn begin_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.sync {
            out.queue(BeginSynchronizedUpdate).map(|_| ())
        } else {
            Ok(())
        }
    }

    fn end_sync<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.sync {
            out.queue(EndSynchronizedUpdate)?;
        }
        out.flush()
    }

    fn invalidate<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.queue(SetAttribute(Attribute::Reset))?;
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

/// Translate an engine [`Color`] into a crossterm color.
pub fn to_ct_color(c: Color) -> CtColor {
    match c {
        Color::Reset => CtColor::Reset,
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
        Color::Indexed(idx) => CtColor::AnsiValue(idx),
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::Gray => CtColor::Grey,
        Color::DarkGray => CtColor::DarkGrey,
        Color::LightRed => CtColor::Red,
        Color::LightGreen => CtColor::Green,
        Color::LightYellow => CtColor::Yellow,
        Color::LightBlue => CtColor::Blue,
        Color::LightMagenta => CtColor::Magenta,
        Color::LightCyan => CtColor::Cyan,
        Color::White => CtColor::White,
    }
}

/// Translate modifier bits into crossterm `Attribute`s in a stable order, so a
/// given style always emits the same byte sequence.
pub fn iter_attrs(m: Modifier) -> impl Iterator<Item = Attribute> {
    let mut v = Vec::new();
    if m.contains(Modifier::BOLD) {
        v.push(Attribute::Bold);
    }
    if m.contains(Modifier::DIM) {
        v.push(Attribute::Dim);
    }
    if m.contains(Modifier::ITALIC) {
        v.push(Attribute::Italic);
    }
    if m.contains(Modifier::UNDERLINE) {
        v.push(Attribute::Underlined);
    }
    if m.contains(Modifier::REVERSE) {
        v.push(Attribute::Reverse);
    }
    if m.contains(Modifier::STRIKETHROUGH) {
        v.push(Attribute::CrossedOut);
    }
    v.into_iter()
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
        assert!(out.contains("38;5;") || out.contains("3"), "{out:?}");
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
        assert!(!out.contains("38;"), "no fg color code: {out:?}");
        assert!(!out.contains("48;"), "no bg color code: {out:?}");
        assert!(out.contains('7'), "emphasis becomes reverse video: {out:?}");
    }

    #[test]
    fn italic_maps_to_underline_when_unsupported() {
        let style = Style::default().add_modifier(Modifier::ITALIC);
        let mut no_italic = Driver::with_palette(Palette::Ansi16).with_italic(false);
        let out = emit(&mut no_italic, style);
        assert!(out.contains('4'), "italic became underline: {out:?}");
        assert!(!out.contains('3'), "no italic SGR: {out:?}");

        let mut italic = Driver::with_palette(Palette::TrueColor).with_italic(true);
        assert!(emit(&mut italic, style).contains('3'));
    }

    #[test]
    fn dropping_an_attribute_resets_then_reapplies_the_style() {
        let mut d = Driver::with_palette(Palette::TrueColor);
        emit(
            &mut d,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        );
        let out = emit(&mut d, Style::default().fg(Color::Red));
        assert!(out.starts_with("\x1b[0m"), "reset first: {out:?}");
        // crossterm emits the named palette as an indexed foreground; what
        // matters is that a foreground is re-applied after SGR 0 cleared it.
        assert!(
            out.contains("m\x1b[38;5;1m") || out.contains("m\x1b[31m"),
            "fg re-applied after reset: {out:?}"
        );
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
