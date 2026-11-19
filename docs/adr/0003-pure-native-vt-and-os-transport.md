# ADR-0003: Pure-native VT wire protocol (`graticule-vt`) and zero-`crossterm` OS transport

- Status: Accepted
- Date: 2026-11-19

## Context

In `v0.0.1`, Graticule removed `crossterm` from its public API and implemented a native Unix `VtParser` + `poll(2)` loop, but three internal `crossterm` couplings remained:

1. `graticule-canvas` depended on `crossterm` to format outbound ANSI escape strings (`Driver`, `Backend`) and to query live terminal dimensions (`Terminal::new` / `render_frame`), silently pulling OS crates (`libc`, `windows-sys`) into the supposedly pure-memory canvas crate and leaking `to_ct_color` / `iter_attrs`.
2. `graticule-tty::session` used `crossterm` for raw mode and alternate-screen transitions, and `graticule-tty::sys::unix` kept a `crossterm::terminal::size()` fallback.
3. `graticule-tty::sys::windows` wrapped `crossterm::event` with a 200 ms polling loop.

For Graticule to be a self-contained, production-grade terminal UI substrate with minimal compile times, deterministic rendering, and full ownership of every byte on the wire, `crossterm` must be eliminated from the entire workspace.

## Decision

1. **Extract a fourth foundational crate: `graticule-vt` (`#![deny(unsafe_code)]`, zero OS deps, depends only on `bitflags`).**
   - Owns outbound VT / ECMA-48 / DEC / xterm / Kitty encoding:
     - `csi`: stack-formatted, zero-allocation `CUP`, `DECSTBM`, `SU`/`SD`, `DECTCEM`, `DECAWM`, `DECSCUSR` (`CursorStyle`), `ED`/`EL`, Mode `2026`, Mode `1049`, Mode `2004`, Mode `1004`, Kitty keyboard probes.
     - `sgr`: zero-allocation stack-buffered combined `SgrEncoder` (`\x1b[0;38;2;...;1;3m`) flushing each style delta in one `write_all` call.
     - `osc`: control-byte-sanitized OSC 2 window titles and OSC 8 hyperlinks.
     - `mouse`: `MouseMode::{None, Button, AllMotion}` (`?1002`/`?1003` + `?1006`).
   - Owns inbound decoding: `event` (`Event`, `KeyEvent`, `MouseEvent`, ...) and incremental `VtParser`.
2. **Make `graticule-canvas` strictly pure memory with zero OS dependencies.**
   - Depends only on `graticule-vt`, `bitflags`, `unicode-segmentation`, and `unicode-width`.
   - `Driver` emits combined SGR directly via `graticule_vt::sgr::SgrEncoder` (fixing the non-italic attribute bug where non-italic styles could inherit underline) and drops `to_ct_color`/`iter_attrs`.
   - `Backend` coalesces synchronized frames (and standalone `render` calls) into a reusable internal scratch buffer so an entire frame lands in a single `write_all` syscall, and paints non-BCE blank tails using a static space buffer with zero heap allocation.
   - `Terminal` replaces implicit OS size queries with explicit `Terminal::with_size(backend, w, h)` and `Terminal::with_size_query(backend, query)`.
3. **Implement pure-native OS TTY transport in `graticule-tty`.**
   - **Unix (`libc` + `signal-hook`)**: direct `tcgetattr`/`cfmakeraw`/`tcsetattr(TCSANOW)` raw mode with `EINTR` retry, `/dev/tty` fallback, and bit-for-bit original `termios` preservation (verified against a real kernel PTY in unit tests); direct `TIOCGWINSZ` sizing across `STDOUT`/`STDIN`/`STDERR`/`/dev/tty`; `poll(2)` + `SIGWINCH` self-pipe + wake-pipe input loop.
   - **Windows (`windows-sys`)**: direct `GetConsoleMode`/`SetConsoleMode` (enabling `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on output and raw + mouse + window input modes with exact original mode restoration), `GetConsoleScreenBufferInfo` visible-viewport sizing, and `WaitForMultipleObjects([conin, wake_event])` + `ReadConsoleInputW` feeding a cross-platform unit-tested `WinInputDecoder` (UTF-16 surrogate pairs, AltGr, Ctrl chords, mouse press/release/drag/wheel transitions).
   - **Session hardening**: single-write coalesced enter and `restore_global()` teardown (including SGR reset, cursor style reset, focus events, bracketed paste, mouse, Kitty pop, alt screen, show cursor, raw-mode restore) plus idempotent `install_panic_hook()`.
4. **Umbrella `graticule` connects the layers cleanly.**
   - Re-exports `graticule_vt as vt`, `graticule_canvas::*`, and (under default feature `tty`) `graticule_tty::*`, plus `live_terminal(backend)` (`Terminal::with_size_query(backend, terminal_size)`) and `session_config_for_profile`.

## Consequences

- `crossterm` is completely removed from the workspace dependency graph.
- `graticule-vt` and `graticule-canvas` have zero OS/FFI dependencies and compile cleanly for headless, embedded, SSH/PTY, and WebAssembly hosts.
- Every frame renders in a single `write_all` call with combined SGR sequences and zero per-style heap allocations.
- Windows and Unix both shut down `InputReader` with 0 ms wakeup latency and test their core decoders in CI.
