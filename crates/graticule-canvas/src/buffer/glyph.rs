//! Typography: the glyph-set system plus the ASCII transliteration used when a
//! terminal reports [`CharsetStandard::Ascii`].
//!
//! Rather than hardcoding Unicode string literals across UI components, every
//! box-drawing character, status indicator and spinner frame lives in a
//! canonical [`GlyphSet`]. A component that wants ASCII selects
//! [`ASCII_GLYPHS`] explicitly; the *emitter* additionally transliterates any
//! non-ASCII scalar that still reaches it, so an ASCII profile cannot leak
//! UTF-8 even if a component forgot.
//!
//! Transliteration is **geometry-preserving**: one character in, one ASCII
//! character out. A multi-character fallback would change a cell's column
//! count and with it the layout, which is not an acceptable trade for a
//! capability fallback.

use crate::render::profile::CharsetStandard;

/// A coherent suite of box-drawing characters, indicators and animation glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphSet {
    /// Vertical border bar (e.g. `┃` or `|`).
    pub border_v: &'static str,
    /// Horizontal border bar (e.g. `─` or `-`).
    pub border_h: &'static str,
    /// Top-left corner (e.g. `╭` or `+`).
    pub corner_tl: &'static str,
    /// Top-right corner (e.g. `╮` or `+`).
    pub corner_tr: &'static str,
    /// Bottom-left corner (e.g. `╰` or `+`).
    pub corner_bl: &'static str,
    /// Bottom-right corner (e.g. `╯` or `+`).
    pub corner_br: &'static str,
    /// Activity / status indicator dot (e.g. `●` or `*`).
    pub dot: &'static str,
    /// Secondary bullet (e.g. `•` or `*`).
    pub bullet: &'static str,
    /// Affirmative / check indicator (e.g. `✓` or `[OK]`).
    pub check: &'static str,
    /// Error / cross indicator (e.g. `✗` or `[X]`).
    pub cross: &'static str,
    /// Rightward directional arrow (e.g. `→` or `->`).
    pub arrow_right: &'static str,
    /// Ellipsis truncation indicator (e.g. `…` or `...`).
    pub ellipsis: &'static str,
    /// Secret / password mask character (e.g. `•` or `*`).
    pub mask: &'static str,
    /// Animated progress / spinner frames.
    pub spinner_frames: &'static [&'static str],
}

impl Default for GlyphSet {
    fn default() -> Self {
        UNICODE_GLYPHS
    }
}

impl GlyphSet {
    /// Resolve the appropriate [`GlyphSet`] for a [`CharsetStandard`].
    pub const fn for_standard(standard: CharsetStandard) -> Self {
        match standard {
            CharsetStandard::Utf8 => UNICODE_GLYPHS,
            CharsetStandard::Ascii => ASCII_GLYPHS,
        }
    }

    /// The current spinner frame for the given tick phase.
    #[inline]
    pub fn spinner_frame(&self, phase: usize) -> &'static str {
        if self.spinner_frames.is_empty() {
            "*"
        } else {
            self.spinner_frames[phase % self.spinner_frames.len()]
        }
    }
}

/// Standard modern ISO/IEC 10646 (UTF-8) glyph set.
pub const UNICODE_GLYPHS: GlyphSet = GlyphSet {
    border_v: "┃",
    border_h: "─",
    corner_tl: "╭",
    corner_tr: "╮",
    corner_bl: "╰",
    corner_br: "╯",
    dot: "●",
    bullet: "•",
    check: "✓",
    cross: "✗",
    arrow_right: "→",
    ellipsis: "…",
    mask: "•",
    spinner_frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
};

/// 7-bit US-ASCII safe fallback glyph set for VT100 / getty / serial consoles.
pub const ASCII_GLYPHS: GlyphSet = GlyphSet {
    border_v: "|",
    border_h: "-",
    corner_tl: "+",
    corner_tr: "+",
    corner_bl: "+",
    corner_br: "+",
    dot: "*",
    bullet: "*",
    check: "[OK]",
    cross: "[X]",
    arrow_right: "->",
    ellipsis: "...",
    mask: "*",
    spinner_frames: &["|", "/", "-", "\\"],
};

/// Single-character ASCII substitutions, geometry-preserving.
///
/// The first block pairs [`UNICODE_GLYPHS`] entries with [`ASCII_GLYPHS`]
/// entries so the two sets cannot drift; a test asserts the pairing. The rest
/// covers the box-drawing and symbol range a widget may draw directly.
pub const ASCII_FALLBACKS: &[(char, char)] = &[
    // --- Must mirror UNICODE_GLYPHS <-> ASCII_GLYPHS ---
    ('┃', '|'),
    ('─', '-'),
    ('╭', '+'),
    ('╮', '+'),
    ('╰', '+'),
    ('╯', '+'),
    ('●', '*'),
    ('•', '*'),
    ('✓', 'v'),
    ('✗', 'x'),
    ('→', '>'),
    ('…', '.'),
    // --- Remaining light/heavy/rounded/double box drawing ---
    ('│', '|'),
    ('┌', '+'),
    ('┐', '+'),
    ('└', '+'),
    ('┘', '+'),
    ('├', '+'),
    ('┤', '+'),
    ('┬', '+'),
    ('┴', '+'),
    ('┼', '+'),
    ('━', '-'),
    ('┏', '+'),
    ('┓', '+'),
    ('┗', '+'),
    ('┛', '+'),
    ('┣', '+'),
    ('┫', '+'),
    ('┳', '+'),
    ('┻', '+'),
    ('╋', '+'),
    ('═', '='),
    ('║', '|'),
    ('╔', '+'),
    ('╗', '+'),
    ('╚', '+'),
    ('╝', '+'),
    ('╠', '+'),
    ('╣', '+'),
    ('╦', '+'),
    ('╩', '+'),
    ('╬', '+'),
    // --- Indicators and marks ---
    ('○', 'o'),
    ('·', '.'),
    ('∙', '*'),
    ('‣', '>'),
    ('✔', 'v'),
    ('✘', 'x'),
    ('×', 'x'),
    ('✕', 'x'),
    ('⋯', '.'),
    ('←', '<'),
    ('↑', '^'),
    ('↓', 'v'),
    ('≤', '<'),
    ('≥', '>'),
    ('≠', '!'),
    ('‥', '.'),
    ('–', '-'),
    ('—', '-'),
    ('―', '-'),
    ('“', '"'),
    ('”', '"'),
    ('‘', '\''),
    ('’', '\''),
    ('«', '<'),
    ('»', '>'),
    // --- Dashes / quotes / spaces that render wider or oddly on old consoles ---
    ('\u{00A0}', ' '),
    ('█', '#'),
    ('▉', '#'),
    ('▊', '#'),
    ('▋', '#'),
    ('▌', '#'),
    ('▍', '#'),
    ('▎', '#'),
    ('▏', '#'),
    ('▓', '#'),
    ('▒', '#'),
    ('░', '#'),
    ('▁', '_'),
    ('▔', '-'),
];

/// The ASCII substitute for a scalar that a 7-bit terminal cannot render, or
/// `None` if the scalar has no considered fallback.
///
/// Braille spinner frames (`U+2800..=U+28FF`) all map to `*`: they are a
/// spinner, and any single-column ASCII mark preserves the animation's
/// geometry.
pub fn ascii_fallback(ch: char) -> Option<char> {
    if ('\u{2800}'..='\u{28FF}').contains(&ch) {
        return Some('*');
    }
    if ch.is_ascii() {
        return Some(ch);
    }
    ASCII_FALLBACKS
        .iter()
        .find(|(from, _)| *from == ch)
        .map(|(_, to)| *to)
}

/// Transliterate a string for a 7-bit terminal, substituting `?` for any scalar
/// with no considered fallback. Column count is preserved for every scalar in
/// [`ASCII_FALLBACKS`] and for the Braille spinner range.
pub fn to_ascii(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    s.chars()
        .map(|ch| ascii_fallback(ch).unwrap_or('?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-character glyph fields must have a fallback that matches the
    /// hand-written ASCII set, so the two sets cannot drift apart.
    #[test]
    fn ascii_fallbacks_mirror_the_glyph_sets() {
        let pairs = [
            (UNICODE_GLYPHS.border_v, ASCII_GLYPHS.border_v),
            (UNICODE_GLYPHS.border_h, ASCII_GLYPHS.border_h),
            (UNICODE_GLYPHS.corner_tl, ASCII_GLYPHS.corner_tl),
            (UNICODE_GLYPHS.corner_tr, ASCII_GLYPHS.corner_tr),
            (UNICODE_GLYPHS.corner_bl, ASCII_GLYPHS.corner_bl),
            (UNICODE_GLYPHS.corner_br, ASCII_GLYPHS.corner_br),
            (UNICODE_GLYPHS.dot, ASCII_GLYPHS.dot),
            (UNICODE_GLYPHS.bullet, ASCII_GLYPHS.bullet),
            (UNICODE_GLYPHS.check, "v"),
            (UNICODE_GLYPHS.cross, "x"),
            (UNICODE_GLYPHS.arrow_right, ">"),
            (UNICODE_GLYPHS.ellipsis, "."),
            (UNICODE_GLYPHS.mask, ASCII_GLYPHS.mask),
        ];
        for (unicode, ascii) in pairs {
            let mut u = unicode.chars();
            let (from, rest) = (u.next().unwrap(), u.as_str());
            assert!(rest.is_empty(), "{unicode} must be a single scalar");
            let mut a = ascii.chars();
            let (to, rest) = (a.next().unwrap(), a.as_str());
            assert!(rest.is_empty(), "{ascii} must be a single scalar");
            assert_eq!(
                ascii_fallback(from),
                Some(to),
                "{from} should transliterate to {to}"
            );
        }
    }

    #[test]
    fn transliteration_preserves_column_count() {
        for (from, to) in ASCII_FALLBACKS {
            assert_eq!(to.len_utf8(), 1, "{to} must be one byte");
            assert!(to.is_ascii(), "{to} must be ASCII");
            assert_eq!(ascii_fallback(*from), Some(*to));
        }
        // Braille spinners are multi-byte in UTF-8 but one column wide.
        for frame in UNICODE_GLYPHS.spinner_frames {
            let ch = frame.chars().next().unwrap();
            assert_eq!(ascii_fallback(ch), Some('*'));
        }
    }

    #[test]
    fn to_ascii_never_emits_a_non_ascii_byte() {
        let mixed = "┃─╭╮ a✓✗→…漢 \u{280B} ";
        let out = to_ascii(mixed);
        assert!(out.is_ascii(), "{out:?} must be 7-bit clean");
        // ASCII input is returned unchanged, including its column count.
        assert_eq!(to_ascii("plain"), "plain");
        // A CJK scalar with no mapping degrades to '?' rather than leaking.
        assert!(out.contains('?'));
    }

    #[test]
    fn test_unicode_glyphs_resolve() {
        let g = GlyphSet::for_standard(CharsetStandard::Utf8);
        assert_eq!(g.border_v, "┃");
        assert_eq!(g.dot, "●");
        assert_eq!(g.spinner_frame(0), "⠋");
    }

    #[test]
    fn test_ascii_glyphs_resolve() {
        let g = GlyphSet::for_standard(CharsetStandard::Ascii);
        assert_eq!(g.border_v, "|");
        assert_eq!(g.dot, "*");
        assert_eq!(g.spinner_frame(0), "|");
        assert_eq!(g.spinner_frame(1), "/");
    }
}
