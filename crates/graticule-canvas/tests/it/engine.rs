//! End-to-end engine tests: the full write → diff → render → promote cycle,
//! plus the two scenarios the engine exists to fix:
//!
//! 1. **Wide-glyph ghost eradication.** Writing a CJK glyph marks its trailing
//!    column as a continuation carrying the glyph's own background, and the
//!    diff emits the glyph without ever leaking a stale `Color::Reset` cell —
//!    the class of ghost cell bug that trailing-skip produces through
//!    multiplexers like tmux.
//! 2. **Convergence.** After a diff is promoted into the front grid, a second
//!    diff is empty: the back grid and the (now-updated) front grid agree, so
//!    the terminal emits nothing on an idle frame.
//!
//! These run without a real terminal: the backend writes into a `Vec<u8>`.

use graticule_canvas::render::diff::{self, Draw, DrawCmd};
use graticule_canvas::{
    Backend, Bce, Cell, CellGrid, CellWidth, Color, Damage, Fit, Modifier, Pos, Style,
    TerminalProfile,
};

/// The full cycle over an in-memory capture buffer. Returns the emitted bytes.
fn render_cycle(
    back: &mut CellGrid,
    damage: &mut Damage,
    front: &mut CellGrid,
    bce: Bce,
) -> String {
    let cmd = diff::diff(back, damage, front);
    let mut buf = Vec::new();
    {
        let mut be = Backend::new(&mut buf)
            .with_bce(bce)
            .with_profile(TerminalProfile::direct_color());
        be.render(&cmd).unwrap();
    }
    diff::promote_scrolled(back, damage, front, &cmd);
    String::from_utf8(buf).unwrap()
}

#[test]
fn second_frame_after_promote_is_idle() {
    let (w, h) = (8, 2);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);

    // Frame 1: write some content.
    back.put(
        &mut damage,
        0,
        0,
        Fit::Clip,
        Style::default().fg(Color::Rgb(1, 1, 1)),
        "hello",
    );
    let first = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);
    assert!(first.contains("hello"));

    // Frame 2: nothing changed → diff is empty → backend emits nothing.
    let second = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);
    assert!(
        second.is_empty(),
        "idle frame should emit nothing, got {second:?}"
    );
}

#[test]
fn wide_glyph_trailing_column_carries_background_not_reset() {
    let mut back = CellGrid::new(6, 1);
    let mut damage = Damage::new(1);
    let style = Style::default()
        .fg(Color::Rgb(255, 255, 255))
        .bg(Color::Rgb(7, 8, 8)); // near-black surface
    back.put(&mut damage, 0, 0, Fit::Clip, style, "😀");

    // Trailing continuation cell: width 0, and its bg is the glyph's bg.
    let trail = back.get(1, 0).unwrap();
    assert!(trail.is_wide_continuation());
    assert_eq!(trail.style.bg, Color::Rgb(7, 8, 8));
    assert_ne!(
        trail.style.bg,
        Color::Reset,
        "trailing cell must never reset background"
    );

    let front = CellGrid::new(6, 1);
    let cmd = diff::diff(&back, &damage, &front);
    let emitted_cells: Vec<_> = cmd
        .draws
        .iter()
        .filter_map(|d| match d {
            Draw::Cells { cells, .. } => Some(cells.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(emitted_cells, vec![("😀".into(), CellWidth::Wide)]);
}

#[test]
fn wide_glyph_continuation_mismatch_forces_head_repainting() {
    let mut back = CellGrid::new(6, 1);
    let mut damage = Damage::new(1);
    let style = Style::default().bg(Color::Rgb(7, 8, 8));
    back.put(&mut damage, 0, 0, Fit::Clip, style, "中");

    // Front already has the head at col 0, but col 1 was corrupted by an orphaned background
    let mut front = CellGrid::new(6, 1);
    let mut front_damage = Damage::new(1);
    front.put(&mut front_damage, 0, 0, Fit::Clip, style, "中");
    // Corrupt front's continuation column with a stale selection highlight
    front.set(
        &mut front_damage,
        1,
        0,
        Cell::new(
            " ",
            CellWidth::Narrow,
            Style::default().bg(Color::Rgb(100, 100, 100)),
        ),
    );

    let cmd = diff::diff(&back, &damage, &front);
    let emitted_cells: Vec<_> = cmd
        .draws
        .iter()
        .filter_map(|d| match d {
            Draw::Cells { x, cells, .. } => Some((*x, cells.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        emitted_cells,
        vec![(0, vec![("中".into(), CellWidth::Wide)])],
        "differ must re-emit head at col 0 to repair corrupted continuation column at col 1"
    );
}

#[test]
fn streaming_token_grows_only_the_changed_run() {
    let (w, h) = (20, 2);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);

    let style = Style::default().fg(Color::Rgb(255, 255, 255));
    back.put(&mut damage, 0, 0, Fit::Clip, style, "Hello");
    let _ = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);

    // Grow the message by one word. Only the new cells should be dirty.
    let end = back.put(&mut damage, 5, 0, Fit::Clip, style, " world");
    assert_eq!(end, Pos::new(11, 0));

    let cmd = diff::diff(&back, &damage, &front);
    assert_eq!(cmd.draws.len(), 1);
    match &cmd.draws[0] {
        Draw::Cells { x, y, cells, .. } => {
            assert_eq!(*x, 5);
            assert_eq!(*y, 0);
            let text: String = cells.iter().map(|(s, _)| s.as_str()).collect();
            assert_eq!(text, " world");
        }
        other => panic!("expected Cells, got {other:?}"),
    }
}

#[test]
fn style_change_emits_sgr_only_for_the_delta() {
    let (w, h) = (10, 1);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let front = CellGrid::new(w, h);

    let s1 = Style::default().fg(Color::Rgb(100, 100, 100));
    let s2 = Style::default().fg(Color::Rgb(200, 200, 200));

    back.put(&mut damage, 0, 0, Fit::Clip, s1, "ab");
    back.put(&mut damage, 2, 0, Fit::Clip, s2, "cd");

    let cmd = diff::diff(&back, &damage, &front);
    assert_eq!(cmd.draws.len(), 2);
}

#[test]
fn clear_row_then_redraw_marks_only_that_row() {
    let (w, h) = (10, 3);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);

    for y in 0..h {
        back.put(&mut damage, 0, y, Fit::Clip, Style::default(), "aaaaa");
    }
    let _ = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);

    // Clear row 1 with a background, then write over it.
    back.clear_row(
        &mut damage,
        1,
        0,
        Style::default().bg(Color::Rgb(10, 10, 10)),
    );
    back.put(&mut damage, 0, 1, Fit::Clip, Style::default(), "bbbbb");

    let cmd = diff::diff(&back, &damage, &front);
    for draw in &cmd.draws {
        match draw {
            Draw::Cells { y, .. } => assert_eq!(*y, 1),
            Draw::ClearEol { y, .. } => assert_eq!(*y, 1),
            _ => panic!("unexpected draw on row other than 1"),
        }
    }
}

#[test]
fn resize_preserves_content_and_converges() {
    let (w, h) = (4, 2);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);
    back.put(&mut damage, 0, 0, Fit::Clip, Style::default(), "abcd");
    let _ = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);

    // Grow both grids. Content survives on back; exposed cells are blank.
    back.resize(&mut damage, 6, 3);
    let mut front_damage = Damage::new(3);
    front.resize(&mut front_damage, 6, 3);
    assert_eq!(back.get(0, 0).unwrap().symbol, "a");
    assert_eq!(back.get(3, 0).unwrap().symbol, "d");
    assert_eq!(back.get(5, 2).unwrap().symbol, " ");

    let cmd = diff::diff(&back, &damage, &front);
    assert!(cmd.is_empty(), "identical resized grids produce no draws");
    let _ = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);
    let second = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);
    assert!(
        second.is_empty(),
        "frame converges after resize, got {second:?}"
    );
}

#[test]
fn bce_clear_to_end_uses_clr_eol_only_for_default_bg() {
    let cmd = DrawCmd {
        w: 10,
        h: 1,
        draws: vec![Draw::ClearEol {
            x: 2,
            y: 0,
            style: Style::RESET,
            width: 8,
        }],
    };
    let mut buf = Vec::new();
    {
        let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[K"), "bce clear must use clr_eol: {s:?}");
}

#[test]
fn bce_clear_to_end_falls_back_to_spaces_without_bce() {
    let cmd = DrawCmd {
        w: 10,
        h: 1,
        draws: vec![Draw::ClearEol {
            x: 2,
            y: 0,
            style: Style::RESET,
            width: 8,
        }],
    };
    let mut buf = Vec::new();
    {
        let mut be = Backend::new(&mut buf).with_bce(Bce::No);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(
        !s.contains("\x1b[K"),
        "non-bce clear must not use clr_eol: {s:?}"
    );
    assert!(
        s.contains("        "),
        "non-bce clear must emit 8 spaces: {s:?}"
    );
}

#[test]
fn diff_collapses_uniform_blank_tail_to_clear_eol() {
    let mut back = CellGrid::new(8, 1);
    let mut damage = Damage::new(1);
    let mut front = CellGrid::new(8, 1);
    back.put(&mut damage, 0, 0, Fit::Clip, Style::default(), "abcdef");
    let _ = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);

    let bg = Style::default().bg(Color::Rgb(1, 2, 3));
    back.clear_row(&mut damage, 0, 2, bg);

    let cmd = diff::diff(&back, &damage, &front);
    assert_eq!(cmd.draws.len(), 1);
    match &cmd.draws[0] {
        Draw::ClearEol { x, y, style, width } => {
            assert_eq!(*x, 2);
            assert_eq!(*y, 0);
            assert_eq!(*style, bg);
            assert_eq!(*width, 6);
        }
        other => panic!("expected ClearEol, got {other:?}"),
    }
}

/// A minimal terminal emulator model that parses cursor movement, SGR styles,
/// and cell text to verify that rendered escapes match the back grid's content.
struct ModelTerminal {
    w: u16,
    h: u16,
    cur_x: u16,
    cur_y: u16,
    cells: Vec<String>,
}

impl ModelTerminal {
    fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            cur_x: 0,
            cur_y: 0,
            cells: vec![" ".to_string(); (w as usize) * (h as usize)],
        }
    }

    fn apply(&mut self, bytes: &str) {
        let mut chars = bytes.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut seq = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_alphabetic() || c == '?' {
                            seq.push(chars.next().unwrap());
                            if c.is_ascii_alphabetic() {
                                break;
                            }
                        } else {
                            seq.push(chars.next().unwrap());
                        }
                    }
                    self.apply_csi(&seq);
                }
            } else if !ch.is_control() {
                let g = ch.to_string();
                let width = graticule_canvas::grapheme_width(&g).max(1) as u16;
                if self.cur_x < self.w && self.cur_y < self.h {
                    let idx = self.cur_y as usize * self.w as usize + self.cur_x as usize;
                    self.cells[idx] = g;
                    for c in 1..width {
                        let fill_idx = idx + c as usize;
                        if fill_idx < self.cells.len() {
                            self.cells[fill_idx] = String::new();
                        }
                    }
                }
                self.cur_x = self.cur_x.saturating_add(width);
            }
        }
    }

    fn apply_csi(&mut self, seq: &str) {
        if let Some(stripped) = seq.strip_suffix('H') {
            let nums: Vec<u16> = stripped.split(';').filter_map(|s| s.parse().ok()).collect();
            let row = nums.first().copied().unwrap_or(1).saturating_sub(1);
            let col = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
            self.cur_y = row.min(self.h.saturating_sub(1));
            self.cur_x = col.min(self.w.saturating_sub(1));
        } else if seq == "K" || seq == "0K" {
            for x in self.cur_x..self.w {
                let idx = self.cur_y as usize * self.w as usize + x as usize;
                self.cells[idx] = " ".to_string();
            }
        }
    }

    fn row(&self, y: u16) -> Vec<String> {
        (0..self.w)
            .map(|x| self.cells[y as usize * self.w as usize + x as usize].clone())
            .collect()
    }
}

fn back_row(back: &CellGrid, y: u16) -> Vec<String> {
    (0..back.size().0)
        .map(|x| {
            let cell = back.get(x, y).unwrap();
            if cell.is_wide_continuation() {
                String::new()
            } else {
                cell.symbol.to_string()
            }
        })
        .collect()
}

fn assert_terminal_tracks_back(w: u16, h: u16, bce: Bce, frames: &[&str]) {
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);
    let mut term = ModelTerminal::new(w, h);

    for (n, text) in frames.iter().enumerate() {
        back.fill_rect(&mut damage, 0, 0, w, h, Style::default());
        back.put(&mut damage, 0, 0, Fit::Clip, Style::default(), text);

        let bytes = render_cycle(&mut back, &mut damage, &mut front, bce);
        term.apply(&bytes);

        for y in 0..h {
            assert_eq!(
                term.row(y),
                back_row(&back, y),
                "frame {n} ({text:?}): terminal row {y} diverged from back grid",
            );
        }
    }
}

#[test]
fn terminal_tracks_back_across_wide_narrow_transitions() {
    let (w, h) = (8, 2);
    let sequence = [
        "abcd",
        "ab😀d",
        "a😀cd",
        "😀bcd",
        "abcd",
        "中文测试",
        "a文b试",
        "    ",
        "😀😀😀",
    ];
    for bce in [Bce::Yes, Bce::No] {
        assert_terminal_tracks_back(w, h, bce, &sequence);
    }
}

#[test]
fn bottom_right_cell_renders_under_line_wrap_guard() {
    let (w, h) = (4, 2);
    for bce in [Bce::Yes, Bce::No] {
        let mut back = CellGrid::new(w, h);
        let mut damage = Damage::new(h);
        let mut front = CellGrid::new(w, h);
        let mut term = ModelTerminal::new(w, h);

        back.fill_rect(&mut damage, 0, 0, w, h, Style::default());
        back.put(&mut damage, 0, 1, Fit::Clip, Style::default(), "wxyz");
        let bytes = render_cycle(&mut back, &mut damage, &mut front, bce);
        term.apply(&bytes);

        assert_eq!(term.row(0), back_row(&back, 0), "row 0 (bce={bce:?})");
        assert_eq!(term.row(1), back_row(&back, 1), "row 1 (bce={bce:?})");
        assert!(
            bytes.contains("\x1b[?7l") && bytes.contains("\x1b[?7h"),
            "bottom-right write must be protected by DECAWM off/on: {bytes:?}"
        );
    }
}

#[test]
fn cell_constructors() {
    let blank = Cell::blank();
    assert_eq!(blank.symbol, " ");
    assert_eq!(blank.width, CellWidth::Narrow);

    let wide = Cell::wide_continuation(Style::default().bg(Color::Rgb(1, 2, 3)));
    assert!(wide.is_wide_continuation());
    assert_eq!(wide.style.bg, Color::Rgb(1, 2, 3));
}

#[test]
fn invalidate_emits_real_sgr_reset() {
    let mut sink = Vec::new();
    let mut be = Backend::new(&mut sink).with_bce(Bce::Yes);
    be.apply_style(Style::default().fg(Color::Red)).unwrap();
    be.render(&DrawCmd {
        w: 10,
        h: 1,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: Style::default().fg(Color::Red),
            cells: vec![("X".into(), CellWidth::Narrow)],
        }],
    })
    .unwrap();

    be.invalidate().unwrap();
    let emitted = String::from_utf8(be.writer().clone()).unwrap();
    assert!(
        emitted.contains("\x1b[0m"),
        "invalidate must emit SGR reset: {emitted:?}"
    );
}

#[test]
fn resize_then_repaint_does_not_inherit_stale_bold() {
    let mut sink = Vec::new();
    let mut be = Backend::new(&mut sink).with_bce(Bce::Yes);
    let bold = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    be.render(&DrawCmd {
        w: 10,
        h: 1,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: bold,
            cells: vec![("X".into(), CellWidth::Narrow)],
        }],
    })
    .unwrap();

    be.invalidate().unwrap();
    be.writer().clear();

    let plain = Style::default().fg(Color::Red);
    be.render(&DrawCmd {
        w: 10,
        h: 1,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: plain,
            cells: vec![("Y".into(), CellWidth::Narrow)],
        }],
    })
    .unwrap();
    let emitted = String::from_utf8(be.writer().clone()).unwrap();
    assert!(
        !emitted.contains("\x1b[1m"),
        "bold must not leak past invalidate: {emitted:?}"
    );
}

fn labeled(w: u16, labels: &[&str]) -> (CellGrid, Damage) {
    let mut g = CellGrid::new(w, labels.len() as u16);
    let mut d = Damage::new(labels.len() as u16);
    for (y, label) in labels.iter().enumerate() {
        g.put(&mut d, 0, y as u16, Fit::Clip, Style::default(), label);
    }
    d.clear();
    (g, d)
}

#[test]
fn streaming_append_scrolls_history_instead_of_repainting_it() {
    let (w, h) = (10, 4);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    for (y, text) in [(0u16, "h1"), (1, "h2"), (2, "h3")] {
        back.put(&mut damage, 0, y, Fit::Clip, Style::default(), text);
    }
    let mut front = CellGrid::new(w, h);
    let first = render_cycle(&mut back, &mut damage, &mut front, Bce::Yes);
    assert!(first.contains("h1") && first.contains("h3"));

    let (mut next, mut next_damage) = labeled(w, &["h2", "h3", "NEW", ""]);
    for y in 0..=2 {
        next_damage.mark(0, y);
    }
    let cmd = diff::diff(&next, &next_damage, &front);

    match cmd.scroll() {
        Some(diff::Draw::ScrollUp { y, height, amount }) => {
            assert_eq!((*y, *height, *amount), (0, 3, 1), "history scrolls up by 1");
        }
        other => panic!("expected ScrollUp, got {other:?} — draws: {:?}", cmd.draws),
    }

    let mut buf = Vec::new();
    {
        let mut be = Backend::new(&mut buf).with_bce(Bce::Yes);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(
        s.contains("\x1b[1;3r"),
        "scroll region covers the moved band: {s:?}"
    );
    assert!(s.contains("\x1b[1S"), "scroll-up by 1: {s:?}");
    assert!(!s.contains("h1"), "history must not be repainted: {s:?}");

    diff::promote_scrolled(&next, &mut next_damage, &mut front, &cmd);
    let idle = render_cycle(&mut next, &mut next_damage, &mut front, Bce::Yes);
    assert!(
        idle.is_empty(),
        "post-scroll frame must be idle, got {idle:?}"
    );
}

#[test]
fn combined_sgr_driver_reproduces_exact_cell_styles_across_frames() {
    // Minimal SGR + CUP interpreter verifying that our zero-alloc combined SGR
    // encoder preserves exact fg, bg, and modifier bits across style transitions
    // including attribute drops (`\x1b[0;...m`).
    struct StyledTerminal {
        w: u16,
        h: u16,
        cur_x: u16,
        cur_y: u16,
        active: Style,
        grid: Vec<(String, Style)>,
    }

    impl StyledTerminal {
        fn new(w: u16, h: u16) -> Self {
            Self {
                w,
                h,
                cur_x: 0,
                cur_y: 0,
                active: Style::RESET,
                grid: vec![(" ".to_string(), Style::RESET); (w as usize) * (h as usize)],
            }
        }

        fn apply(&mut self, wire: &str) {
            let mut chars = wire.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\x1b' && chars.peek() == Some(&'[') {
                    chars.next();
                    let mut seq = String::new();
                    for c in chars.by_ref() {
                        seq.push(c);
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                    self.apply_csi(&seq);
                } else if !ch.is_control() {
                    if self.cur_x < self.w && self.cur_y < self.h {
                        let idx = self.cur_y as usize * self.w as usize + self.cur_x as usize;
                        self.grid[idx] = (ch.to_string(), self.active);
                    }
                    self.cur_x = self.cur_x.saturating_add(1);
                }
            }
        }

        fn apply_csi(&mut self, seq: &str) {
            if let Some(body) = seq.strip_suffix('H') {
                let mut parts = body.split(';');
                let row = parts
                    .next()
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                let col = parts
                    .next()
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                self.cur_y = row.min(self.h.saturating_sub(1));
                self.cur_x = col.min(self.w.saturating_sub(1));
            } else if let Some(body) = seq.strip_suffix('m') {
                let nums: Vec<u8> = if body.is_empty() {
                    vec![0]
                } else {
                    body.split(';').filter_map(|p| p.parse().ok()).collect()
                };
                let mut i = 0;
                while i < nums.len() {
                    match nums[i] {
                        0 => self.active = Style::RESET,
                        1 => self.active.add.insert(Modifier::BOLD),
                        2 => self.active.add.insert(Modifier::DIM),
                        3 => self.active.add.insert(Modifier::ITALIC),
                        4 => self.active.add.insert(Modifier::UNDERLINE),
                        7 => self.active.add.insert(Modifier::REVERSE),
                        9 => self.active.add.insert(Modifier::STRIKETHROUGH),
                        31 => self.active.fg = Color::Red,
                        32 => self.active.fg = Color::Green,
                        39 => self.active.fg = Color::Reset,
                        49 => self.active.bg = Color::Reset,
                        38 if nums.get(i + 1) == Some(&2) && i + 4 < nums.len() => {
                            self.active.fg = Color::Rgb(nums[i + 2], nums[i + 3], nums[i + 4]);
                            i += 4;
                        }
                        48 if nums.get(i + 1) == Some(&5) && i + 2 < nums.len() => {
                            self.active.bg = Color::Indexed(nums[i + 2]);
                            i += 2;
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
        }
    }

    let (w, h) = (12, 2);
    let mut back = CellGrid::new(w, h);
    let mut damage = Damage::new(h);
    let mut front = CellGrid::new(w, h);
    let mut model = StyledTerminal::new(w, h);

    let s_bold_rgb = Style::default()
        .fg(Color::Rgb(12, 34, 56))
        .bg(Color::Indexed(236))
        .add_modifier(Modifier::BOLD | Modifier::ITALIC);
    let s_drop_italic = Style::default()
        .fg(Color::Rgb(12, 34, 56))
        .bg(Color::Indexed(236))
        .add_modifier(Modifier::BOLD);
    let s_ansi_green = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::UNDERLINE);

    back.put(&mut damage, 0, 0, Fit::Clip, s_bold_rgb, "AAA");
    back.put(&mut damage, 3, 0, Fit::Clip, s_drop_italic, "BBB");
    back.put(&mut damage, 6, 0, Fit::Clip, s_ansi_green, "CCC");

    let mut be = Backend::new(Vec::new())
        .with_bce(Bce::No)
        .with_profile(TerminalProfile::direct_color());

    let cmd1 = diff::diff(&back, &damage, &front);
    be.render(&cmd1).unwrap();
    diff::promote(&back, &mut damage, &mut front);
    let frame1 = String::from_utf8(std::mem::take(be.writer())).unwrap();
    model.apply(&frame1);

    // Frame 2: overwrite middle run with plain Reset style on the SAME backend
    // so multi-frame SGR state tracking (dropping UNDERLINE + changing fg) is exercised.
    back.put(&mut damage, 3, 0, Fit::Clip, Style::RESET, "ddd");
    let cmd2 = diff::diff(&back, &damage, &front);
    be.render(&cmd2).unwrap();
    diff::promote(&back, &mut damage, &mut front);
    let frame2 = String::from_utf8(std::mem::take(be.writer())).unwrap();
    model.apply(&frame2);

    for x in 0..9u16 {
        let cell = back.get(x, 0).unwrap();
        let (sym, style) = &model.grid[x as usize];
        assert_eq!(sym.as_str(), cell.symbol.as_str(), "col {x} symbol");
        assert_eq!(*style, cell.style, "col {x} style");
    }
}
