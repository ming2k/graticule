//! High-level API tests: Frame, Layout, Paragraph/Block/Clear widgets render
//! correctly through `TestTerminal` and the frame loop converges.

use graticule_canvas::render::diff;
use graticule_canvas::{
    Alignment, Backend, Bce, Block, BorderType, Borders, CellGrid, Clear, Color, Constraint,
    Damage, Direction, Fit, Layout, Line, Margin, Paragraph, Rect, Span, Style, Terminal,
    TerminalProfile, TestTerminal, Wrap,
};

#[test]
fn layout_split_vertical_three_rows() {
    let area = Rect::new(0, 0, 10, 30);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].height, 5);
    assert_eq!(chunks[0].y, 0);
    assert_eq!(chunks[1].height, 20);
    assert_eq!(chunks[2].height, 5);
    assert_eq!(chunks[2].y, 25);
}

#[test]
fn paragraph_writes_styled_text_into_grid() {
    let mut term = TestTerminal::new(20, 2);
    let style = Style::default().fg(Color::Rgb(255, 0, 0));
    let mut para = Paragraph::new(Line::from(Span::styled("hello", style)));
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 20, 1));
    });
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, "h");
    assert_eq!(term.buffer().get(4, 0).unwrap().symbol, "o");
    assert_eq!(
        term.buffer().get(0, 0).unwrap().style.fg,
        Color::Rgb(255, 0, 0)
    );
}

#[test]
fn paragraph_text_without_bg_preserves_existing_surface() {
    let mut term = TestTerminal::new(20, 1);
    let bg = Color::Rgb(7, 8, 9);
    term.draw(|frame| {
        frame.paint(Rect::new(0, 0, 20, 1), |p| {
            p.fill(Rect::new(0, 0, 20, 1), Style::default().bg(bg))
        });
    });
    let mut para = Paragraph::new(Line::from(Span::styled(
        "hello",
        Style::default().fg(Color::Rgb(255, 0, 0)),
    )));
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 20, 1));
    });
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, "h");
    assert_eq!(term.buffer().get(0, 0).unwrap().style.bg, bg);
    assert_eq!(term.buffer().get(4, 0).unwrap().style.bg, bg);
}

#[test]
fn paragraph_wraps_long_text() {
    let mut term = TestTerminal::new(5, 4);
    let mut para = Paragraph::new("hello world").wrap(Wrap { trim: false });
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 5, 4));
    });
    // "hello" on row 0, " worl" on row 1, "d" on row 2.
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, "h");
    assert_eq!(term.buffer().get(0, 1).unwrap().symbol, " ");
    assert_eq!(term.buffer().get(1, 1).unwrap().symbol, "w");
    assert_eq!(term.buffer().get(0, 2).unwrap().symbol, "d");
}

#[test]
fn paragraph_with_block_fills_bg_and_draws_left_bar() {
    let mut term = TestTerminal::new(10, 3);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Rgb(1, 2, 3)))
        .style(Style::default().bg(Color::Rgb(10, 10, 10)));
    let mut para = Paragraph::new(Line::raw("hi")).block(block);
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 10, 3));
    });
    // Left border bar at col 0.
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, "┃");
    // Background fill behind the bar.
    assert_eq!(
        term.buffer().get(0, 0).unwrap().style.bg,
        Color::Rgb(10, 10, 10)
    );
    // Text starts at col 1.
    assert_eq!(term.buffer().get(1, 0).unwrap().symbol, "h");
}

#[test]
fn paragraph_center_alignment() {
    let mut term = TestTerminal::new(10, 1);
    let mut para = Paragraph::new(Line::raw("hi")).alignment(Alignment::Center);
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 10, 1));
    });
    // "hi" is 2 wide, centered in 10 → starts at col 4.
    assert_eq!(term.buffer().get(4, 0).unwrap().symbol, "h");
    assert_eq!(term.buffer().get(5, 0).unwrap().symbol, "i");
    assert_eq!(term.buffer().get(3, 0).unwrap().symbol, " ");
}

#[test]
fn clear_resets_area_to_blank() {
    let mut term = TestTerminal::new(5, 2);
    term.draw(|frame| {
        frame.put(0, 0, Style::default(), "abcde");
    });
    term.draw(|frame| {
        frame.render_widget(&mut Clear::new(), Rect::new(0, 0, 3, 1));
    });
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, " ");
    assert_eq!(term.buffer().get(3, 0).unwrap().symbol, "d");
}

#[test]
fn rect_inner_margin() {
    let r = Rect::new(0, 0, 10, 10);
    let inner = r.inner(Margin::new(2, 1));
    assert_eq!(inner, Rect::new(2, 1, 6, 8));
}

#[test]
fn paragraph_scroll_skips_top_rows() {
    let mut term = TestTerminal::new(5, 2);
    let mut para = Paragraph::new(vec![
        Line::raw("row0"),
        Line::raw("row1"),
        Line::raw("row2"),
    ])
    .scroll(1, 0);
    term.draw(|frame| {
        frame.render_widget(&mut para, Rect::new(0, 0, 5, 2));
    });
    // Row 0 scrolled away; row1 is visible.
    assert_eq!(term.buffer().get(0, 0).unwrap().symbol, "r");
    assert_eq!(term.buffer().get(0, 1).unwrap().symbol, "r");
}

#[test]
fn frame_loop_converges() {
    // Paint, then assert content reached stdout (backend owns the borrow for
    // the terminal's lifetime, so read after it drops).
    let mut buf = Vec::new();
    {
        let backend = Backend::new(&mut buf)
            .with_bce(Bce::Yes)
            .with_profile(TerminalProfile::direct_color());
        let mut term = Terminal::new(backend);
        term.resize_to(10, 2);
        term.draw(|f| {
            f.render_widget(&mut Paragraph::new(Line::raw("hi")), Rect::new(0, 0, 10, 1));
        })
        .unwrap();
    }
    assert!(String::from_utf8_lossy(&buf).contains("hi"));

    // Convergence is a property of the diff: after a promote, a second diff
    // against the stable front grid is empty.
    let mut back = CellGrid::new(10, 2);
    let mut damage = Damage::new(2);
    let mut front = CellGrid::new(10, 2);
    back.put(&mut damage, 0, 0, Fit::Clip, Style::default(), "hi");
    let first = diff::diff(&back, &damage, &front);
    assert!(!first.draws.is_empty());
    diff::promote(&back, &mut damage, &mut front);
    let second = diff::diff(&back, &damage, &front);
    assert!(second.draws.is_empty(), "idle frame emits nothing");
}

#[test]
fn resize_to_clears_screen_and_homes_cursor_on_commit() {
    let mut buf = Vec::new();
    {
        let backend = Backend::new(&mut buf)
            .with_bce(Bce::Yes)
            .with_profile(TerminalProfile::direct_color());
        let mut term = Terminal::new(backend);
        term.resize_to(20, 5);
        term.draw(|f| {
            f.render_widget(
                &mut Paragraph::new(Line::raw("hello")),
                Rect::new(0, 0, 10, 1),
            );
        })
        .unwrap();

        // Now resize again
        term.resize_to(30, 8);
        term.draw(|f| {
            f.render_widget(
                &mut Paragraph::new(Line::raw("resized")),
                Rect::new(0, 0, 10, 1),
            );
        })
        .unwrap();
    }
    let output = String::from_utf8_lossy(&buf);
    assert!(
        output.contains("\x1b[2J"),
        "resize commit must clear screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H") || output.contains("\x1b[H"),
        "resize commit must home cursor before clear: {output:?}"
    );
}

#[test]
fn declarative_column_and_row_composition_in_terminal() {
    use graticule_canvas::widgets::{Column, Divider, Padding, Row, WidgetExt};

    let mut buf = Vec::new();
    {
        let backend = Backend::new(&mut buf)
            .with_bce(Bce::Yes)
            .with_profile(TerminalProfile::direct_color());
        let mut term = Terminal::new(backend);
        term.resize_to(30, 10);

        // Build declarative UI tree
        let mut ui = Column::new()
            .child_with_constraint(Constraint::Length(1), Paragraph::new("Header Title"))
            .child_with_constraint(Constraint::Length(1), Divider::horizontal())
            .child_with_constraint(
                Constraint::Min(0),
                Row::new()
                    .child_with_constraint(Constraint::Length(10), Paragraph::new("Left Pane"))
                    .child_with_constraint(Constraint::Length(1), Divider::vertical())
                    .child_with_constraint(
                        Constraint::Min(0),
                        Paragraph::new("Content")
                            .with_padding(Padding::all(1))
                            .with_background(Style::default().bg(Color::Blue)),
                    ),
            );

        term.draw(|f| {
            f.render_widget(&mut ui, f.area());
        })
        .unwrap();
    }

    let output = String::from_utf8_lossy(&buf);
    assert!(output.contains("Header"));
    assert!(output.contains("Title"));
    assert!(output.contains("Left"));
    assert!(output.contains("Pane"));
    assert!(output.contains("Content"));
    assert!(output.contains("─"));
    assert!(output.contains("│"));
}
