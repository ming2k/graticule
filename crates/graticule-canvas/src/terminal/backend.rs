//! Pure-VT output backend: turns a [`DrawCmd`] into the minimal escape-code
//! delta on the wire, with BCE (back-color-erase) awareness and atomic
//! single-write frame coalescing.
//!
//! # Responsibilities
//!
//! - Track the **current** cursor position, cursor visibility, and emitter
//!   style across draws, so consecutive runs that share a style emit no SGR
//!   and a run already at the right position emits no cursor move.
//! - Detect `bce` from `TERM` and, when available, clear a blank tail with
//!   `clr_eol` (`\x1b[K`) instead of writing per-cell spaces. Without BCE, blank
//!   tails are painted as styled space cells — the only correct fallback.
//! - Coalesce an entire synchronized-update frame (or standalone `render` call)
//!   into a reusable internal buffer and emit it in a single [`Write::write_all`]
//!   call, avoiding per-cell syscalls and torn PTY writes.
//! - Manage raw mode and the alternate screen? **No.** This crate deliberately
//!   carries zero OS/TTY dependencies; physical session lifecycle lives in
//!   `graticule-tty`.
//!
//! # Emitter selection
//!
//! `Backend<W, E: EscapeEmitter = Driver>` defaults to the standard
//! profile-driven [`Driver`], so `Backend::new(sink)` needs no type annotation,
//! while an unusual protocol can supply its own emitter through
//! [`Backend::with_emitter`] without a box or a dyn call.

use std::io::{self, Write};

use graticule_vt::csi::{
    self, CLEAR_ALL, CLEAR_UNTIL_NEWLINE, CURSOR_HIDE, CURSOR_SHOW, CursorStyle, DISABLE_LINE_WRAP,
    ENABLE_LINE_WRAP, RESET_SCROLL_REGION,
};

use crate::buffer::{Color, Style};
use crate::render::diff::{Draw, DrawCmd};
use crate::render::driver::{Driver, EscapeEmitter};
use crate::render::profile::TerminalProfile;

/// Whether the terminal advertises back-color-erase (`bce`).
///
/// When BCE is available, clearing a line tail to the current background is a
/// single `\x1b[K` that inherits the active background. Without it, the backend
/// must write explicit space cells styled with the target background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bce {
    /// The terminal honours `clr_eol` with the current background.
    Yes,
    /// It does not; clears must be painted cell by cell.
    No,
}

impl Bce {
    /// Detect BCE from the environment: `TERM` against the set of terminals
    /// known to set the capability, plus the `GRATICULE_BCE` override (`1` forces on,
    /// `0` forces off).
    ///
    /// We do not shell out to `tput`/`infocmp` (slow, not always present). The
    /// known list names the `TERM` strings whose terminfo entry carries `bce`,
    /// so it includes the 256-color multiplexer variants rather than only the
    /// bare `tmux` name.
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let override_str = std::env::var("GRATICULE_BCE").ok();
        Self::for_term(&term, override_str.as_deref())
    }

    /// Pure detection logic, separable from the environment for tests.
    ///
    /// `override_str` is the optional `GRATICULE_BCE` value. An unrecognised `TERM`
    /// defaults to [`Bce::No`], because emitting `clr_eol` to a terminal that
    /// does not honour the current background would paint the wrong colour.
    pub fn for_term(term: &str, override_str: Option<&str>) -> Self {
        if let Some(v) = override_str {
            match v {
                "1" | "true" | "yes" | "on" => return Bce::Yes,
                "0" | "false" | "no" | "off" => return Bce::No,
                _ => {}
            }
        }
        Self::from_known(term)
    }

    /// Whether the terminfo entry for `TERM` carries `bce`.
    fn from_known(term: &str) -> Self {
        const BCE_TERMS: &[&str] = &[
            "xterm",
            "xterm-256color",
            "xterm-direct",
            "xterm-kitty",
            "foot",
            "foot-extra",
            "kitty",
            "kitty-direct",
            "wezterm",
            "alacritty",
            "tmux",
            "tmux-256color",
            "screen",
            "screen-256color",
            "screen.xterm-256color",
            "st",
            "st-256color",
        ];
        // A terminfo name may carry a `+`-suffixed feature list.
        let base = term.split('+').next().unwrap_or(term);
        if BCE_TERMS.contains(&base) || BCE_TERMS.contains(&term) {
            Bce::Yes
        } else {
            Bce::No
        }
    }
}

impl Default for Bce {
    fn default() -> Self {
        Self::detect()
    }
}

/// Pure-VT escape renderer. Owns the output writer, the escape emitter, a
/// reusable frame-coalescing buffer, and the "what's currently on screen"
/// tracking (cursor position and visibility), so each draw emits only the
/// minimal delta.
pub struct Backend<W: Write, E: EscapeEmitter = Driver> {
    out: W,
    bce: Bce,
    emitter: E,
    /// Reusable frame buffer so an entire synchronized frame (or `render` call)
    /// lands in one [`Write::write_all`] call without reallocating after warmup.
    scratch: Vec<u8>,
    /// True between [`Self::begin_sync_update`] and [`Self::end_sync_update`].
    in_sync: bool,
    /// Last cursor position we moved to. `None` until the first move, which means
    /// the next draw must always reposition.
    cur: Option<(u16, u16)>,
    /// Last cursor **visibility** we set (`?25h`/`?25l`). `None` until the first
    /// hide/show. Tracking it lets us skip re-emitting the same sequence every
    /// frame, which on light, frequent incremental redraws shows up as caret
    /// flicker.
    cursor_visible: Option<bool>,
}

impl<W: Write> Backend<W, Driver> {
    /// Wrap an output writer (typically `io::stdout()`), detecting the BCE
    /// capability and terminal profile from the environment.
    pub fn new(out: W) -> Self {
        let profile = TerminalProfile::detect();
        Self::with_emitter(out, Bce::detect(), Driver::for_profile(&profile))
    }

    /// Builder: override the BCE capability.
    pub fn with_bce(mut self, bce: Bce) -> Self {
        self.bce = bce;
        self
    }

    /// Builder: override the capability profile (replaces the driver).
    pub fn with_profile(mut self, profile: TerminalProfile) -> Self {
        self.emitter = Driver::for_profile(&profile);
        self
    }

    /// Builder: override the driver directly.
    pub fn with_driver(mut self, driver: Driver) -> Self {
        self.emitter = driver;
        self
    }

    /// Replace the emitter, resetting tracked state. Returns the previous one.
    pub fn set_profile(&mut self, profile: TerminalProfile) -> Driver {
        let _ = self.invalidate();
        std::mem::replace(&mut self.emitter, Driver::for_profile(&profile))
    }
}

impl<W: Write, E: EscapeEmitter> Backend<W, E> {
    /// Construct with an explicit emitter. This is the extension point for a
    /// custom protocol.
    pub fn with_emitter(out: W, bce: Bce, emitter: E) -> Self {
        Self {
            out,
            bce,
            emitter,
            scratch: Vec::with_capacity(8 * 1024),
            in_sync: false,
            cur: None,
            cursor_visible: None,
        }
    }

    /// The BCE setting in use.
    pub fn bce(&self) -> Bce {
        self.bce
    }

    /// Borrow the emitter.
    pub fn emitter(&self) -> &E {
        &self.emitter
    }

    /// Mutably borrow the emitter.
    pub fn emitter_mut(&mut self) -> &mut E {
        &mut self.emitter
    }

    /// Borrow the underlying writer. Any pending buffered bytes are flushed to
    /// the writer first so caller writes preserve byte ordering.
    pub fn writer(&mut self) -> &mut W {
        if !self.scratch.is_empty() {
            let _ = self.flush_scratch();
        }
        &mut self.out
    }

    /// Open a DEC synchronized-update (mode 2026) envelope and begin coalescing
    /// all writes until the matching [`Self::end_sync_update`].
    pub fn begin_sync_update(&mut self) -> io::Result<()> {
        if !self.in_sync {
            self.in_sync = true;
            self.emitter.begin_sync(&mut self.scratch)?;
        }
        Ok(())
    }

    /// Close a synchronized-update envelope opened by
    /// [`Self::begin_sync_update`], emitting the coalesced frame in a single
    /// [`Write::write_all`] call and flushing the underlying writer.
    pub fn end_sync_update(&mut self) -> io::Result<()> {
        self.in_sync = false;
        let _ = self.emitter.end_sync(&mut self.scratch);
        let write_res = self.flush_scratch();
        let flush_res = self.out.flush();
        write_res.and(flush_res)
    }

    /// Clear the entire visible screen (`CUP 1;1` + `ED 2`), keeping cursor
    /// tracking in sync.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        csi::write_move_to(&mut self.scratch, 0, 0)?;
        self.scratch.write_all(CLEAR_ALL)?;
        self.cur = Some((0, 0));
        self.commit_if_unbatched()
    }

    /// Set the hardware cursor shape (`DECSCUSR`).
    pub fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()> {
        self.scratch.write_all(style.as_bytes())?;
        self.commit_if_unbatched()
    }

    /// Apply a diff's draw commands: move the cursor into place, set the style
    /// delta, and write each run's symbols. Returns the number of draw commands
    /// processed.
    pub fn render(&mut self, cmd: &DrawCmd) -> io::Result<usize> {
        let terminal_w = cmd.w;
        let terminal_h = cmd.h;
        let mut processed = 0usize;

        for draw in &cmd.draws {
            processed += 1;
            match draw {
                Draw::ScrollDown { y, height, amount } => {
                    self.scroll_region(*y, *height, *amount, terminal_h, false)?;
                }
                Draw::ScrollUp { y, height, amount } => {
                    self.scroll_region(*y, *height, *amount, terminal_h, true)?;
                }
                Draw::Cells { x, y, style, cells } => {
                    self.move_to_buf(*x, *y)?;
                    self.emitter.apply_style(*style, &mut self.scratch)?;
                    let mut current_x = *x;
                    for (sym, width) in cells {
                        // A symbol may need transliteration for a 7-bit terminal;
                        // the fallback preserves the column count, so the cursor
                        // arithmetic below stays exact.
                        let text = self.emitter.transliterate(sym.as_str());
                        // Writing into the bottom-right cell makes the terminal
                        // wrap; disable autowrap around exactly that glyph.
                        let reaches_bottom_right = *y == terminal_h.saturating_sub(1)
                            && current_x.saturating_add(width.advance()) >= terminal_w;
                        if reaches_bottom_right {
                            self.scratch.write_all(DISABLE_LINE_WRAP)?;
                        }
                        self.scratch.write_all(text.as_bytes())?;
                        if reaches_bottom_right {
                            self.scratch.write_all(ENABLE_LINE_WRAP)?;
                        }
                        // A wide glyph advances two columns: the trailing
                        // continuation column is implicit and never emitted.
                        current_x = current_x.saturating_add(width.advance());
                        if let Some((cx, _)) = self.cur.as_mut() {
                            *cx = cx.saturating_add(width.advance());
                        }
                    }
                }
                Draw::ClearEol { x, y, style, width } => {
                    self.move_to_buf(*x, *y)?;
                    self.emitter.apply_style(*style, &mut self.scratch)?;
                    let use_bce = matches!(self.bce, Bce::Yes) && style.bg == Color::Reset;
                    if use_bce {
                        // `\x1b[K` clears from the cursor to end of line with the
                        // currently-set background, which is default here.
                        self.scratch.write_all(CLEAR_UNTIL_NEWLINE)?;
                    } else {
                        // No BCE, or a non-default background: paint explicit
                        // styled spaces to the edge without heap allocation.
                        let reaches_bottom_right = *y == terminal_h.saturating_sub(1)
                            && (*x).saturating_add(*width) >= terminal_w;
                        if reaches_bottom_right {
                            self.scratch.write_all(DISABLE_LINE_WRAP)?;
                        }
                        if *width > 0 {
                            csi::write_spaces(&mut self.scratch, *width as usize)?;
                        }
                        if reaches_bottom_right {
                            self.scratch.write_all(ENABLE_LINE_WRAP)?;
                        }
                        if let Some((cx, _)) = self.cur.as_mut() {
                            *cx = cx.saturating_add(*width);
                        }
                    }
                }
            }
        }
        self.commit_if_unbatched()?;
        Ok(processed)
    }

    /// Scroll the screen rows `[y, y+height)` using DECSTBM (scroll region) plus
    /// `CSI S` (SU, content up) or `CSI T` (SD, content down). The region is set,
    /// the scroll issued, and the region reset — all queued in order. Cursor
    /// position tracking is reset to unknown, because the terminal's post-scroll
    /// cursor position is not specified by the standard and varies by terminal.
    fn scroll_region(
        &mut self,
        y: u16,
        height: u16,
        amount: u16,
        terminal_h: u16,
        up: bool,
    ) -> io::Result<()> {
        // DECSTBM is 1-based and inclusive; a zero height means "to the bottom".
        let top = y.saturating_add(1);
        let bottom = y.saturating_add(height).min(terminal_h);
        if height == 0 || bottom <= y {
            return Ok(());
        }
        // Some terminals clamp a scroll region to at least two rows; a one-row
        // band has nothing to scroll.
        if bottom <= top {
            return Ok(());
        }
        csi::write_scroll_region(&mut self.scratch, top, bottom)?;
        // Home the cursor inside the region first: SU/SD scroll relative to the
        // cursor row on terminals that do not treat the region as absolute.
        csi::write_move_to(&mut self.scratch, 0, y)?;
        if up {
            csi::write_scroll_up(&mut self.scratch, amount)?;
        } else {
            csi::write_scroll_down(&mut self.scratch, amount)?;
        }
        // Reset the scroll region; the cursor tracking is invalidated below.
        self.scratch.write_all(RESET_SCROLL_REGION)?;
        self.cur = None; // the terminal moved the cursor; forget our tracking
        Ok(())
    }

    /// Move the terminal cursor to `(x, y)` inside `self.scratch` if we are not
    /// already there.
    fn move_to_buf(&mut self, x: u16, y: u16) -> io::Result<()> {
        if self.cur == Some((x, y)) {
            return Ok(());
        }
        csi::write_move_to(&mut self.scratch, x, y)?;
        self.cur = Some((x, y));
        Ok(())
    }

    /// Hide the terminal cursor, deduped against the last visibility we set.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        if self.cursor_visible != Some(false) {
            self.scratch.write_all(CURSOR_HIDE)?;
            self.cursor_visible = Some(false);
        }
        self.commit_if_unbatched()
    }

    /// Hide the terminal cursor and park its physical position at `(x, y)`.
    ///
    /// Some terminal/multiplexer/IME stacks sample the hidden cursor's physical
    /// coordinate as a composition-window hint. Parking it after hiding keeps
    /// "no visible caret" states from inheriting the last diff write position,
    /// which moves during streaming transcript updates.
    pub fn hide_cursor_at(&mut self, x: u16, y: u16) -> io::Result<()> {
        if self.cursor_visible != Some(false) {
            self.scratch.write_all(CURSOR_HIDE)?;
            self.cursor_visible = Some(false);
        }
        self.move_to_buf(x, y)?;
        self.commit_if_unbatched()
    }

    /// Show the terminal cursor, deduped against the last visibility we set.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        if self.cursor_visible != Some(true) {
            self.scratch.write_all(CURSOR_SHOW)?;
            self.cursor_visible = Some(true);
        }
        self.commit_if_unbatched()
    }

    /// Show the terminal cursor at `(x, y)`, keeping the backend's cursor tracker
    /// in sync with the real terminal.
    ///
    /// Positioning happens while the cursor is still hidden. Showing first would
    /// expose the last draw coordinate for one terminal update before the final
    /// `MoveTo`, visible as a jumping caret whenever a terminal, multiplexer or
    /// IME does not present synchronized updates as one indivisible operation.
    pub fn show_cursor_at(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.move_to_buf(x, y)?;
        if self.cursor_visible != Some(true) {
            self.scratch.write_all(CURSOR_SHOW)?;
            self.cursor_visible = Some(true);
        }
        self.commit_if_unbatched()
    }

    /// Apply a style delta using the configured emitter.
    pub fn apply_style(&mut self, want: Style) -> io::Result<()> {
        self.emitter.apply_style(want, &mut self.scratch)?;
        self.commit_if_unbatched()
    }

    /// Reset style and cursor tracking — call after the application performs a
    /// wholesale screen clear or enters the alt screen, where the terminal's state
    /// no longer matches what we track.
    pub fn invalidate(&mut self) -> io::Result<()> {
        self.emitter.invalidate(&mut self.scratch)?;
        self.cur = None;
        self.cursor_visible = None;
        self.commit_if_unbatched()
    }

    #[inline]
    fn commit_if_unbatched(&mut self) -> io::Result<()> {
        if self.in_sync {
            Ok(())
        } else {
            self.flush_scratch()
        }
    }

    #[inline]
    fn flush_scratch(&mut self) -> io::Result<()> {
        if self.scratch.is_empty() {
            return Ok(());
        }
        let res = self.out.write_all(&self.scratch);
        self.scratch.clear();
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::CellWidth;
    use crate::buffer::cell::Cell;
    use crate::buffer::grid::{CellGrid, Damage, Fit};

    /// Capture backend: accumulates the raw bytes emitted so tests can assert
    /// the exact escape sequence without a real terminal.
    fn render_to_string(cmd: &DrawCmd, bce: Bce) -> String {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf)
                .with_bce(bce)
                .with_profile(crate::render::profile::TerminalProfile::direct_color());
            be.render(cmd).unwrap();
        }
        String::from_utf8(buf).unwrap()
    }

    /// Counts how many `write()` calls hit the underlying sink.
    #[derive(Default)]
    struct WriteCountingSink {
        bytes: Vec<u8>,
        write_calls: usize,
        flush_calls: usize,
    }

    impl Write for WriteCountingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            Ok(())
        }
    }

    #[test]
    fn synchronized_frame_coalesces_into_a_single_write_call() {
        let mut sink = WriteCountingSink::default();
        let cmd = DrawCmd {
            w: 10,
            h: 2,
            draws: vec![
                Draw::Cells {
                    x: 0,
                    y: 0,
                    style: Style::default().fg(Color::Rgb(10, 20, 30)),
                    cells: vec![
                        ("H".into(), CellWidth::Narrow),
                        ("i".into(), CellWidth::Narrow),
                    ],
                },
                Draw::ClearEol {
                    x: 2,
                    y: 0,
                    style: Style::RESET,
                    width: 8,
                },
            ],
        };
        {
            let mut be = Backend::new(&mut sink)
                .with_bce(Bce::Yes)
                .with_profile(TerminalProfile::direct_color());
            be.begin_sync_update().unwrap();
            be.hide_cursor().unwrap();
            be.render(&cmd).unwrap();
            be.show_cursor_at(2, 0).unwrap();
            be.end_sync_update().unwrap();
        }
        assert_eq!(sink.write_calls, 1, "entire frame must land in one write");
        assert_eq!(sink.flush_calls, 1, "entire frame must flush once");
        let s = String::from_utf8(sink.bytes).unwrap();
        assert!(s.starts_with("\x1b[?2026h"));
        assert!(s.ends_with("\x1b[?2026l"));
    }

    #[test]
    fn empty_diff_emits_nothing() {
        let cmd = DrawCmd::default();
        assert_eq!(render_to_string(&cmd, Bce::Yes), "");
    }

    #[test]
    fn single_run_emits_move_sgr_and_text() {
        let mut back = CellGrid::new(4, 1);
        let mut damage = Damage::new(1);
        back.put(
            &mut damage,
            0,
            0,
            Fit::Clip,
            Style::default().fg(Color::Rgb(1, 2, 3)),
            "ab",
        );
        let front = CellGrid::new(4, 1);
        let cmd = crate::render::diff::diff(&back, &damage, &front);
        let s = render_to_string(&cmd, Bce::Yes);
        assert!(s.contains("\x1b[38;2;1;2;3m"), "fg SGR present: {s:?}");
        assert!(s.contains("ab"));
    }

    #[test]
    fn repeated_style_emits_no_duplicate_sgr() {
        // Two adjacent runs with the same style should emit the SGR once.
        let mut back = CellGrid::new(4, 1);
        let mut damage = Damage::new(1);
        let style = Style::default().fg(Color::Rgb(9, 9, 9));
        back.put(&mut damage, 0, 0, Fit::Clip, style, "a");
        back.set(&mut damage, 2, 0, Cell::narrow("b", style));
        let front = CellGrid::new(4, 1);
        let cmd = crate::render::diff::diff(&back, &damage, &front);
        let s = render_to_string(&cmd, Bce::Yes);
        // Count occurrences of the SGR set; should appear exactly once.
        let count = s.matches("\x1b[38;2;9;9;9m").count();
        assert_eq!(count, 1, "SGR emitted once, got: {s:?}");
    }

    #[test]
    fn cursor_positioning_keeps_next_frame_draws_honest() {
        let first = DrawCmd {
            w: 4,
            h: 3,
            draws: vec![Draw::Cells {
                x: 0,
                y: 0,
                style: Style::default(),
                cells: vec![("A".into(), CellWidth::Narrow)],
            }],
        };
        let second = DrawCmd {
            w: 4,
            h: 3,
            draws: vec![Draw::Cells {
                // This is exactly where the first render left the backend's
                // cursor tracker. If the visible caret move below does not
                // update that tracker, the second render omits MoveTo and
                // writes at the real caret position instead.
                x: 1,
                y: 0,
                style: Style::default(),
                cells: vec![("B".into(), CellWidth::Narrow)],
            }],
        };

        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.render(&first).unwrap();
            be.show_cursor_at(0, 2).unwrap();
            assert_eq!(be.cur, Some((0, 2)));
            be.render(&second).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let caret_move = s
            .find("\x1b[3;1H")
            .expect("caret move to row 3 col 1 must be emitted");
        let redraw_move = s[caret_move..]
            .find("\x1b[1;2H")
            .expect("next frame must move back to row 1 col 2 before drawing")
            + caret_move;
        let redraw_text = s[redraw_move..]
            .find('B')
            .expect("second frame text must be emitted")
            + redraw_move;
        assert!(redraw_move < redraw_text, "MoveTo must precede B: {s:?}");
    }

    #[test]
    fn show_cursor_at_positions_before_revealing_the_cursor() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.hide_cursor().unwrap();
            be.show_cursor_at(5, 7).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let move_at = s
            .find("\x1b[8;6H")
            .expect("final caret MoveTo must be emitted");
        let show_at = s.find("\x1b[?25h").expect("cursor Show must be emitted");
        assert!(
            move_at < show_at,
            "the cursor must be positioned while hidden, then shown: {s:?}"
        );
    }

    #[test]
    fn hidden_cursor_can_be_parked_at_stable_coordinate() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.hide_cursor_at(3, 2).unwrap();
            assert_eq!(be.cur, Some((3, 2)));
        }

        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("\x1b[?25l"),
            "cursor hide must be emitted: {s:?}"
        );
        assert!(
            s.contains("\x1b[3;4H"),
            "hidden cursor must be moved to row 3 col 4: {s:?}"
        );
    }

    #[test]
    fn repeated_hide_does_not_reamit_visibility_sequence() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.hide_cursor_at(3, 2).unwrap();
            be.hide_cursor_at(4, 5).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let hide_count = s.matches("\x1b[?25l").count();
        assert_eq!(hide_count, 1, "hide emitted once, got: {s:?}");
        assert!(
            s.contains("\x1b[6;5H"),
            "second park must move to row 6 col 5: {s:?}"
        );
    }

    #[test]
    fn show_hide_toggle_emits_only_on_transition() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.hide_cursor().unwrap(); // None -> Some(false): emit ?25l
            be.hide_cursor().unwrap(); // Some(false): silent
            be.show_cursor().unwrap(); // -> Some(true): emit ?25h
            be.show_cursor().unwrap(); // Some(true): silent
            be.hide_cursor().unwrap(); // -> Some(false): emit ?25l
        }

        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.matches("\x1b[?25l").count(),
            2,
            "hide emitted on transitions only: {s:?}"
        );
        assert_eq!(
            s.matches("\x1b[?25h").count(),
            1,
            "show emitted on transition only: {s:?}"
        );
    }

    #[test]
    fn invalidate_forces_next_visibility_to_reemit() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
            be.hide_cursor().unwrap(); // emit ?25l
            be.hide_cursor().unwrap(); // silent
            be.invalidate().unwrap();
            let _ = be.writer().flush();
            be.hide_cursor().unwrap(); // must re-emit after invalidate
        }

        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.matches("\x1b[?25l").count(),
            2,
            "invalidate resets visibility tracking: {s:?}"
        );
    }

    #[test]
    fn non_bce_clear_eol_paints_bottom_right_under_line_wrap_guard() {
        let cmd = DrawCmd {
            w: 4,
            h: 2,
            draws: vec![Draw::ClearEol {
                x: 2,
                y: 1,
                style: Style::default().bg(Color::Rgb(7, 8, 9)),
                width: 2,
            }],
        };

        let s = render_to_string(&cmd, Bce::No);
        let guard_off = s
            .find("\x1b[?7l")
            .expect("bottom-right clear must disable line wrap");
        let guard_on = s[guard_off..]
            .find("\x1b[?7h")
            .expect("bottom-right clear must restore line wrap")
            + guard_off;
        let spaces = s[guard_off..guard_on].matches(' ').count();
        assert_eq!(spaces, 2, "clear must paint through the corner: {s:?}");
    }

    #[test]
    fn scroll_up_emits_scroll_region_and_su() {
        let cmd = DrawCmd {
            w: 20,
            h: 10,
            draws: vec![Draw::ScrollUp {
                y: 1,
                height: 8,
                amount: 2,
            }],
        };
        let s = render_to_string(&cmd, Bce::Yes);
        assert!(s.contains("\x1b[2;9r"), "scroll region set: {s:?}");
        assert!(s.contains("\x1b[2;1H"), "cursor homed inside region: {s:?}");
        assert!(s.contains("\x1b[2S"), "scroll-up issued: {s:?}");
        assert!(s.contains("\x1b[r"), "scroll region reset: {s:?}");
    }

    #[test]
    fn scroll_down_emits_scroll_region_and_sd() {
        let cmd = DrawCmd {
            w: 20,
            h: 10,
            draws: vec![Draw::ScrollDown {
                y: 0,
                height: 10,
                amount: 1,
            }],
        };
        let s = render_to_string(&cmd, Bce::Yes);
        assert!(s.contains("\x1b[1;10r"), "full-screen region: {s:?}");
        assert!(s.contains("\x1b[1T"), "scroll-down issued: {s:?}");
        assert!(s.contains("\x1b[r"), "region reset: {s:?}");
    }

    #[test]
    fn bce_detection_defaults_for_known_terms() {
        assert_eq!(Bce::for_term("xterm-256color", None), Bce::Yes);
        assert_eq!(Bce::for_term("tmux-256color", None), Bce::Yes);
        assert_eq!(Bce::for_term("screen-256color", None), Bce::Yes);
        assert_eq!(Bce::for_term("foot", None), Bce::Yes);
        assert_eq!(Bce::for_term("dumb", None), Bce::No);
        assert_eq!(Bce::for_term("unknown-term", None), Bce::No);
    }

    #[test]
    fn bce_override_env_wins() {
        assert_eq!(Bce::for_term("xterm-256color", Some("0")), Bce::No);
        assert_eq!(Bce::for_term("dumb", Some("1")), Bce::Yes);
        assert_eq!(Bce::for_term("xterm-256color", Some("maybe")), Bce::Yes);
    }

    #[test]
    fn sync_update_envelope_brackets_out_of_band_writes() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::new(&mut buf)
                .with_bce(Bce::Yes)
                .with_profile(crate::render::profile::TerminalProfile::direct_color());
            be.begin_sync_update().unwrap();
            be.show_cursor_at(5, 7).unwrap();
            be.end_sync_update().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("\x1b[?2026h"), "envelope opens first: {s:?}");
        assert!(s.ends_with("\x1b[?2026l"), "envelope closes last: {s:?}");
        assert!(
            s.contains("\x1b[?25h") && s.contains("\x1b[8;6H"),
            "the out-of-band caret write sits inside the envelope: {s:?}"
        );
        assert!(
            s.find("\x1b[8;6H") < s.find("\x1b[?25h"),
            "the caret is positioned before it becomes visible: {s:?}"
        );
    }

    #[test]
    fn sync_update_envelopes_never_nest() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::new(&mut buf)
                .with_bce(Bce::Yes)
                .with_profile(crate::render::profile::TerminalProfile::direct_color());
            for _ in 0..3 {
                be.begin_sync_update().unwrap();
                be.hide_cursor_at(0, 0).unwrap();
                be.end_sync_update().unwrap();
            }
        }
        let s = String::from_utf8(buf).unwrap();
        let begins = s.matches("\x1b[?2026h").count();
        let ends = s.matches("\x1b[?2026l").count();
        assert_eq!(begins, 3, "one begin per envelope: {s:?}");
        assert_eq!(ends, 3, "one end per envelope: {s:?}");
        let mut depth = 0i32;
        let mut pos = 0usize;
        let mut properly_nested = true;
        while let Some(rel) = s[pos..].find("\x1b[?2026") {
            let at = pos + rel;
            let is_begin = s[at + 7..].starts_with('h');
            if is_begin {
                depth += 1;
                if depth > 1 {
                    properly_nested = false;
                }
            } else {
                depth -= 1;
            }
            pos = at + 7;
        }
        assert!(properly_nested, "envelopes never nest: {s:?}");
        assert_eq!(depth, 0, "every envelope closes: {s:?}");
    }

    #[test]
    fn monochrome_profile_emits_no_color_escape_codes() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be =
                Backend::new(&mut buf).with_profile(TerminalProfile::dec_vt100_monochrome());
            let cmd = DrawCmd {
                w: 10,
                h: 2,
                draws: vec![
                    Draw::Cells {
                        x: 0,
                        y: 0,
                        style: Style::default()
                            .fg(Color::Rgb(255, 0, 0))
                            .bg(Color::Rgb(0, 0, 255)),
                        cells: vec![("A".into(), CellWidth::Narrow)],
                    },
                    Draw::Cells {
                        x: 1,
                        y: 0,
                        style: Style::default().fg(Color::White),
                        cells: vec![("B".into(), CellWidth::Narrow)],
                    },
                ],
            };
            be.render(&cmd).unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            !s.contains("\x1b[38;"),
            "no 38; color codes in monochrome: {s:?}"
        );
        assert!(
            !s.contains("\x1b[48;"),
            "no 48; color codes in monochrome: {s:?}"
        );
        assert!(
            !s.contains("\x1b[31m"),
            "no ANSI color codes in monochrome: {s:?}"
        );
        assert!(
            s.contains("\x1b[7m"),
            "highlighted cell uses reverse video: {s:?}"
        );
        assert!(s.contains('A'));
        assert!(s.contains('B'));
    }

    #[test]
    fn monochrome_profile_disables_sync_update() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be =
                Backend::new(&mut buf).with_profile(TerminalProfile::dec_vt100_monochrome());
            be.begin_sync_update().unwrap();
            be.end_sync_update().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            !s.contains("?2026h"),
            "monochrome profile must not emit sync update: {s:?}"
        );
        assert!(
            !s.contains("?2026l"),
            "monochrome profile must not emit sync update: {s:?}"
        );
    }

    #[test]
    fn ansi16_profile_quantizes_rgb_to_standard_ansi() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::new(&mut buf).with_profile(TerminalProfile::ecma48_ansi16());
            let cmd = DrawCmd {
                w: 10,
                h: 2,
                draws: vec![Draw::Cells {
                    x: 0,
                    y: 0,
                    style: Style::default().fg(Color::Rgb(255, 10, 10)),
                    cells: vec![("X".into(), CellWidth::Narrow)],
                }],
            };
            be.render(&cmd).unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            !s.contains("\x1b[38;2;"),
            "ANSI16 profile must not emit DirectColor 38;2: {s:?}"
        );
        assert!(s.contains('X'));
    }
}
