# ADR-0001: Split Graticule into `graticule-canvas`, `graticule-tty`, and the `graticule` umbrella

- Status: Accepted
- Date: 2026-11-19

## Context

Terminal UI applications have two distinct layers with different dependency and safety requirements:

1. **Pure-memory rendering and layout**: cell grids, damage tracking, Flexbox layout, differential escape emission, widgets, and retained scene graphs must run headlessly in tests, avoid OS terminal state mutations, and remain strictly free of raw FFI (`#![deny(unsafe_code)]`).
2. **Physical TTY host transport**: interactive applications need raw mode, alternate screen, SGR mouse tracking (`?1002h` / `?1003h` + `?1006h`), bracketed paste (`?2004h`), off-frame Kitty keyboard enhancement negotiation, signal/drop restoration, and reliable inbound byte-stream decoding.

Coupling both layers into one indivisible crate forces pure presentation/component libraries to pull in OS polling and raw-mode drivers, while excluding the TTY layer forces every application to re-implement terminal setup and input decoding.

## Decision

1. **Organize Graticule as a three-crate workspace with role-specific names:**
   - **`graticule-canvas`**: pure-memory `CellGrid`, `Damage`, `Flex` layout solver, `diff`, `Driver`, `TerminalProfile`, `widgets`, `ui` (`Scene`/`UiRuntime`), and output `Backend`/`Frame`/`Terminal`/`TestTerminal`. Zero OS session or stdin dependencies.
   - **`graticule-tty`**: physical TTY host transport — Graticule-owned `Event` types, `VtParser` incremental state machine, `MouseMode`, single-reader `InputReader` (`ProbeHandle`), and opt-in `TerminalSession` RAII guard. Independent of `graticule-canvas`.
   - **`graticule`**: umbrella crate re-exporting `graticule-canvas` unconditionally and `graticule-tty` behind `default = ["tty"]`. Callers needing only the pure canvas opt out with `default-features = false` or depend directly on `graticule-canvas`.
2. **Terminal session ownership is explicitly opt-in.** `Terminal::new(Backend::new(...))` never touches raw mode or stdin; applications opt in explicitly via `TerminalSession::enter(SessionConfig { ... })`.
3. **Single environment override namespace:** `GRATICULE_BCE`, `GRATICULE_COLOR_STANDARD`, `GRATICULE_CHARSET_STANDARD`, `GRATICULE_SYNC_UPDATE`.
4. **Workspace lint baseline:** `unsafe_code = "deny"` at workspace level (localized `#[allow(unsafe_code)]` with safety arguments only in `graticule-tty::sys::unix` syscall wrappers and `graticule-canvas`'s `CompactSymbol`), plus `unwrap_used`/`expect_used`/`panic`/`todo`/`dbg_macro` warnings in library code and `-D warnings` in CI and `rustdoc`.

## Consequences

- Pure presentation/view crates depend only on `graticule-canvas` with zero OS session or stdin coupling.
- Interactive TUI applications depend on one crate (`graticule`) for both differential rendering and reliable TTY input/session control.
- `graticule-tty` can also be consumed standalone by applications that bring their own renderer.
