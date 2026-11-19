//! Outbound OSC (Operating System Command) sequences with control-byte
//! sanitization so untrusted strings cannot break out of an OSC payload.

use std::io::{self, Write};

/// Close an active OSC 8 hyperlink (`ESC ] 8 ; ; ST`).
pub const HYPERLINK_END: &[u8] = b"\x1b]8;;\x1b\\";

/// Write an OSC 2 terminal window title (`\x1b]2;{title}\x07`), stripping C0
/// control bytes and DEL so untrusted titles cannot inject escape sequences.
pub fn write_window_title<W: Write>(out: &mut W, title: &str) -> io::Result<()> {
    out.write_all(b"\x1b]2;")?;
    write_sanitized_osc_payload(out, title)?;
    out.write_all(b"\x07")
}

/// Open an OSC 8 hyperlink (`\x1b]8;id={id};{uri}\x1b\\`), sanitizing C0/DEL
/// bytes from both `id` and `uri`.
pub fn write_hyperlink_start<W: Write>(out: &mut W, uri: &str, id: Option<&str>) -> io::Result<()> {
    out.write_all(b"\x1b]8;")?;
    if let Some(id) = id {
        out.write_all(b"id=")?;
        for byte in id.bytes() {
            // Parameter delimiters (`;`, `:`) and controls are stripped from `id`.
            if byte >= 0x20 && byte != 0x7f && byte != b';' && byte != b':' {
                out.write_all(&[byte])?;
            }
        }
    }
    out.write_all(b";")?;
    write_sanitized_osc_payload(out, uri)?;
    out.write_all(b"\x1b\\")
}

fn write_sanitized_osc_payload<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    // Fast path: write contiguous clean slices without per-byte writes.
    let bytes = text.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b < 0x20 || b == 0x7f {
            if start < i {
                out.write_all(&bytes[start..i])?;
            }
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.write_all(&bytes[start..])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_title_strips_escape_injection() {
        let mut out = Vec::new();
        write_window_title(&mut out, "Graticule\x1b]52;c;evil\x07 Demo").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]2;Graticule]52;c;evil Demo\x07"
        );
    }

    #[test]
    fn hyperlink_start_and_end_format_cleanly() {
        let mut out = Vec::new();
        write_hyperlink_start(&mut out, "https://example.com/a\x1b[2J", Some("link;1")).unwrap();
        out.write_all(b"click").unwrap();
        out.write_all(HYPERLINK_END).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]8;id=link1;https://example.com/a[2J\x1b\\click\x1b]8;;\x1b\\"
        );
    }
}
