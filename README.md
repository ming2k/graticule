# Graticule

A pure-native retained-mode terminal UI engine, VT wire protocol, and OS TTY host transport (`graticule-vt` + `graticule-canvas` + `graticule-tty`, exposed together through `graticule`) with **zero third-party terminal wrapper crates**.

Graticule owns a 2-D grid of cells, tracks which cells changed *at write time*, emits only the minimal combined escape codes that turn what the terminal is showing into what the application wants in a single atomic write per frame, and decodes inbound VT / Kitty / SGR mouse byte streams through a timeout-aware incremental parser.

## Architecture & Crates

```text
graticule-vt        Pure VT/ECMA-48/DEC/Kitty wire protocol (outbound CSI/SGR/OSC + inbound VtParser & Event)
    ▲               Zero OS deps · #![deny(unsafe_code)] · depends only on `bitflags`
    │
    ├───────────────────────────────┐
    │                               │
graticule-canvas                graticule-tty
Pure retained-mode canvas,      Pure-native OS TTY host transport
Flexbox layout, differ,         - Unix: termios, TIOCGWINSZ, poll(2), SIGWINCH (`libc` + `signal-hook`)
Driver, Backend, Frame,         - Windows: Win32 Console API + ReadConsoleInputW (`windows-sys`)
widgets, retained `ui`          - TerminalSession RAII guard, InputReader, ProbeHandle, panic hook
Zero OS deps
    ▲                               ▲
    └───────────────┬───────────────┘
                    │
                graticule
     Umbrella crate (`default = ["tty"]`)
```

| crate | owns |
| --- | --- |
| `graticule-vt` | `csi`, `sgr` (zero-alloc combined SGR encoder), `osc` (sanitized window title & OSC 8 hyperlinks), `mouse` (`MouseMode`), `event` (`Event`, `KeyEvent`, `MouseEvent`), and `parser` (`VtParser`). Strictly pure memory (`#![deny(unsafe_code)]`, zero OS deps). |
| `graticule-canvas` | `buffer` (`CellGrid`, `Damage`, `Style`), `layout` (`Rect`, `Flex`, `Anchor`), `render` (`diff`, `Paint`, `Driver`, `TerminalProfile`), `widgets`, `ui` (`Scene`, `UiRuntime`), and `terminal` (`Backend` with single-write frame coalescing, `Frame`, `Terminal`, `TestTerminal`). Zero OS deps. |
| `graticule-tty` | Native Unix (`termios`, `TIOCGWINSZ`, `poll(2)`, `SIGWINCH`) and Windows (Win32 Console API, `ENABLE_VIRTUAL_TERMINAL_PROCESSING`, `WaitForMultipleObjects` + `ReadConsoleInputW`) transport, `InputReader`, `ProbeHandle`, `TerminalSession`, `restore_global`, and `install_panic_hook`. |
| `graticule` | Umbrella crate re-exporting `graticule-vt` (as `vt`), `graticule-canvas` unconditionally, and `graticule-tty` under `default = ["tty"]`, plus `live_terminal` and `session_config_for_profile`. |

## The rendering model

```text
Widget::render(area, &mut Paint)   clipped writes; damage marked as you write
        │
    CellGrid + Damage              desired state, and what changed
        │
    diff(&back, &damage, &front)   pure: Draw commands, no I/O, no mutation
        │
    Backend::render(&DrawCmd)      coalesced combined-SGR VT bytes (1 write/frame)
        │
    promote(&back, &damage, &mut front)   only after the bytes were flushed
```

Four properties fall out of that shape, and each has tests:

- **A clean frame costs nothing.** With an empty `Damage` the diff returns immediately, and a frame that changes neither cells nor cursor emits zero bytes.
- **The model cannot run ahead of the terminal.** `diff` is pure; the front grid is advanced only by `promote`, only after a successful flush. A failed write forces a reset-and-repaint rather than leaving the two out of step.
- **A wholesale row shift becomes one terminal scroll.** The differ *proves* a candidate translation, so a streaming append repaints one new row instead of the viewport.
- **Wide (CJK) glyphs cannot ghost.** The trailing column belongs to the writer and carries the head's background, and continuation mismatches force a head repaint.

## Opt-in terminal session & input

`Terminal::with_size(Backend::new(...), w, h)` is strictly pure and never touches raw mode or stdin. When building an interactive TUI, opt in explicitly via `TerminalSession`, `InputReader`, and `live_terminal`:

```rust,no_run
use std::io::stdout;
use graticule::{
    Backend, InputReader, MouseMode, SessionConfig, TerminalSession,
    install_panic_hook, live_terminal,
};

fn main() -> std::io::Result<()> {
    install_panic_hook();
    let _session = TerminalSession::enter(SessionConfig {
        mouse_mode: MouseMode::Button,
        ..SessionConfig::default()
    })?;
    let (_reader, rx) = InputReader::spawn_channel()?;
    let mut terminal = live_terminal(Backend::new(stdout()));
    let _ = (rx, &mut terminal);
    Ok(())
}
```

## Capabilities are implemented, not advertised

Every field on `TerminalProfile` has an observable effect in the `Driver` built from it (`color_standard`, `charset_standard`, `supports_italic`, `supports_sync_update`). Detection is a pure function of its inputs (`TerminalProfile::for_env`) with one impure shell (`TerminalProfile::detect`). Overrides use the `GRATICULE_*` namespace (`GRATICULE_BCE`, `GRATICULE_COLOR_STANDARD`, `GRATICULE_CHARSET_STANDARD`, `GRATICULE_SYNC_UPDATE`).

## Design records

`docs/adr/` holds the architectural decision log (see [ADR-0001](docs/adr/0001-layered-architecture-canvas-and-tty.md), [ADR-0002](docs/adr/0002-incremental-vt-parser-and-input-contract.md), and [ADR-0003](docs/adr/0003-pure-native-vt-and-os-transport.md)).

## MSRV

Rust **1.88**.

## License

MIT
