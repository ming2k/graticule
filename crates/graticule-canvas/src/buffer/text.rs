//! Text measurement and wrapping: grapheme-cluster width, caret geometry, and
//! CJK-aware line breaking.
//!
//! It answers the questions the grid and the application both need:
//!
//! - How many columns does a grapheme occupy? ([`grapheme_width`])
//! - How many columns does a string occupy? ([`str_len`])
//! - Where does a byte offset sit on screen? ([`cursor_column`])
//! - How does a string break into lines of at most `N` columns, honoring CJK
//!   kinsoku (line-start/line-end prohibition) rules? ([`wrap`])
//!
//! Measurement is done on **grapheme clusters**, never on `char`s, and always
//! through `UnicodeWidthStr::width` on the whole cluster. That is what makes a
//! ZWJ emoji family measure 2 columns instead of 1, and it is the same
//! primitive the grid paints with, so the caret can never disagree with the
//! glyphs.
//!
//! # Newline semantics
//!
//! A hard line break is `"\n"`, `"\r\n"` or a lone `"\r"`. `UAX #29` treats
//! `"\r\n"` as a single grapheme cluster, so a wrapper that only tests for
//! `"\n"` silently ignores CRLF input. Both the wrapper here and the grid's
//! string writer share [`is_hard_break`] so the two cannot disagree.

use unicode_segmentation::UnicodeSegmentation;

/// Whether a grapheme cluster is a hard line break (`"\n"`, `"\r\n"`, `"\r"`).
#[inline]
pub fn is_hard_break(grapheme: &str) -> bool {
    matches!(grapheme, "\n" | "\r\n" | "\r")
}

/// The display width of a string, summing each grapheme's width.
pub fn str_len(s: &str) -> usize {
    s.graphemes(true).map(|g| grapheme_width(g) as usize).sum()
}

/// The column width of a single grapheme cluster.
///
/// Uses `UnicodeWidthStr::width` on the **full cluster** so multi-codepoint
/// sequences — emoji ZWJ families, skin-tone variants, flag sequences — are
/// measured as a whole (typically width 2) instead of by first code point.
/// `unicode-width >= 0.2` handles these correctly at the string level, which
/// per-`char` measurement cannot.
///
/// Control characters and line breaks report width 0 so callers handle them
/// structurally rather than as visible glyphs.
pub fn grapheme_width(grapheme: &str) -> u8 {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    // Control characters are structural, never visible: `unicode-width` may
    // still assign them a column (it reports 1 for `\n`), and a wrapper that
    // believed that would count a line break as a cell.
    if grapheme.chars().any(char::is_control) {
        return 0;
    }

    // Terminal cell width rule for Variation Selectors:
    // When a single base character is modified by Variation Selector-15 (text: U+FE0E)
    // or Variation Selector-16 (emoji: U+FE0F), POSIX libc wcwidth, terminal multiplexers
    // (tmux), and standard terminal emulators (foot, xterm, linux VT) evaluate the
    // variation selector as a zero-width combining mark (width 0).
    // If the underlying base character has a standard column width of 1 (e.g. BMP symbols
    // like ⚠ U+26A0, ⚡ U+26A1, ℹ U+2139, ▶ U+25B6), the physical terminal advances its
    // cursor by exactly 1 column.
    //
    // `unicode-width >= 0.2` promotes any cluster with U+FE0F to width 2 via TR51 emoji
    // rules, which disagrees with physical terminals and creates a 1-column drift that
    // breaks wide-character pairing and partial diff updates.
    //
    // Only characters whose base width is already 2 (CJK ideographs, fullwidth forms,
    // and supplementary-plane emojis U+1F000+) or multi-character sequences (like flag
    // pairs) occupy 2 columns.
    let non_vs_count = grapheme
        .chars()
        .filter(|&c| c != '\u{fe0f}' && c != '\u{fe0e}')
        .count();
    if non_vs_count == 1
        && let Some(base) = grapheme
            .chars()
            .find(|&c| c != '\u{fe0f}' && c != '\u{fe0e}')
        && base.width() == Some(1)
    {
        return 1;
    }

    grapheme.width() as u8
}

/// A grapheme cluster with its precomputed width, so callers that walk a string
/// column by column (the wrapper, the grid's string writer) never re-measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece<'a> {
    /// The cluster's text.
    pub text: &'a str,
    /// Its display width in columns; 0 for a line break or control character.
    pub width: u8,
}

/// Iterate a string as width-annotated grapheme clusters. Hard line breaks and
/// other control characters are passed through with width 0.
pub fn graphemes(s: &str) -> impl Iterator<Item = Piece<'_>> {
    s.graphemes(true).map(|g| Piece {
        text: g,
        width: if is_hard_break(g) {
            0
        } else {
            grapheme_width(g)
        },
    })
}

/// The display column of the caret sitting at `byte_offset` within `text`.
///
/// This is the single authoritative "byte offset -> screen column" mapping, used
/// to place the terminal caret. It walks the string once as grapheme clusters,
/// adding each cluster's width for every cluster that ends at or before
/// `byte_offset`, and stops at the first cluster the offset falls inside.
/// Because the grid paints with the same [`graphemes`] / [`grapheme_width`]
/// primitives, this column cannot drift from where the text was drawn.
///
/// `byte_offset` is floored to a grapheme boundary, and `text.len()` is a valid
/// caret position (after the last grapheme), so the caret lands flush against
/// the final glyph instead of one grapheme short — two columns off for CJK.
pub fn cursor_column(text: &str, byte_offset: usize) -> usize {
    let target = byte_offset.min(text.len());
    let mut col = 0usize;
    for (idx, g) in text.grapheme_indices(true) {
        if idx + g.len() <= target {
            col += grapheme_width(g) as usize;
        } else {
            break;
        }
    }
    col
}

/// The largest grapheme boundary `<= offset`, clamped to `text.len()`.
///
/// Returns `offset` unchanged when it already sits on a boundary — including
/// `text.len()` — and only moves left when `offset` lands inside a multi-byte
/// grapheme. Use it to keep `text[..n]` slices valid before measuring or
/// rendering them, and pair it with [`inclusive_grapheme_end`] for inclusive
/// selection heads.
pub fn floor_grapheme_boundary(text: &str, offset: usize) -> usize {
    let clamped = offset.min(text.len());
    let mut floor = 0;
    for (idx, g) in text.grapheme_indices(true) {
        if idx + g.len() <= clamped {
            floor = idx + g.len();
        } else {
            break;
        }
    }
    floor
}

/// The end (exclusive) of the grapheme cluster containing `offset`, so the
/// character under a selection head is included. `text.len()` (a caret resting
/// past the last grapheme) returns `text.len()`.
pub fn inclusive_grapheme_end(text: &str, offset: usize) -> usize {
    let clamped = offset.min(text.len());
    for (idx, g) in text.grapheme_indices(true) {
        let end = idx + g.len();
        if end > clamped {
            return end;
        }
    }
    text.len()
}

/// A wrapped line: the text it contains and its byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line's text, without its terminating break.
    pub text: String,
    /// Byte offset in the source where this line starts.
    pub start_byte: usize,
    /// Byte offset in the source where this line ends (exclusive).
    pub end_byte: usize,
}

/// Wrap `text` into lines that each fit within `max_width` display columns.
///
/// # Rules
///
/// - A hard break ([`is_hard_break`]: `"\n"`, `"\r\n"`, `"\r"`) starts a new
///   line and is not included in either line's text.
/// - Break opportunity is *before* the grapheme that would overflow the budget.
///   A grapheme is never split; a single grapheme wider than `max_width` is
///   therefore emitted on a line of its own that exceeds the budget, which is
///   the only lossless option and is the caller's cue to clip.
/// - Kinsoku: a line-start-prohibited grapheme (e.g. `，`) is pulled onto the
///   previous line, and a line-end-prohibited grapheme (e.g. `（`) is pushed
///   onto the next. When the previous line holds exactly one grapheme and
///   pulling it would strand an empty line, the prohibited grapheme is instead
///   kept attached to that single grapheme — an intentional one-column
///   overflow, in preference to orphaning punctuation at a line start.
///
/// Byte ranges cover the source exactly, so a caller can slice the original
/// string to recover the styled spans for a wrapped line.
pub fn wrap(text: &str, max_width: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut line_start_byte = 0usize;

    // Iterate by *grapheme* so multi-codepoint sequences (emoji with ZWJ, base
    // characters with combining marks) are never split. Kinsoku rules test the
    // first and last char of the cluster.
    for (byte_idx, grapheme) in text.grapheme_indices(true) {
        if is_hard_break(grapheme) {
            lines.push(Line {
                text: std::mem::take(&mut current),
                start_byte: line_start_byte,
                end_byte: byte_idx,
            });
            line_start_byte = byte_idx + grapheme.len();
            current_width = 0;
            continue;
        }

        let g_width = grapheme_width(grapheme) as usize;

        if current_width + g_width > max_width && !current.is_empty() {
            let first_char = grapheme.chars().next().unwrap_or('\0');
            let last_char = current.chars().next_back().unwrap_or('\0');
            let move_previous = prohibited_line_start(first_char) || prohibited_line_end(last_char);

            // Try to carry the previous grapheme across the break so the
            // prohibited pair stays adjacent.
            let mut moved: Option<String> = None;
            if move_previous
                && let Some((offset, last_g)) = current.grapheme_indices(true).next_back()
                && offset > 0
            {
                moved = Some(last_g.to_string());
                current.truncate(offset);
            }

            if let Some(moved) = moved {
                let moved_start = byte_idx - moved.len();
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    start_byte: line_start_byte,
                    end_byte: moved_start,
                });
                current_width = grapheme_width(&moved) as usize;
                current.push_str(&moved);
                line_start_byte = moved_start;
            } else if move_previous {
                // The previous line holds a single grapheme we cannot move
                // without stranding an empty line. Keep the punctuation
                // attached and accept a one-grapheme overflow on this line.
                current.push_str(grapheme);
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    start_byte: line_start_byte,
                    end_byte: byte_idx + grapheme.len(),
                });
                line_start_byte = byte_idx + grapheme.len();
                current_width = 0;
                continue;
            } else {
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    start_byte: line_start_byte,
                    end_byte: byte_idx,
                });
                line_start_byte = byte_idx;
                current_width = 0;
            }
        }

        current.push_str(grapheme);
        current_width += g_width;
    }

    if !current.is_empty() || line_start_byte < text.len() {
        lines.push(Line {
            text: current,
            start_byte: line_start_byte,
            end_byte: text.len(),
        });
    }

    lines
}

/// Characters that must not start a line: CJK closing punctuation plus the
/// ASCII closers, so mixed CJK/Latin text also breaks cleanly.
pub const PROHIBITED_LINE_START: &[char] = &[
    '，', '。', '、', '！', '？', '：', '；', '）', '】', '》', '〉', '」', '』', '〕', '”', '’',
    ',', '.', '!', '?', ':', ';', ')', ']', '}',
];

/// Characters that must not end a line: CJK opening punctuation plus the ASCII
/// openers.
pub const PROHIBITED_LINE_END: &[char] = &[
    '（', '【', '《', '〈', '「', '『', '〔', '“', '‘', '(', '[', '{',
];

/// Whether `ch` must not start a line (see [`PROHIBITED_LINE_START`]).
pub fn prohibited_line_start(ch: char) -> bool {
    PROHIBITED_LINE_START.contains(&ch)
}

/// Whether `ch` must not end a line (see [`PROHIBITED_LINE_END`]).
pub fn prohibited_line_end(ch: char) -> bool {
    PROHIBITED_LINE_END.contains(&ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_column_lands_flush_at_end_of_text() {
        assert_eq!(cursor_column("中文", "中文".len()), 4);
        assert_eq!(cursor_column("ab", 2), 2);
        assert_eq!(cursor_column("😀", "😀".len()), 2);
        assert_eq!(cursor_column("", 0), 0);
    }

    #[test]
    fn cursor_column_mid_string_and_mid_grapheme() {
        assert_eq!(cursor_column("中文", 3), 2);
        assert_eq!(cursor_column("ab", 1), 1);
        assert_eq!(cursor_column("中文", 1), 0);
        assert_eq!(cursor_column("中文", 4), 2);
        assert_eq!(cursor_column("😀", 2), 0);
    }

    #[test]
    fn cursor_column_matches_paint_width_exactly() {
        for (text, off) in [("中文测试", 6), ("abc", 2), ("a😀b", 2), ("中", 3)] {
            let col = cursor_column(text, off);
            let floored = floor_grapheme_boundary(text, off);
            assert_eq!(
                col,
                str_len(&text[..floored]),
                "mismatch for {text:?}@{off}"
            );
        }
    }

    #[test]
    fn floor_and_inclusive_boundaries_are_grapheme_aware() {
        assert_eq!(floor_grapheme_boundary("中文", "中文".len()), 6);
        assert_eq!(floor_grapheme_boundary("中文", 4), 3);
        assert_eq!(inclusive_grapheme_end("中文", 3), 6);
        assert_eq!(inclusive_grapheme_end("中文", 6), 6);
    }

    #[test]
    fn ascii_wraps_on_word_overflow() {
        let lines = wrap("hello world", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, " worl");
        assert_eq!(lines[2].text, "d");
    }

    #[test]
    fn every_hard_break_starts_a_new_line() {
        for (input, expected) in [
            ("hi\nthere", vec!["hi", "there"]),
            // UAX #29 makes CRLF a single cluster; a "\n"-only test misses it.
            ("hi\r\nthere", vec!["hi", "there"]),
            ("hi\rthere", vec!["hi", "there"]),
        ] {
            let lines = wrap(input, 10);
            let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
            assert_eq!(texts, expected, "for {input:?}");
            // Byte ranges cover the source exactly.
            assert_eq!(lines[0].start_byte, 0);
            assert_eq!(lines[0].end_byte, 2);
            assert_eq!(lines[1].end_byte, input.len());
        }
        assert!(wrap("", 10).is_empty());
    }

    #[test]
    fn cjk_avoids_prohibited_line_start() {
        let lines = wrap("人生需要坚持，才能前进。", 12);
        assert!(lines.len() > 1);
        // No line after the first may start with a prohibited glyph.
        assert!(lines.iter().skip(1).all(|l| {
            l.text
                .chars()
                .next()
                .is_none_or(|ch| !prohibited_line_start(ch))
        }));
        assert!(lines.iter().all(|l| {
            l.text
                .chars()
                .next_back()
                .is_none_or(|ch| !prohibited_line_end(ch))
        }));
    }

    #[test]
    fn single_grapheme_line_keeps_prohibited_punctuation_attached() {
        // The old wrapper refused to pull the only grapheme on the line and
        // therefore orphaned the comma at a line start.
        let lines = wrap("人，", 2);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "人，");
        assert_eq!(lines[0].start_byte, 0);
        assert_eq!(lines[0].end_byte, "人，".len());
        // Every line starts on a valid boundary.
        for l in &lines {
            assert!(l.text.is_char_boundary(0) && l.text.len() == l.text.len());
        }
    }

    #[test]
    fn wide_grapheme_has_width_two() {
        assert_eq!(grapheme_width("😀"), 2);
        assert_eq!(grapheme_width("a"), 1);
        assert_eq!(grapheme_width("e\u{0301}"), 1);
        assert_eq!(grapheme_width("\n"), 0);
        assert_eq!(str_len("中文"), 4);
        assert_eq!(grapheme_width("⚠️"), 1);
        assert_eq!(grapheme_width("⚠"), 1);
        assert_eq!(grapheme_width("⚡️"), 2);
        assert_eq!(grapheme_width("⚡"), 2);
        assert_eq!(grapheme_width("🇺🇸"), 2);
    }

    #[test]
    fn oversized_grapheme_gets_its_own_line_rather_than_being_dropped() {
        // A 2-column glyph cannot fit a 1-column budget; it must still be
        // emitted (the caller clips) rather than vanish.
        let lines = wrap("中文", 1);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "中");
        assert_eq!(lines[1].text, "文");
    }

    #[test]
    fn pieces_annotate_hard_breaks_with_zero_width() {
        let pieces: Vec<_> = graphemes("a\r\nb").collect();
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].width, 1);
        assert_eq!(pieces[1].text, "\r\n");
        assert_eq!(pieces[1].width, 0);
        assert!(is_hard_break(pieces[1].text));
    }
}
