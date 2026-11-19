# ADR-0002: Incremental VT parser, Graticule-owned `Event` model, and single-reader input contract

- Status: Accepted
- Date: 2026-11-19

## Context

On Unix terminals, keyboard input, SGR mouse reports, bracketed paste, and terminal capability replies share a single byte stream (`STDIN_FILENO` or `/dev/tty`). Read-size heuristics (such as treating `read_count == buffer_size` as the sole signal that more bytes follow `0x1b`) misclassify split SGR mouse reports (`\x1b` followed by `[<35;...M` in the next `read(2)`) as a bare `Esc` press followed by printable characters. Similarly, consuming two `0x1b` bytes as a single `Esc` breaks `\x1b\x1b[A`, and failing to terminate unrecognized `ESC[?...` replies wedges input.

Additionally, running a separate terminal-capability probe reader alongside the main input loop introduces a concurrent-read race on the TTY descriptor.

## Decision

1. **Graticule owns its input `Event` contract (`graticule-tty::event`).** `Event`, `KeyEvent`, `KeyCode`, `KeyModifiers`, `KeyEventKind`, `KeyEventState`, `MouseEvent`, `MouseEventKind`, and `MouseButton` are native Graticule types with zero third-party event types in the public API.
2. **Time-based incremental `VtParser` (`graticule-tty::parser`).**
   - A lone `0x1b` waits up to `DEFAULT_ESC_TIMEOUT` (20 ms) before flushing as `KeyCode::Esc`.
   - Double `0x1b` flushes the first `Esc` immediately and retains the second `0x1b` as a fresh pending escape.
   - CSI decoding follows ECMA-48 framing (`0x20..=0x3f` parameters/intermediates, `0x40..=0x7e` final byte) so unknown sequences terminate cleanly on their final byte, CPR (`CSI row;col R`) never masquerades as `F(3)`, and malformed bytes abort without swallowing valid trailing starts.
   - Sequence (`256` bytes) and bracketed-paste (`16 MiB`) buffers are strictly bounded.
   - Supports SGR-1006 / X10 / rxvt mouse, Kitty CSI-u (preserving `SHIFT` on multi-modifier chords with shifted alternates), xterm `modifyOtherKeys`, legacy CSI/SS3/rxvt keys, `Alt+<key>`, multi-byte UTF-8 across read boundaries, and bracketed paste.
3. **Single-reader invariant on Unix.** `InputReader` runs one dedicated `libc::poll` loop over `(tty_fd, wake_pipe, sigwinch_pipe)` without setting `O_NONBLOCK` on `STDIN_FILENO` (preventing `EAGAIN` on shared-PTY `stdout`), queries `TIOCGWINSZ` directly on `SIGWINCH`, and routes Kitty capability replies (`ESC[?...u`, `ESC[?...c`) through `ParsedItem` and `ProbeHandle`.
4. **Private Windows console adapter.** `#[cfg(windows)]` adapts structured `INPUT_RECORD` console events privately into `graticule_tty::Event` so public semantics match across Linux, macOS, and Windows.

## Consequences

- `MouseMode::AllMotion` (`?1003h` + `?1006h`) is safe to enable without split-sequence leakage or phantom `Esc` events.
- `VtParser` is pure and deterministic (`feed(&[u8], Instant, &mut Vec<ParsedItem>)`), enabling exhaustive byte-split unit tests without a PTY.
- Capability probes never race with user input and resolve immediately on DA1 when Kitty keyboard enhancement is unsupported.
