//! End-to-end smoke tests across terminal standards and capability profiles.
//!
//! Each scenario takes a *profile*, builds the driver that profile implies, and
//! asserts the bytes that actually leave the backend — so a capability that is
//! declared but not honoured fails here.
//!
//! Scenarios:
//! 1. ITU-T T.416 DirectColor (24-bit, Mode 2026, UTF-8, italic)
//! 2. ECMA-48 ANSI-16 (`TERM=linux`: quantized color, italic mapped to underline)
//! 3. DEC VT100 / getty / serial (`TERM=vt100`: no color, SGR 7 reverse, 7-bit)
//! 4. `NO_COLOR=1` cross-tool standard
//! 5. Fixed 80x24 classical serial console geometry
//! 6. 8-bit Indexed-256 emission and quantization
//! 7. ASCII charsets are enforced by transliteration, not merely declared

use graticule_canvas::render::driver::{Driver, EscapeEmitter};
use graticule_canvas::render::profile::{CharsetStandard, ColorStandard, TerminalProfile};
use graticule_canvas::widgets::{Block, Borders};
use graticule_canvas::{
    ASCII_GLYPHS, Backend, Bce, CellGrid, Color, Damage, Draw, DrawCmd, Modifier, Paint, Rect,
    Style, UNICODE_GLYPHS, Widget,
};

/// Render one multi-feature frame through the driver a profile implies.
///
/// The frame deliberately contains a border glyph, plain text, an RGB
/// background highlight and an italic run, so every capability a profile can
/// claim shows up in the bytes.
fn render_test_frame(profile: &TerminalProfile) -> Vec<u8> {
    let mut sink = Vec::new();
    let cmd = DrawCmd {
        w: 80,
        h: 24,
        draws: vec![
            // A border glyph, indicating the character set in use.
            Draw::Cells {
                x: 0,
                y: 0,
                style: Style::default().fg(Color::Cyan),
                cells: vec![(
                    UNICODE_GLYPHS.border_v.into(),
                    graticule_canvas::CellWidth::Narrow,
                )],
            },
            // Plain text.
            Draw::Cells {
                x: 2,
                y: 0,
                style: Style::default().fg(Color::White),
                cells: vec![("Status:".into(), graticule_canvas::CellWidth::Narrow)],
            },
            // A highlighted selection: expressed with a background, which a
            // monochrome terminal must turn into reverse video.
            Draw::Cells {
                x: 10,
                y: 0,
                style: Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .bg(Color::Rgb(38, 48, 44)),
                cells: vec![("Active".into(), graticule_canvas::CellWidth::Narrow)],
            },
            // Bold + italic.
            Draw::Cells {
                x: 18,
                y: 0,
                style: Style::default()
                    .fg(Color::Rgb(180, 190, 254))
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                cells: vec![("Notice".into(), graticule_canvas::CellWidth::Narrow)],
            },
        ],
    };

    let mut backend = Backend::new(&mut sink)
        .with_bce(Bce::No)
        .with_profile(*profile);
    backend.begin_sync_update().unwrap();
    backend.render(&cmd).unwrap();
    backend.end_sync_update().unwrap();
    sink
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("valid utf-8 output")
}

#[test]
fn smoke_scenario_1_direct_color_truecolor() {
    let profile = TerminalProfile::direct_color();
    let output = text(render_test_frame(&profile));

    // 1. The synchronized-update envelope opens and closes.
    assert!(
        output.contains("\x1b[?2026h"),
        "must open mode 2026: {output:?}"
    );
    assert!(
        output.contains("\x1b[?2026l"),
        "must close mode 2026: {output:?}"
    );
    // 2. 24-bit color.
    assert!(output.contains("38;2;"), "fg RGB: {output:?}");
    assert!(output.contains("48;2;"), "bg RGB: {output:?}");
    // 3. Italic is emitted directly, because the profile supports it.
    assert!(
        output.contains("\x1b[3m") || output.contains(";3m") || output.contains(";3;"),
        "SGR 3 italic: {output:?}"
    );
    // 4. The box-drawing glyph survives untransliterated.
    assert!(output.contains('┃'), "UTF-8 glyph intact: {output:?}");
}

#[test]
fn smoke_scenario_2_ecma48_ansi16_linux_console() {
    let profile = TerminalProfile::for_env("linux", "", false, None, None, None);
    assert_eq!(profile.color_standard, ColorStandard::Ansi16);
    let output = text(render_test_frame(&profile));

    // No 24-bit sequences may leak onto a 16-color console.
    assert!(!output.contains("\x1b[38;2;"), "no 24-bit fg: {output:?}");
    assert!(!output.contains("\x1b[48;2;"), "no 24-bit bg: {output:?}");
    // Italic is not available, so it must arrive as underline.
    assert!(!output.contains("\x1b[3m"), "no italic SGR: {output:?}");
    assert!(
        output.contains("\x1b[4m") || output.contains(";4m"),
        "italic became underline: {output:?}"
    );
    // The linux console has no synchronized-update support.
    assert!(!output.contains("\x1b[?2026h"), "no mode 2026: {output:?}");
}

#[test]
fn smoke_scenario_3_dec_vt100_getty_serial() {
    let profile = TerminalProfile::dec_vt100_monochrome();
    let bytes = render_test_frame(&profile);
    let output = String::from_utf8_lossy(&bytes).into_owned();

    // 1. Zero color codes of any kind.
    assert!(!output.contains("\x1b[38;"), "no fg color: {output:?}");
    assert!(!output.contains("\x1b[48;"), "no bg color: {output:?}");
    assert!(!output.contains("\x1b[39m"), "no fg reset: {output:?}");
    assert!(!output.contains("\x1b[49m"), "no bg reset: {output:?}");
    // 2. No synchronized updates.
    assert!(!output.contains("\x1b[?2026h"), "no mode 2026: {output:?}");
    // 3. The highlighted background became reverse video.
    assert!(
        output.contains("\x1b[7m") || output.contains(";7m"),
        "SGR 7: {output:?}"
    );
    // 4. The Unicode border was transliterated to ASCII by the emitter.
    assert!(!output.contains('┃'), "no Unicode box glyph: {output:?}");
    assert!(output.contains('|'), "ASCII fallback: {output:?}");
    // 5. Every byte is 7-bit.
    for b in &bytes {
        assert!(b.is_ascii(), "byte {b} must be ASCII");
    }
}

#[test]
fn smoke_scenario_4_no_color_environment_standard() {
    let profile = TerminalProfile::for_env("xterm-256color", "truecolor", true, None, None, None);
    assert_eq!(profile.color_standard, ColorStandard::Monochrome);

    let driver = Driver::for_profile(&profile);
    assert_eq!(driver.color_standard(), ColorStandard::Monochrome);
    // Synchronized updates are orthogonal to colour: NO_COLOR forbids colour,
    // not mode 2026, so sync support is not asserted off here.

    // And the frame it produces obeys the standard: no color at all.
    let output = text(render_test_frame(&profile));
    assert!(
        !output.contains("\x1b[38;"),
        "NO_COLOR forbids color: {output:?}"
    );
    assert!(
        !output.contains("\x1b[48;"),
        "NO_COLOR forbids color: {output:?}"
    );
}

#[test]
fn smoke_scenario_5_constrained_80x24_getty_geometry() {
    let profile = TerminalProfile::dec_vt100_monochrome();
    let mut block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .glyph_set(&ASCII_GLYPHS);

    let mut grid = CellGrid::new(80, 24);
    let mut damage = Damage::new(24);
    let area = grid.area();
    let mut paint = Paint::new(&mut grid, &mut damage, area);
    block.render(area, &mut paint);

    // `Block` must never exceed its allocation, even when it is exact.
    assert_eq!(grid.get(0, 0).unwrap().symbol, "|");
    assert_eq!(grid.get(0, 23).unwrap().symbol, "|");
    assert_eq!(grid.get(79, 0).unwrap().symbol, "|");
    assert_eq!(grid.get(79, 23).unwrap().symbol, "|");
    assert!(!damage.is_empty());
    let _ = profile;
}

#[test]
fn smoke_scenario_6_indexed256_emission_and_quantization() {
    let profile = TerminalProfile::indexed256();
    assert!(profile.supports_color());
    assert!(profile.supports_color_themes());
    assert!(profile.supports_256_color());
    assert!(profile.is_indexed256());
    assert_eq!(profile.color_depth_bits(), 8);
    assert_eq!(profile.color_count(), 256);

    let mut driver = Driver::for_profile(&profile);
    let mut sink = Vec::new();
    driver
        .apply_style(Style::default().fg(Color::Rgb(255, 0, 0)), &mut sink)
        .unwrap();
    let output = String::from_utf8_lossy(&sink);
    assert!(
        output.contains("\x1b[38;5;"),
        "Indexed256 must emit 38;5; sequences: {output:?}"
    );
    // 24-bit codes must not appear on a 256-color profile.
    assert!(!output.contains("\x1b[38;2;"), "no truecolor: {output:?}");
}

/// Transliteration is the mechanism that makes `CharsetStandard` a capability
/// rather than a comment: an ASCII profile cannot emit UTF-8 even when the
/// drawing code hands it a Unicode glyph.
#[test]
fn smoke_scenario_7_ascii_charset_is_enforced() {
    let ascii = TerminalProfile {
        color_standard: ColorStandard::DirectColor,
        charset_standard: CharsetStandard::Ascii,
        supports_italic: true,
        supports_sync_update: false,
        supports_mouse: true,
    };
    let driver = Driver::for_profile(&ascii);
    assert_eq!(driver.transliterate("┃"), "|");
    assert_eq!(driver.transliterate("漢"), "?");

    let bytes = render_test_frame(&ascii);
    for b in &bytes {
        assert!(
            b.is_ascii(),
            "byte {b} must be 7-bit under an ASCII profile"
        );
    }
}

/// An anchored box that would be placed off-screen must be clamped into the
/// viewport, whichever way the anchor sits.
#[test]
fn smoke_scenario_8_anchored_popup_stays_on_screen() {
    let viewport = Rect::new(0, 0, 20, 10);
    for anchor_y in [0u16, 9, 50] {
        let rect = graticule_canvas::compute_anchored_rect(
            Rect::new(0, anchor_y, 4, 1),
            viewport,
            (8, 4),
            graticule_canvas::AnchorPlacement::AutoVertical,
            graticule_canvas::AnchorAlignment::Start,
            &graticule_canvas::AnchorConstraints::default(),
        );
        assert!(
            rect.right() <= viewport.right() && rect.bottom() <= viewport.bottom(),
            "anchored box {rect:?} escaped the viewport for anchor_y={anchor_y}"
        );
    }
}
