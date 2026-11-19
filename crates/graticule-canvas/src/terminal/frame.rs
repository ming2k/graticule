//! `Frame` and `Terminal`: the per-tick draw entry point and the commit loop.
//!
//! [`Frame`] is the handle the application renders into. It borrows the back grid
//! and its damage tracker, exposes the area (and clipped painters) plus the
//! desired cursor position, and owns no terminal state.
//!
//! [`Terminal`] owns the [`Backend`], the back grid (desired state), the front
//! grid (what the terminal currently shows), and the back grid's damage. Its
//! [`Terminal::draw`] renders the closure into the back grid, diffs it against
//! the front grid, hands the [`DrawCmd`] to the backend, and — only after the
//! bytes have been flushed — promotes the changed cells into the front grid.
//!
//! That ordering is the module's central invariant: **the retained model can never
//! run ahead of the physical terminal**. An idle frame (clean damage, unchanged
//! cursor) emits zero bytes.
//!
//! Raw mode, alternate screen, and OS TTY size queries are deliberately outside
//! this pure-memory crate: construct a fixed-size terminal with
//! [`Terminal::with_size`], or attach a live OS query via
//! [`Terminal::with_size_query`] (or `graticule::live_terminal`).

use std::io::{self, Write};

use crate::buffer::grid::{CellGrid, Damage};
use crate::buffer::{Cell, Style};
use crate::layout::Rect;
use crate::render::diff::{self, DrawCmd};
use crate::render::driver::{Driver, EscapeEmitter};
use crate::render::paint::{Paint, Widget};
use crate::terminal::backend::Backend;

/// The terminal cursor mode a frame should end with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorState {
    /// No visible caret.
    #[default]
    Hidden,
    /// A visible caret at `(x, y)`.
    Visible(u16, u16),
}

/// A per-draw handle over the back grid.
///
/// Widgets and direct painters both go through [`Frame::paint`], which yields a
/// clipped [`Paint`]. There is deliberately no unclipped mutable grid accessor:
/// a write that bypasses the damage tracker would leave the retained model and
/// the terminal permanently out of step.
pub struct Frame<'a> {
    grid: &'a mut CellGrid,
    damage: &'a mut Damage,
    area_rect: Rect,
    cursor: CursorState,
}

impl<'a> Frame<'a> {
    /// Construct a frame over a grid and its damage tracker.
    pub fn new(grid: &'a mut CellGrid, damage: &'a mut Damage) -> Self {
        let area_rect = grid.area();
        Self {
            grid,
            damage,
            area_rect,
            cursor: CursorState::default(),
        }
    }

    /// The full terminal area this frame covers.
    pub fn area(&self) -> Rect {
        self.area_rect
    }

    /// What the frame has marked dirty so far.
    pub fn damage(&self) -> &Damage {
        self.damage
    }

    /// Run `f` against a painter clipped to `area`.
    ///
    /// This is the only way to reach the grid, and `area` is intersected with the
    /// frame's own area, so a painter can never draw outside its allocation.
    pub fn paint<A>(&mut self, area: Rect, f: impl FnOnce(&mut Paint<'_>) -> A) -> A {
        let mut paint = Paint::new(self.grid, self.damage, area);
        f(&mut paint)
    }

    /// Render a widget into `area`.
    pub fn render_widget<W: Widget + ?Sized>(&mut self, widget: &mut W, area: Rect) {
        let mut paint = Paint::new(self.grid, self.damage, area);
        widget.render(area, &mut paint);
    }

    /// Set the cursor position for after this frame's flush. The last call wins.
    pub fn set_cursor_position<P: Into<(u16, u16)>>(&mut self, pos: P) {
        let (x, y) = pos.into();
        self.cursor = if self.area_rect.is_empty() {
            CursorState::Hidden
        } else {
            CursorState::Visible(
                x.min(self.area_rect.right().saturating_sub(1)),
                y.min(self.area_rect.bottom().saturating_sub(1)),
            )
        };
    }

    /// Write a styled string at `(x, y)`, clipped to the frame area.
    pub fn put(&mut self, x: u16, y: u16, style: Style, text: &str) {
        let mut paint = Paint::new(self.grid, self.damage, self.area_rect);
        paint.put(x, y, crate::buffer::grid::Fit::Clip, style, text);
    }

    /// Change one cell's style in place, keeping its glyph. The sanctioned way to
    /// implement a dim/recess effect or a scrollbar highlight.
    pub fn restyle(&mut self, x: u16, y: u16, f: impl FnOnce(&mut Style)) {
        let area = self.area_rect;
        self.paint(area, |paint| paint.restyle(x, y, f));
    }

    /// Read a cell from the back grid.
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.grid.get(x, y)
    }

    pub(crate) fn take_cursor(&mut self) -> CursorState {
        std::mem::take(&mut self.cursor)
    }
}

#[derive(Clone, Copy)]
enum SizeSource {
    Fixed,
    Query(fn() -> io::Result<(u16, u16)>),
}

/// Owns the backend and the retained state for one render loop.
pub struct Terminal<W: Write, E: EscapeEmitter = Driver> {
    backend: Backend<W, E>,
    size_source: SizeSource,
    /// Desired state.
    back: CellGrid,
    /// What the terminal currently shows.
    front: CellGrid,
    /// Damage for `back`.
    damage: Damage,
    cursor: CursorState,
    /// The cursor state the terminal was last committed into.
    presented_cursor: Option<CursorState>,
    /// Where a hidden caret should be parked so an IME samples a stable
    /// coordinate while cells stream underneath it.
    cursor_anchor: Option<(u16, u16)>,
    /// The next commit must reset the terminal and repaint everything, because
    /// the terminal's state is unknown (resize, or a failed write).
    pending_clear: bool,
}

impl<W: Write, E: EscapeEmitter> Terminal<W, E> {
    /// Create a pure, deterministic terminal with explicit `(width, height)`.
    ///
    /// Performs zero OS syscalls. Call [`Self::resize_to`] whenever the host
    /// viewport changes (e.g. on `Event::Resize`).
    pub fn with_size(backend: Backend<W, E>, width: u16, height: u16) -> Self {
        Self {
            backend,
            size_source: SizeSource::Fixed,
            back: CellGrid::new(width, height),
            front: CellGrid::new_uninitialized(width, height),
            damage: Damage::new(height),
            cursor: CursorState::Hidden,
            presented_cursor: None,
            cursor_anchor: None,
            pending_clear: false,
        }
    }

    /// Create a terminal whose viewport tracks an external size query function
    /// (typically `graticule_tty::terminal_size`), falling back to `80x24` if
    /// the initial query fails.
    pub fn with_size_query(backend: Backend<W, E>, query: fn() -> io::Result<(u16, u16)>) -> Self {
        let (w, h) = query().unwrap_or((80, 24));
        Self {
            backend,
            size_source: SizeSource::Query(query),
            back: CellGrid::new(w, h),
            front: CellGrid::new_uninitialized(w, h),
            damage: Damage::new(h),
            cursor: CursorState::Hidden,
            presented_cursor: None,
            cursor_anchor: None,
            pending_clear: false,
        }
    }

    /// Resize the grids, discarding the front-grid mirror: the next commit resets
    /// the terminal and repaints in full, because a resize leaves unknown content
    /// (and a stale scrollback view) behind.
    pub fn resize_to(&mut self, width: u16, height: u16) {
        if self.back.size() == (width, height) {
            return;
        }
        self.back.resize(&mut self.damage, width, height);
        self.front = CellGrid::new_uninitialized(width, height);
        self.pending_clear = true;
    }

    /// Run the draw closure against a fresh frame, then commit it.
    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        self.commit()
    }

    /// Render into the retained back grid *without* emitting anything.
    ///
    /// The next [`Self::commit_staged`] (or [`Self::draw`]) replaces or completes
    /// this staged frame and commits only the final grid. This supports
    /// layout-dependent state such as bottom-follow scrolling without flashing an
    /// intermediate viewport.
    pub fn stage<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        Ok(())
    }

    /// Commit a previously [`Self::stage`]d frame.
    pub fn commit_staged(&mut self) -> io::Result<()> {
        self.commit()
    }

    fn render_frame<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        // Sync the grid size with the configured size query, if any.
        if let SizeSource::Query(query) = self.size_source
            && let Ok((w, h)) = query()
            && self.back.size() != (w, h)
        {
            self.resize_to(w, h);
        }
        let mut frame = Frame::new(&mut self.back, &mut self.damage);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    fn commit(&mut self) -> io::Result<()> {
        let cmd = diff::diff(&self.back, &self.damage, &self.front);
        let desired_cursor = self.normalized_cursor(self.cursor);
        let cursor_changed = self.presented_cursor != Some(desired_cursor);

        // Repainting identical content may leave conservative dirty marks but no
        // terminal commands. Clear those marks without opening an otherwise empty
        // synchronized-update envelope.
        if !self.pending_clear && cmd.is_empty() && !cursor_changed {
            diff::promote(&self.back, &mut self.damage, &mut self.front);
            return Ok(());
        }

        if let Err(error) = self.backend.begin_sync_update() {
            self.recover_failed_commit();
            return Err(error);
        }
        let commit_result = self.commit_frame(&cmd, desired_cursor);
        // Always close an envelope that was opened, even when a queued write
        // failed, so the terminal never remains update-suspended.
        let end_result = self.backend.end_sync_update();
        let result = match (commit_result, end_result) {
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        };

        if let Err(error) = result {
            self.recover_failed_commit();
            return Err(error);
        }

        // Only a successful flush commits logical terminal state: therefore
        // `diff` cannot have mutated `front`, and the scroll projection is applied
        // here, after the bytes landed.
        diff::promote_scrolled(&self.back, &mut self.damage, &mut self.front, &cmd);
        self.pending_clear = false;
        self.presented_cursor = Some(desired_cursor);
        if let CursorState::Visible(x, y) = desired_cursor {
            self.cursor_anchor = Some((x, y));
        }
        Ok(())
    }

    /// Emit one already-rendered logical frame. [`Self::commit`] owns the
    /// synchronized-update envelope around this method.
    fn commit_frame(&mut self, cmd: &DrawCmd, desired_cursor: CursorState) -> io::Result<()> {
        if self.pending_clear {
            // Reconcile the emitter's tracked SGR state and the real terminal only
            // when a frame is actually committed. A staged layout pass may resize
            // the grids, but it must remain completely invisible.
            self.backend.invalidate()?;
        }

        // Hide before any command that can expose a transient physical cursor
        // coordinate. Correctness does not depend on DEC synchronized-update
        // support: unsupported terminals may present these bytes separately, but
        // the caret stays hidden until its final position is installed.
        if self.pending_clear || !cmd.is_empty() {
            self.backend.hide_cursor()?;
        }

        if self.pending_clear {
            // A resize (or a recovered failed write) leaves unknown content
            // behind: home and clear, then repaint the whole grid.
            self.backend.clear_screen()?;
        }

        // Every cell write moves the physical cursor, so the draw commands must
        // be emitted *before* the final caret coordinate is installed. Placing
        // the caret first lets the diff drag it to the end of the last written
        // run, which is visible as a caret jumping around the screen.
        self.backend.render(cmd)?;

        // Install the final coordinate while hidden, then reveal the cursor.
        // Hidden frames return it to the last input anchor so IME sampling is
        // stable even while cells stream underneath.
        match desired_cursor {
            CursorState::Hidden => {
                if let Some((x, y)) = self.normalized_anchor() {
                    self.backend.hide_cursor_at(x, y)?;
                } else {
                    self.backend.hide_cursor()?;
                }
            }
            CursorState::Visible(x, y) => {
                self.backend.show_cursor_at(x, y)?;
            }
        }
        Ok(())
    }

    fn normalized_cursor(&self, cursor: CursorState) -> CursorState {
        let (w, h) = self.back.size();
        match cursor {
            CursorState::Visible(x, y) if w > 0 && h > 0 => {
                CursorState::Visible(x.min(w - 1), y.min(h - 1))
            }
            _ => CursorState::Hidden,
        }
    }

    fn normalized_anchor(&self) -> Option<(u16, u16)> {
        let (w, h) = self.back.size();
        let (x, y) = self.cursor_anchor?;
        (w > 0 && h > 0).then_some((x.min(w - 1), y.min(h - 1)))
    }

    fn recover_failed_commit(&mut self) {
        // The terminal may have consumed an arbitrary prefix of the failed stream.
        // Force the next attempt through reset + clear + full repaint, and forget
        // cursor state until that recovery frame flushes.
        self.damage.mark_all();
        self.pending_clear = true;
        self.presented_cursor = None;
    }

    /// Borrow the underlying writer, for raw-mode / alternate-screen / mouse setup.
    pub fn writer(&mut self) -> &mut W {
        self.backend.writer()
    }

    /// Borrow the backend (e.g. to `invalidate` after the application clears the
    /// screen itself).
    pub fn backend(&mut self) -> &mut Backend<W, E> {
        &mut self.backend
    }

    /// The grid size, which the frame loop keeps in step with the physical
    /// terminal.
    ///
    /// This does **not** query the terminal: it reports the size observed at the
    /// last [`Self::draw`], so calling it in a tight loop costs nothing.
    pub fn size(&self) -> (u16, u16) {
        self.back.size()
    }

    /// What the pending frame has marked dirty. Empty after a successful commit.
    pub fn damage(&self) -> &Damage {
        &self.damage
    }

    /// Show the cursor, deduped against the last visibility the backend set.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        let result = self.backend.writer().flush();
        self.presented_cursor = None;
        result
    }

    /// Hide the cursor, deduped against the last visibility the backend set.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        let result = self.backend.writer().flush();
        self.presented_cursor = None;
        result
    }
}

/// A headless terminal for tests and for applications that render off-line.
///
/// It owns a back grid and its damage but no backend, so a test can render into a
/// buffer and inspect the cells and the requested caret without any I/O.
pub struct TestTerminal {
    back: CellGrid,
    damage: Damage,
    cursor: CursorState,
}

impl TestTerminal {
    /// Create a test terminal with a grid of the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            back: CellGrid::new(width, height),
            damage: Damage::new(height),
            cursor: CursorState::default(),
        }
    }

    /// Run a draw closure against a frame over the back grid.
    ///
    /// Damage from the previous draw is discarded first, mirroring the commit a
    /// real terminal performs: [`Self::damage`] afterwards describes exactly this
    /// frame.
    pub fn draw<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.damage.clear();
        let mut frame = Frame::new(&mut self.back, &mut self.damage);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    /// The rendered grid.
    pub fn buffer(&self) -> &CellGrid {
        &self.back
    }

    /// What the last draw marked dirty.
    pub fn damage(&self) -> &Damage {
        &self.damage
    }

    /// The caret position the last draw closure requested (or `Hidden`).
    pub fn cursor(&self) -> CursorState {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Color;
    use crate::terminal::backend::Bce;

    /// A profile-independent backend: truecolor, no BCE, no env reads.
    fn headless() -> Backend<Vec<u8>, Driver> {
        Backend::new(Vec::new())
            .with_bce(Bce::No)
            .with_profile(crate::render::profile::TerminalProfile::direct_color())
    }

    /// A writer that fails when it sees a chosen byte, so partial-write recovery
    /// can be exercised.
    #[derive(Default)]
    struct FailOnByteWriter {
        bytes: Vec<u8>,
        fail_on: Option<u8>,
    }

    impl Write for FailOnByteWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_on.is_some_and(|needle| buf.contains(&needle)) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected terminal write failure",
                ));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn staged_frame_emits_only_the_final_committed_grid() {
        let mut terminal = Terminal::with_size(headless(), 80, 24);
        terminal
            .stage(|frame| {
                frame.put(0, 0, Style::default(), "intermediate");
            })
            .unwrap();
        assert!(
            terminal.writer().is_empty(),
            "a staged frame must not reach the terminal"
        );

        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "final");
            })
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();
        assert!(rendered.contains("final"));
        assert!(!rendered.contains("intermediate"));
    }

    #[test]
    fn identical_frame_emits_no_envelope_or_cursor_bytes() {
        let mut terminal = Terminal::with_size(headless(), 80, 24);
        let paint = |frame: &mut Frame<'_>| {
            frame.put(0, 0, Style::default(), "same");
            frame.set_cursor_position((4, 0));
        };
        terminal.draw(paint).unwrap();
        terminal.writer().clear();

        terminal.draw(paint).unwrap();
        assert!(
            terminal.writer().is_empty(),
            "an identical logical frame must be a zero-byte commit"
        );
    }

    #[test]
    fn incremental_frame_positions_caret_before_showing_it() {
        let mut terminal = Terminal::with_size(headless(), 80, 24);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((2, 1));
            })
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();
        let a_idx = rendered.find('A').expect("cell write");
        let move_idx = rendered.find("\x1b[2;3H").expect("caret move");
        let show_idx = rendered.find("\x1b[?25h").expect("caret reveal");
        assert!(
            a_idx < move_idx,
            "draw commands must be emitted before the caret is moved/revealed: {rendered:?}"
        );
        assert!(
            move_idx < show_idx,
            "the caret must be positioned before it is revealed: {rendered:?}"
        );
    }

    #[test]
    fn cursor_only_frame_moves_without_visibility_toggle() {
        let mut terminal = Terminal::with_size(headless(), 80, 24);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((2, 2));
            })
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((2, 2));
            })
            .unwrap();
        assert!(terminal.writer().is_empty());
    }

    #[test]
    fn hidden_frame_parks_at_last_visible_input_anchor() {
        let mut terminal = Terminal::with_size(headless(), 80, 24);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((5, 5));
            })
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| frame.put(0, 0, Style::default(), "B"))
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();
        assert!(
            rendered.contains("\x1b[6;6H"),
            "hidden cursor returns to the stable input anchor: {rendered:?}"
        );
        assert!(
            !rendered.contains("\x1b[?25h"),
            "a hidden frame never reveals the cursor: {rendered:?}"
        );
    }

    #[test]
    fn frame_clamps_requested_cursor_to_terminal_bounds() {
        let mut terminal = TestTerminal::new(4, 3);
        terminal.draw(|frame| frame.set_cursor_position((u16::MAX, u16::MAX)));
        assert_eq!(terminal.cursor(), CursorState::Visible(3, 2));
        assert_eq!(terminal.buffer().size(), (4, 3));
    }

    #[test]
    fn failed_frame_closes_envelope_then_forces_clear_and_full_repaint() {
        let backend = Backend::new(FailOnByteWriter::default())
            .with_bce(Bce::No)
            .with_profile(crate::render::profile::TerminalProfile::direct_color());
        let mut terminal = Terminal::with_size(backend, 80, 24);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((1, 0));
            })
            .unwrap();

        terminal.writer().bytes.clear();
        terminal.writer().fail_on = Some(b'B');
        assert!(
            terminal
                .draw(|frame| {
                    frame.put(0, 0, Style::default(), "B");
                    frame.set_cursor_position((1, 0));
                })
                .is_err(),
            "the injected write failure must surface"
        );

        let rendered = String::from_utf8(terminal.writer().bytes.clone()).unwrap();
        assert!(
            !rendered.contains("\x1b[?2026h") || rendered.contains("\x1b[?2026l"),
            "an opened envelope is always closed: {rendered:?}"
        );
        assert!(terminal.damage().is_dirty());
        assert!(terminal.damage().rows().is_some());
    }

    #[test]
    fn size_query_updates_viewport_on_draw() {
        fn fixed_query() -> io::Result<(u16, u16)> {
            Ok((42, 13))
        }
        let mut terminal = Terminal::with_size_query(headless(), fixed_query);
        assert_eq!(terminal.size(), (42, 13));
        terminal
            .draw(|frame| {
                assert_eq!(frame.area(), Rect::new(0, 0, 42, 13));
            })
            .unwrap();
    }

    #[test]
    fn resize_marks_everything_and_replaces_the_front_mirror() {
        let mut terminal = TestTerminal::new(3, 2);
        terminal.draw(|frame| {
            frame.put(0, 0, Style::default(), "abc");
        });
        terminal.draw(|frame| {
            frame.put(0, 0, Style::default(), "xy");
        });
        assert_eq!(terminal.buffer().size(), (3, 2));
    }

    #[test]
    fn paint_is_clipped_to_the_requested_area() {
        let mut terminal = TestTerminal::new(10, 3);
        terminal.draw(|frame| {
            frame.paint(Rect::new(2, 1, 3, 1), |paint| {
                paint.put(
                    0,
                    0,
                    crate::buffer::grid::Fit::Clip,
                    Style::default(),
                    "leak",
                );
                paint.put(
                    2,
                    1,
                    crate::buffer::grid::Fit::Clip,
                    Style::default(),
                    "inside",
                );
            });
        });
        assert_eq!(terminal.buffer().get(2, 1).unwrap().symbol, "i");
        assert_eq!(terminal.buffer().get(4, 1).unwrap().symbol, "s");
        assert_eq!(terminal.buffer().get(5, 1).unwrap().symbol, " ");
        assert_eq!(terminal.buffer().get(0, 0).unwrap().symbol, " ");
        assert_eq!(terminal.buffer().get(1, 1).unwrap().symbol, " ");
    }

    #[test]
    fn restyle_keeps_the_glyph_and_marks_damage() {
        let mut terminal = TestTerminal::new(4, 1);
        terminal.draw(|frame| frame.put(0, 0, Style::default(), "ab"));
        terminal.draw(|frame| {
            frame.restyle(1, 0, |s| s.bg = Color::Rgb(9, 8, 7));
        });
        let cell = terminal.buffer().get(1, 0).unwrap();
        assert_eq!(cell.symbol, "b");
        assert_eq!(cell.style.bg, Color::Rgb(9, 8, 7));
        assert_eq!(terminal.damage().col_of(0), Some(1));
    }

    #[test]
    fn size_reports_the_observed_grid_without_querying_the_terminal() {
        let terminal = TestTerminal::new(7, 4);
        assert_eq!(terminal.buffer().size(), (7, 4));
        let t = Terminal::with_size(headless(), 64, 20);
        assert_eq!(t.size(), (64, 20));
    }
}
