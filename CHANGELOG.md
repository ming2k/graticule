# Changelog

## 0.0.2

Pure-native architecture with zero third-party terminal wrapper crates (ADR-0003):

### Added

- **`graticule-vt`**: new foundational pure-Rust VT/ECMA-48/DEC/Kitty wire-protocol crate (`#![deny(unsafe_code)]`, depends only on `bitflags`) owning zero-allocation outbound `csi`, `sgr` (`SgrEncoder`), `osc` (sanitized window titles & OSC 8 hyperlinks), `mouse` (`MouseMode`), and inbound `event` + incremental `VtParser`.
- **Native Unix `termios` raw mode** in `graticule-tty`: direct `tcgetattr`/`cfmakeraw`/`tcsetattr` with `EINTR` retry, `/dev/tty` fallback, bit-for-bit cooked `termios` restoration (tested against a real kernel PTY), `is_tty()`, and `is_raw_mode_enabled()`.
- **Native Windows Console API driver** in `graticule-tty`: direct `windows-sys` `GetConsoleMode`/`SetConsoleMode` (`ENABLE_VIRTUAL_TERMINAL_PROCESSING` + raw input), `GetConsoleScreenBufferInfo` viewport sizing, and `WaitForMultipleObjects` + `ReadConsoleInputW` with 0 ms wakeup and cross-platform unit-tested `WinInputDecoder` (UTF-16 surrogate pairs, AltGr, Ctrl chords, mouse press/release/drag/wheel transitions).
- **Atomic single-write frame coalescing** in `graticule-canvas::Backend`, plus `Backend::clear_screen`, `Backend::set_cursor_style`, `Terminal::with_size`, `Terminal::with_size_query`, `graticule::live_terminal`, `SessionConfig::{focus_events, cursor_style}`, `TerminalSession::{set_cursor_style, set_window_title}`, and idempotent `install_panic_hook()`.

### Changed

- Removed `crossterm` completely from every crate in the workspace.
- `graticule-canvas` now has zero OS dependencies (`graticule-vt`, `bitflags`, `unicode-segmentation`, `unicode-width` only).
- `Driver` emits combined SGR sequences (`\x1b[0;38;2;...;1;3m`) with zero heap allocation; removed `to_ct_color` and `iter_attrs` in favor of `to_sgr_color`.
- Replaced implicit OS size lookup in `Terminal::new` with explicit `Terminal::with_size` / `Terminal::with_size_query` in `graticule-canvas` and `graticule::live_terminal` in the umbrella crate.

### Fixed

- Fixed `Driver::prepare` underlining non-italic styles when `supports_italic == false`.

## 0.0.1

Initial release of **Graticule**, split into three crates under one workspace (ADR-0001, ADR-0002):

### Added

- **`graticule-canvas`**: pure-memory retained `CellGrid`, write-time `Damage` tracking, `Flex` layout solver, anchored popups, run-length differential renderer (`diff`, `Driver`, `TerminalProfile`), widgets, retained `ui` scene graph (`Scene`, `UiRuntime`), and output `Backend`/`Frame`/`Terminal`/`TestTerminal` (`#![deny(unsafe_code)]`, zero OS session/stdin dependencies).
- **`graticule-tty`**: physical TTY host transport — Graticule-owned `Event` types, incremental timeout-aware `VtParser` (20 ms `ESC` deadline, double-`Esc` preservation, ECMA-48 CSI framing, Kitty CSI-u, xterm `modifyOtherKeys`, SGR-1006/X10/rxvt mouse, bracketed paste, UTF-8), `MouseMode`, single-reader `InputReader` (`ProbeHandle` + direct `TIOCGWINSZ`), and opt-in `TerminalSession` RAII guard.
- **`graticule`**: umbrella crate re-exporting `graticule-canvas` unconditionally and `graticule-tty` under `default = ["tty"]`.
- **Environment override namespace**: `GRATICULE_BCE`, `GRATICULE_COLOR_STANDARD`, `GRATICULE_CHARSET_STANDARD`, `GRATICULE_SYNC_UPDATE`.
- **Wide-glyph & variation-selector fidelity**: continuation-mismatch head repainting in `diff_row`, and POSIX/tmux/foot-aligned `grapheme_width` handling for `U+FE0E`/`U+FE0F` on width-1 base glyphs.
