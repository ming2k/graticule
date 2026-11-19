//! Terminal capability profiles: what a terminal can render, and how to find
//! out.
//!
//! # Specification grounding
//!
//! Rather than ad-hoc emulator sniffing (`is_kitty`, `is_iterm2`), capabilities
//! are anchored to formal standards:
//!
//! - **ITU-T T.416 / ISO/IEC 8613-6**: Direct Color (`\x1b[38;2;r;g;bm`), 24-bit.
//! - **ECMA-48 / ISO/IEC 6429**: 16-color SGR codes and text formatting.
//! - **ANSI X3.64 / DEC VT100**: baseline physical terminal for serial/getty.
//!   Zero color codes, emphasis via `SGR 7` (reverse), `SGR 4`, `SGR 1`.
//! - **ANSI X3.4 (ASCII) vs ISO/IEC 10646 (UTF-8)**: character-set constraints.
//!
//! # Purity
//!
//! [`TerminalProfile::for_env`] is a *pure* function of its arguments and is what
//! every test exercises. [`TerminalProfile::detect`] is the only impure entry
//! point: it reads the process environment once and delegates. A capability
//! profile that silently consulted the environment from inside a "pure" resolver
//! made tests depend on the ambient shell; that is why the split is explicit.
//!
//! # Capabilities are implemented, not advertised
//!
//! Every flag on [`TerminalProfile`] has an observable effect in
//! [`crate::render::driver::Driver`], which the profile constructs. A profile
//! field with no consumer is a defect, not a feature: earlier revisions shipped
//! `charset_standard` and `supports_italic` that nothing enforced.

use crate::Color;

/// Standard terminal color reproduction capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorStandard {
    /// ITU-T T.416 Direct Color: 24-bit TrueColor via `\x1b[38;2;r;g;bm` and `\x1b[48;2;r;g;bm`.
    #[default]
    DirectColor,
    /// 8-bit Indexed 256-color palette (`\x1b[38;5;{n}m`, `\x1b[48;5;{n}m`).
    Indexed256,
    /// ECMA-48 5th Edition 8-color + aixterm 16-color (`\x1b[30..37m`, `\x1b[90..97m`).
    Ansi16,
    /// DEC VT100 Monochrome: Zero color codes emitted; visual emphasis uses SGR 7 (Reverse Video).
    Monochrome,
}

impl ColorStandard {
    /// Whether this color standard supports chromatic colors (non-monochrome).
    #[inline]
    pub const fn supports_color(self) -> bool {
        !matches!(self, Self::Monochrome)
    }

    /// Whether this color standard supports visual color themes (requires at least ANSI 16).
    #[inline]
    pub const fn supports_color_themes(self) -> bool {
        !matches!(self, Self::Monochrome)
    }

    /// Whether this color standard supports 24-bit TrueColor.
    #[inline]
    pub const fn supports_truecolor(self) -> bool {
        matches!(self, Self::DirectColor)
    }

    /// Whether this color standard supports at least 256 colors.
    #[inline]
    pub const fn supports_256_color(self) -> bool {
        matches!(self, Self::DirectColor | Self::Indexed256)
    }

    /// Whether this color standard is 8-bit Indexed 256-color.
    #[inline]
    pub const fn is_indexed256(self) -> bool {
        matches!(self, Self::Indexed256)
    }

    /// Whether this color standard is monochromatic.
    #[inline]
    pub const fn is_monochrome(self) -> bool {
        matches!(self, Self::Monochrome)
    }

    /// Whether this color standard is 24-bit TrueColor.
    #[inline]
    pub const fn is_direct_color(self) -> bool {
        matches!(self, Self::DirectColor)
    }

    /// Whether this color standard is 16-color ANSI.
    #[inline]
    pub const fn is_ansi16(self) -> bool {
        matches!(self, Self::Ansi16)
    }

    /// Color depth in bits.
    #[inline]
    pub const fn color_depth_bits(self) -> u8 {
        match self {
            Self::DirectColor => 24,
            Self::Indexed256 => 8,
            Self::Ansi16 => 4,
            Self::Monochrome => 1,
        }
    }

    /// Nominal number of distinct colors addressable by the standard.
    #[inline]
    pub const fn color_count(self) -> u32 {
        match self {
            Self::DirectColor => 16_777_216,
            Self::Indexed256 => 256,
            Self::Ansi16 => 16,
            Self::Monochrome => 2,
        }
    }
}

/// Standard terminal character set capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharsetStandard {
    /// ISO/IEC 10646 (UTF-8) full character set including box drawing and symbols.
    #[default]
    Utf8,
    /// ANSI X3.4 7-bit US-ASCII safe fallback (+, -, |, etc.).
    Ascii,
}

/// A deterministic capability profile governing escape sequence emission and widget degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProfile {
    /// Color reproduction capability.
    pub color_standard: ColorStandard,
    /// Character set capability.
    pub charset_standard: CharsetStandard,
    /// Whether `SGR 3` renders as italic.
    pub supports_italic: bool,
    /// Whether DEC mode 2026 synchronized updates are available.
    pub supports_sync_update: bool,
    /// Whether the application may enable mouse capture.
    pub supports_mouse: bool,
}

impl Default for TerminalProfile {
    fn default() -> Self {
        Self::detect()
    }
}

impl TerminalProfile {
    /// Profile 1: Full ITU-T T.416 Direct Color + UTF-8 + Mode 2026.
    pub const fn direct_color() -> Self {
        Self {
            color_standard: ColorStandard::DirectColor,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: true,
            supports_sync_update: true,
            supports_mouse: true,
        }
    }

    /// Profile 1.5: 8-bit Indexed 256-color profile (xterm-256color, tmux, mosh).
    pub const fn indexed256() -> Self {
        Self {
            color_standard: ColorStandard::Indexed256,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: true,
            supports_sync_update: true,
            supports_mouse: true,
        }
    }

    /// Profile 2: ECMA-48 Standard 16-Color profile.
    pub const fn ecma48_ansi16() -> Self {
        Self {
            color_standard: ColorStandard::Ansi16,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: true,
        }
    }

    /// Profile 3: DEC VT100 Monochrome baseline profile (for getty, serial, rescue).
    pub const fn dec_vt100_monochrome() -> Self {
        Self {
            color_standard: ColorStandard::Monochrome,
            charset_standard: CharsetStandard::Ascii,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: false,
        }
    }

    /// Return the active color model for this terminal profile.
    pub const fn supports_color(&self) -> bool {
        self.color_standard.supports_color()
    }

    /// Whether this terminal supports visual color themes (requires at least ANSI 16).
    #[inline]
    pub const fn supports_color_themes(&self) -> bool {
        self.color_standard.supports_color_themes()
    }

    /// Whether this terminal supports arbitrary 24-bit TrueColor palette customization.
    #[inline]
    pub const fn supports_custom_palettes(&self) -> bool {
        self.color_standard.is_direct_color()
    }

    /// Whether this terminal reproduces full ITU-T T.416 24-bit TrueColor.
    #[inline]
    pub const fn supports_truecolor(&self) -> bool {
        self.color_standard.is_direct_color()
    }

    /// Whether this terminal supports at least 256 colors.
    #[inline]
    pub const fn supports_256_color(&self) -> bool {
        self.color_standard.supports_256_color()
    }

    /// Whether this terminal reproduces 8-bit Indexed 256-color palette.
    #[inline]
    pub const fn is_indexed256(&self) -> bool {
        self.color_standard.is_indexed256()
    }

    /// Whether this terminal reproduces full ITU-T T.416 24-bit TrueColor.
    #[inline]
    pub const fn is_direct_color(&self) -> bool {
        self.color_standard.is_direct_color()
    }

    /// Whether this terminal is restricted to ECMA-48 16-color ANSI rendering.
    #[inline]
    pub const fn is_ansi16(&self) -> bool {
        self.color_standard.is_ansi16()
    }

    /// Whether this terminal is operating in DEC VT100 / NO_COLOR monochromatic mode.
    #[inline]
    pub const fn is_monochrome(&self) -> bool {
        self.color_standard.is_monochrome()
    }

    /// Approximate color depth in bits (DirectColor: 24, Indexed256: 8, Ansi16: 4, Monochrome: 1).
    #[inline]
    pub const fn color_depth_bits(&self) -> u8 {
        self.color_standard.color_depth_bits()
    }

    /// Nominal number of distinct colors addressable by the profile.
    #[inline]
    pub const fn color_count(&self) -> u32 {
        self.color_standard.color_count()
    }

    /// Detect the capability profile from the process environment.
    ///
    /// This is the only impure entry point: it gathers `TERM`, `COLORTERM`,
    /// `NO_COLOR` and the `GRATICULE_*` overrides, then delegates to the pure
    /// [`Self::for_env`].
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let color_override = env_first(&["GRATICULE_COLOR_STANDARD"]);
        let charset_override = env_first(&["GRATICULE_CHARSET_STANDARD"]);
        let sync_override = env_first(&["GRATICULE_SYNC_UPDATE"]).map(|v| is_truthy(&v));

        Self::for_env(
            &term,
            &colorterm,
            no_color,
            color_override.as_deref(),
            charset_override.as_deref(),
            sync_override,
        )
    }

    /// Resolve a capability profile from explicit inputs.
    ///
    /// Pure: given the same arguments it always returns the same profile, so it
    /// is what tests and deterministic overrides use.
    ///
    /// # Resolution precedence
    ///
    /// 1. An explicit `color_override` wins outright.
    /// 2. `NO_COLOR` (the <https://no-color.org> standard) forces monochrome.
    /// 3. A physical or serial terminal (`vt100`, `ttyS*`) is monochrome with an
    ///    ASCII character set.
    /// 4. `COLORTERM=truecolor|24bit` means 24-bit.
    /// 5. `TERM` naming a 256-color variant means 256 colors.
    /// 6. A terminal whose terminfo entry declares 8 colors (`linux`, `xterm`,
    ///    `*16color`, `screen`, `tmux`, `rxvt`, `cons25`, `cygwin`) means 16.
    /// 7. An empty or `dumb` `TERM` means monochrome.
    /// 8. Anything else non-empty is assumed 256-color: a 256-color code
    ///    degrades to the nearest of 16 on a console that cannot take it, which
    ///    is a fidelity loss rather than a rendering fault.
    ///
    /// The last rule is a deliberate rewrite of the previous behaviour, which
    /// claimed 24-bit color for any unrecognised `TERM` — a fidelity promise no
    /// emulator had made.
    pub fn for_env(
        term: &str,
        colorterm: &str,
        no_color: bool,
        color_override: Option<&str>,
        charset_override: Option<&str>,
        sync_override: Option<bool>,
    ) -> Self {
        let color_standard = if let Some(ov) = color_override {
            match ov {
                "direct" | "truecolor" | "24bit" => ColorStandard::DirectColor,
                "256" | "indexed" | "indexed256" => ColorStandard::Indexed256,
                "ansi16" | "16" | "basic" => ColorStandard::Ansi16,
                "monochrome" | "mono" | "0" => ColorStandard::Monochrome,
                // An unrecognised override must not silently upgrade: fall back
                // to the same conservative default as an unknown `TERM`.
                _ => ColorStandard::Indexed256,
            }
        } else if no_color || is_vt100_or_serial(term) {
            // `NO_COLOR` (https://no-color.org) forces monochrome, and so does a
            // physical or serial terminal, whose terminfo has no color at all.
            ColorStandard::Monochrome
        } else if colorterm == "truecolor" || colorterm == "24bit" {
            ColorStandard::DirectColor
        } else if term.contains("256color") {
            ColorStandard::Indexed256
        } else if is_ansi16_term(term) {
            ColorStandard::Ansi16
        } else if term.is_empty() || term == "dumb" {
            ColorStandard::Monochrome
        } else {
            ColorStandard::Indexed256
        };

        let charset_standard = if let Some(ov) = charset_override {
            match ov {
                "ascii" => CharsetStandard::Ascii,
                _ => CharsetStandard::Utf8,
            }
        } else if is_vt100_or_serial(term) || term == "dumb" {
            CharsetStandard::Ascii
        } else {
            CharsetStandard::Utf8
        };

        let supports_italic = matches!(
            color_standard,
            ColorStandard::DirectColor | ColorStandard::Indexed256
        ) && term != "linux"
            && !is_vt100_or_serial(term);

        let supports_sync_update = sync_override
            .unwrap_or_else(|| !is_vt100_or_serial(term) && term != "dumb" && term != "linux");

        let supports_mouse = !is_vt100_or_serial(term) && term != "dumb";

        Self {
            color_standard,
            charset_standard,
            supports_italic,
            supports_sync_update,
            supports_mouse,
        }
    }
}

/// The first environment variable in `names` that is set and non-empty.
fn env_first(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

/// Whether an override value reads as "on".
fn is_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "yes" | "on")
}

/// Terminal names whose terminfo entry declares no more than 16 colors.
fn is_ansi16_term(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    lower == "linux"
        || lower == "cons25"
        || lower == "cygwin"
        || lower == "xterm"
        || lower.contains("16color")
        || lower.starts_with("screen")
        || lower.starts_with("tmux")
        || lower.starts_with("rxvt")
}

/// Helper to identify VT100 physical terminals, serial devices, and unadorned consoles.
fn is_vt100_or_serial(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    lower == "vt100"
        || lower == "vt102"
        || lower == "vt220"
        || lower.starts_with("ttys")
        || lower.starts_with("ttyusb")
        || lower == "serial"
}

/// Quantize any [`Color`] down to the 8-bit 256-color palette (0..255).
pub fn quantize_to_indexed256(color: Color) -> Color {
    match color {
        Color::Reset => Color::Reset,
        Color::Indexed(idx) => Color::Indexed(idx),
        Color::Rgb(r, g, b) => Color::Indexed(nearest_indexed256(r, g, b)),
        named => named,
    }
}

/// Find nearest 256-color palette index for given (r, g, b).
pub fn nearest_indexed256(r: u8, g: u8, b: u8) -> u8 {
    // 1. Candidate from 6x6x6 color cube (indices 16..231)
    let cube_index = |v: u8| -> (u8, u8) {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut best_i = 0;
        let mut best_d = (v as i32 - STEPS[0] as i32).abs();
        for (i, &step) in STEPS.iter().enumerate().skip(1) {
            let d = (v as i32 - step as i32).abs();
            if d < best_d {
                best_d = d;
                best_i = i;
            }
        }
        (best_i as u8, STEPS[best_i])
    };
    let (cr, vr) = cube_index(r);
    let (cg, vg) = cube_index(g);
    let (cb, vb) = cube_index(b);
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let dist_cube = (r as i32 - vr as i32).pow(2)
        + (g as i32 - vg as i32).pow(2)
        + (b as i32 - vb as i32).pow(2);

    // 2. Candidate from 24-step grayscale ramp (indices 232..255)
    let avg = ((r as u32 + g as u32 + b as u32) / 3) as u8;
    let gray_idx = if avg < 8 {
        0
    } else if avg > 238 {
        23
    } else {
        ((avg - 8 + 5) / 10).min(23)
    };
    let gray_val = 8 + gray_idx * 10;
    let dist_gray = (r as i32 - gray_val as i32).pow(2)
        + (g as i32 - gray_val as i32).pow(2)
        + (b as i32 - gray_val as i32).pow(2);

    if dist_gray < dist_cube {
        232 + gray_idx
    } else {
        cube_idx
    }
}

/// Quantize any [`Color`] down to the ECMA-48 / aixterm 16 ANSI color space.
pub fn quantize_to_ansi16(color: Color) -> Color {
    match color {
        Color::Reset => Color::Reset,
        Color::Rgb(r, g, b) => nearest_ansi16(r, g, b),
        named => named,
    }
}

/// Find nearest ANSI 16 color using perceptual weighted Euclidean distance.
fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    const ANSI16_TABLE: &[(Color, u8, u8, u8)] = &[
        (Color::Black, 0, 0, 0),
        (Color::Red, 170, 0, 0),
        (Color::Green, 0, 170, 0),
        (Color::Yellow, 170, 85, 0),
        (Color::Blue, 0, 0, 170),
        (Color::Magenta, 170, 0, 170),
        (Color::Cyan, 0, 170, 170),
        (Color::Gray, 170, 170, 170),
        (Color::DarkGray, 85, 85, 85),
        (Color::LightRed, 255, 85, 85),
        (Color::LightGreen, 85, 255, 85),
        (Color::LightYellow, 255, 255, 85),
        (Color::LightBlue, 85, 85, 255),
        (Color::LightMagenta, 255, 85, 255),
        (Color::LightCyan, 85, 255, 255),
        (Color::White, 255, 255, 255),
    ];

    let mut best_color = Color::White;
    let mut best_dist = u32::MAX;

    for &(c, pr, pg, pb) in ANSI16_TABLE {
        // Redmean-style weighted perceptual distance:
        let r_mean = (r as i32 + pr as i32) / 2;
        let dr = r as i32 - pr as i32;
        let dg = g as i32 - pg as i32;
        let db = b as i32 - pb as i32;
        let dist =
            (((512 + r_mean) * dr * dr) >> 8) + 4 * dg * dg + (((767 - r_mean) * db * db) >> 8);

        let dist_u32 = dist as u32;
        if dist_u32 < best_dist {
            best_dist = dist_u32;
            best_color = c;
        }
    }

    best_color
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::driver::{Driver, EscapeEmitter, Palette};

    /// Shorthand for the pure resolver with no overrides.
    fn env(term: &str, colorterm: &str) -> TerminalProfile {
        TerminalProfile::for_env(term, colorterm, false, None, None, None)
    }

    #[test]
    fn test_vt100_detection() {
        let p = env("vt100", "");
        assert_eq!(p.color_standard, ColorStandard::Monochrome);
        assert_eq!(p.charset_standard, CharsetStandard::Ascii);
        assert!(!p.supports_italic);
        assert!(!p.supports_sync_update);
        assert!(!p.supports_mouse);
    }

    #[test]
    fn test_no_color_forces_monochrome() {
        let p = TerminalProfile::for_env("xterm-256color", "truecolor", true, None, None, None);
        assert_eq!(p.color_standard, ColorStandard::Monochrome);
    }

    #[test]
    fn test_linux_console_detection() {
        let p = env("linux", "");
        assert_eq!(p.color_standard, ColorStandard::Ansi16);
        assert!(!p.supports_italic);
        assert!(!p.supports_sync_update);
    }

    #[test]
    fn test_direct_color_detection() {
        let p = env("xterm-256color", "truecolor");
        assert_eq!(p.color_standard, ColorStandard::DirectColor);
        assert_eq!(p.charset_standard, CharsetStandard::Utf8);
        assert!(p.supports_italic);
        assert!(p.supports_sync_update);
        assert!(p.supports_mouse);
    }

    #[test]
    fn detection_defaults_are_conservative() {
        // 256-color variants resolve to 256.
        assert_eq!(
            env("xterm-256color", "").color_standard,
            ColorStandard::Indexed256
        );
        // A terminfo entry that declares 8 colors stays 16 even under tmux.
        assert_eq!(env("tmux", "").color_standard, ColorStandard::Ansi16);
        assert_eq!(env("screen", "").color_standard, ColorStandard::Ansi16);
        assert_eq!(env("xterm", "").color_standard, ColorStandard::Ansi16);
        assert_eq!(env("linux", "").color_standard, ColorStandard::Ansi16);
        // ...but through tmux's 256-color entry it is 256, and an explicit
        // COLORTERM upgrades further.
        assert_eq!(
            env("tmux-256color", "").color_standard,
            ColorStandard::Indexed256
        );
        assert_eq!(
            env("tmux-256color", "truecolor").color_standard,
            ColorStandard::DirectColor
        );
        // An unrecognised TERM must not claim 24-bit color it never advertised.
        assert_eq!(
            env("weird-term", "").color_standard,
            ColorStandard::Indexed256
        );
        // Empty or dumb is monochrome.
        assert_eq!(env("", "").color_standard, ColorStandard::Monochrome);
        assert_eq!(env("dumb", "").color_standard, ColorStandard::Monochrome);
        // The sync override wins over the environment guess in both directions.
        let forced_off =
            TerminalProfile::for_env("xterm-256color", "", false, None, None, Some(false));
        assert!(!forced_off.supports_sync_update);
        let forced_on = TerminalProfile::for_env("vt100", "", false, None, None, Some(true));
        assert!(forced_on.supports_sync_update);
        // Explicit overrides beat every heuristic.
        let forced =
            TerminalProfile::for_env("vt100", "", true, Some("ansi16"), Some("utf8"), None);
        assert_eq!(forced.color_standard, ColorStandard::Ansi16);
        assert_eq!(forced.charset_standard, CharsetStandard::Utf8);
    }

    /// Every declared capability must have an observable effect on the driver the
    /// profile builds (`charset_standard`, `supports_italic`, `supports_sync_update`,
    /// and `color_standard`).
    #[test]
    fn declared_capabilities_are_honoured_by_the_driver_they_produce() {
        let mono = TerminalProfile::dec_vt100_monochrome();
        let driver = Driver::for_profile(&mono);
        assert_eq!(driver.palette(), Palette::Monochrome);
        assert!(!driver.supports_sync_update());
        assert_eq!(driver.charset_standard(), CharsetStandard::Ascii);
        assert_eq!(driver.transliterate("┃"), "|");

        let ansi = TerminalProfile::ecma48_ansi16();
        let driver = Driver::for_profile(&ansi);
        assert_eq!(driver.palette(), Palette::Ansi16);
        assert!(!driver.supports_sync_update());
        // UTF-8 profile: transliteration is a no-op borrow.
        assert_eq!(driver.transliterate("┃"), "┃");

        let direct = TerminalProfile::direct_color();
        let driver = Driver::for_profile(&direct);
        assert_eq!(driver.palette(), Palette::TrueColor);
        assert!(driver.supports_sync_update());
    }

    #[test]
    fn test_quantize_rgb_to_ansi16() {
        assert_eq!(quantize_to_ansi16(Color::Rgb(255, 255, 255)), Color::White);
        assert_eq!(quantize_to_ansi16(Color::Rgb(0, 0, 0)), Color::Black);
        assert_eq!(quantize_to_ansi16(Color::Rgb(170, 0, 0)), Color::Red);
        assert_eq!(quantize_to_ansi16(Color::Rgb(255, 85, 85)), Color::LightRed);
        assert_eq!(quantize_to_ansi16(Color::Reset), Color::Reset);
    }

    #[test]
    fn test_quantize_rgb_to_indexed256() {
        assert_eq!(quantize_to_indexed256(Color::Reset), Color::Reset);
        assert_eq!(
            quantize_to_indexed256(Color::Indexed(200)),
            Color::Indexed(200)
        );
        // Pure black/white land on the cube's corners.
        assert_eq!(
            quantize_to_indexed256(Color::Rgb(0, 0, 0)),
            Color::Indexed(16)
        );
        assert_eq!(
            quantize_to_indexed256(Color::Rgb(255, 255, 255)),
            Color::Indexed(231)
        );
        // A mid grey resolves to the 24-step grey ramp, not the colour cube.
        match quantize_to_indexed256(Color::Rgb(128, 128, 128)) {
            Color::Indexed(idx) => assert!(idx >= 232, "grey ramp expected, got {idx}"),
            other => panic!("expected an indexed colour, got {other:?}"),
        }
    }

    #[test]
    fn test_profile_capability_predicates() {
        let direct = TerminalProfile::direct_color();
        assert!(direct.supports_color());
        assert!(direct.supports_color_themes());
        assert!(direct.supports_custom_palettes());
        assert!(direct.supports_truecolor());
        assert!(direct.is_direct_color());
        assert!(!direct.is_ansi16());
        assert!(!direct.is_monochrome());
        assert_eq!(direct.color_depth_bits(), 24);
        assert_eq!(direct.color_count(), 16_777_216);

        let ansi16 = TerminalProfile::ecma48_ansi16();
        assert!(ansi16.supports_color());
        assert!(ansi16.supports_color_themes());
        assert!(!ansi16.supports_custom_palettes());
        assert!(!ansi16.supports_truecolor());
        assert!(!ansi16.is_direct_color());
        assert!(ansi16.is_ansi16());
        assert!(!ansi16.is_monochrome());
        assert_eq!(ansi16.color_depth_bits(), 4);
        assert_eq!(ansi16.color_count(), 16);

        let mono = TerminalProfile::dec_vt100_monochrome();
        assert!(!mono.supports_color());
        assert!(!mono.supports_color_themes());
        assert!(!mono.supports_custom_palettes());
        assert!(!mono.supports_truecolor());
        assert!(!mono.is_direct_color());
        assert!(!mono.is_ansi16());
        assert!(mono.is_monochrome());
        assert_eq!(mono.color_depth_bits(), 1);
        assert_eq!(mono.color_count(), 2);

        let indexed = TerminalProfile::indexed256();
        assert!(indexed.supports_color());
        assert!(indexed.supports_256_color());
        assert!(indexed.is_indexed256());
        assert!(!indexed.supports_truecolor());
        assert_eq!(indexed.color_depth_bits(), 8);
        assert_eq!(indexed.color_count(), 256);
    }
}
