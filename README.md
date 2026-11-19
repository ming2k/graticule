# Graticule

A retained-mode terminal UI engine and physical TTY host transport (`graticule-canvas` + `graticule-tty`, exposed together through `graticule`).

Graticule owns a 2-D grid of cells, tracks which cells changed *at write time*, emits only the escape codes that turn what the terminal is showing into what the application wants, and decodes inbound VT / Kitty / SGR mouse byte streams through a timeout-aware incremental parser.

## Architecture & Crates

| crate | owns |
| --- | --- |
| `graticule-canvas` | `buffer` (`CellGrid`, `Damage`, `Style`), `layout` (`Rect`, `Flex`, `Anchor`), `render` (`diff`, `Paint`, `Driver`, `TerminalProfile`), `widgets`, `ui` (`Scene`, `UiRuntime`), and `terminal` (`Backend`, `Frame`, `Terminal`, `TestTerminal`). Pure memory + output writer; `#![deny(unsafe_code)]`; zero OS session or stdin dependencies. |
| `graticule-tty` | `event` (`Event`, `KeyEvent`, `MouseEvent`), `parser` (`VtParser` with 20 ms ESC timeout disambiguation, Kitty CSI-u, SGR-1006 mouse, bracketed paste), `mouse` (`MouseMode`), `reader` (`InputReader`, `ProbeHandle`), and opt-in `session` (`TerminalSession`). |
| `graticule` | Umbrella crate re-exporting `graticule-canvas` unconditionally and `graticule-tty` under `default = ["tty"]` (`default-features = false` gives the pure canvas layer only). |

## The rendering model

```
Widget::render(area, &mut Paint)   clipped writes; damage marked as you write
        │
    CellGrid + Damage              desired state, and what changed
        │
    diff(&back, &damage, &front)   pure: Draw commands, no I/O, no mutation
        │
    Backend::render(&DrawCmd)      escape bytes
        │
    promote(&back, &damage, &mut front)   only after the bytes were flushed
```

Four properties fall out of that shape, and each has tests:

- **A clean frame costs nothing.** With an empty `Damage` the diff returns immediately, and a frame that changes neither cells nor cursor emits zero bytes.
- **The model cannot run ahead of the terminal.** `diff` is pure; the front grid is advanced only by `promote`, only after a successful flush. A failed write forces a reset-and-repaint rather than leaving the two out of step.
- **A wholesale row shift becomes one terminal scroll.** The differ *proves* a candidate translation, so a streaming append repaints one new row instead of the viewport.
- **Wide (CJK) glyphs cannot ghost.** The trailing column belongs to the writer and carries the head's background, and continuation mismatches force a head repaint.

## Opt-in terminal session & input

`Terminal::new(Backend::new(...))` still never touches raw mode or stdin. When building an interactive TUI, opt in explicitly via `TerminalSession` and `InputReader`:

```rust
use std::io::stdout;
use graticule::{Backend, InputReader, MouseMode, SessionConfig, Terminal, TerminalSession};

let _session = TerminalSession::enter(SessionConfig {
    mouse_mode: MouseMode::Button,
    ..SessionConfig::default()
})?;
let (_reader, rx) = InputReader::spawn_channel()?;
let mut terminal = Terminal::new(Backend::new(stdout()));
```

## Capabilities are implemented, not advertised

Every field on `TerminalProfile` has an observable effect in the `Driver` built from it (`color_standard`, `charset_standard`, `supports_italic`, `supports_sync_update`). Detection is a pure function of its inputs (`TerminalProfile::for_env`) with one impure shell (`TerminalProfile::detect`). Overrides use the `GRATICULE_*` namespace (`GRATICULE_BCE`, `GRATICULE_COLOR_STANDARD`, `GRATICULE_CHARSET_STANDARD`, `GRATICULE_SYNC_UPDATE`).

## Design records

`docs/adr/` holds the architectural decision log (see [ADR-0001](docs/adr/0001-layered-architecture-canvas-and-tty.md) and [ADR-0002](docs/adr/0002-incremental-vt-parser-and-input-contract.md)).

## MSRV

Rust **1.88**.

## License

MIT
