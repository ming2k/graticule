# Changelog

## 0.0.1

Initial release of **Graticule**, split into three crates under one workspace (ADR-0001, ADR-0002):

### Added

- **`graticule-canvas`**: pure-memory retained `CellGrid`, write-time `Damage` tracking, `Flex` layout solver, anchored popups, run-length differential renderer (`diff`, `Driver`, `TerminalProfile`), widgets, retained `ui` scene graph (`Scene`, `UiRuntime`), and output `Backend`/`Frame`/`Terminal`/`TestTerminal` (`#![deny(unsafe_code)]`, zero OS session/stdin dependencies).
- **`graticule-tty`**: physical TTY host transport — Graticule-owned `Event` types (zero `crossterm` exposure in public API), incremental timeout-aware `VtParser` (20 ms `ESC` deadline, double-`Esc` preservation, ECMA-48 CSI framing, Kitty CSI-u, xterm `modifyOtherKeys`, SGR-1006/X10/rxvt mouse, bracketed paste, UTF-8), `MouseMode`, single-reader `InputReader` (`ProbeHandle` + direct `TIOCGWINSZ`), private Windows adapter, and opt-in `TerminalSession` RAII guard.
- **`graticule`**: umbrella crate re-exporting `graticule-canvas` unconditionally and `graticule-tty` under `default = ["tty"]`.
- **Environment override namespace**: `GRATICULE_BCE`, `GRATICULE_COLOR_STANDARD`, `GRATICULE_CHARSET_STANDARD`, `GRATICULE_SYNC_UPDATE`.
- **Wide-glyph & variation-selector fidelity**: continuation-mismatch head repainting in `diff_row`, and POSIX/tmux/foot-aligned `grapheme_width` handling for `U+FE0E`/`U+FE0F` on width-1 base glyphs.
