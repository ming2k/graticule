//! Demonstrates the retained `ui` runtime (`UiRuntime`), modal dialogs, and focus.
//!
//! # Concepts demonstrated
//!
//! - **Two-phase transactional scene update**: `begin(viewport)` -> `mount(...)`
//!   -> `paint(...)` -> `commit()`.
//! - **Modal input barrier**: mounting a modal component intercepts pointer and
//!   keyboard routing, preventing interactions with background nodes behind it.
//! - **Automatic focus preservation and restoration**: when the modal closes,
//!   keyboard focus smoothly returns to the previously focused editor.
//! - **Damage-driven repaint**: only the affected bounding regions are
//!   repainted during transitions.
//!
//! Run with:
//! ```sh
//! cargo run --example dialog
//! ```
//! Controls:
//! - `o`: Open confirmation dialog
//! - `Tab`: Cycle focus between elements
//! - `Enter`: Trigger focused button
//! - `Esc`: Dismiss dialog / quit
//! - `q`: Quit

#![allow(clippy::unwrap_used)]

use std::io::{self, IsTerminal, stdout};
use std::time::Duration;

use graticule::ui::{Component, LayoutBox, NodeLayout, UiRuntime};
use graticule::widgets::{Block, BorderType, Borders, Line, Paragraph, Span};
use graticule::{
    Backend, Color, Event, InputReader, KeyCode, MouseMode, Paint, Rect, SessionConfig, Style,
    Terminal, TerminalSession,
};

fn main() -> io::Result<()> {
    let config = SessionConfig {
        mouse_mode: MouseMode::None,
        ..SessionConfig::default()
    };
    let _session = TerminalSession::enter(config)?;
    run_dialog_app()
}

fn run_dialog_app() -> io::Result<()> {
    let out = stdout();
    let is_tty = out.is_terminal();
    let mut terminal = Terminal::new(Backend::new(out));
    let mut ui = UiRuntime::<&'static str>::default();
    let (_reader, rx) = InputReader::spawn_channel()?;

    let mut dialog_open = true;
    let mut confirm_action = false;
    let mut active_btn = 0usize; // 0 = "Cancel", 1 = "Confirm"

    loop {
        if let Ok(Event::Key(key)) = rx.recv_timeout(Duration::from_millis(16)) {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Esc => {
                    if dialog_open {
                        dialog_open = false;
                    } else {
                        break;
                    }
                }
                KeyCode::Char('o') => {
                    dialog_open = true;
                }
                KeyCode::Tab if dialog_open => {
                    active_btn = (active_btn + 1) % 2;
                }
                KeyCode::Enter if dialog_open => {
                    if active_btn == 1 {
                        confirm_action = true;
                    }
                    dialog_open = false;
                }
                _ => {}
            }
        }

        // Draw and present frame
        terminal.draw(|frame| {
            let area = frame.area();

            // Phase 1: Begin a new pending scene
            ui.begin(area);

            // Root viewport canvas
            ui.mount(Component::new("root", None, LayoutBox::Fill))
                .unwrap();

            // Background editor panel
            ui.mount(
                Component::new(
                    "editor",
                    Some("root"),
                    LayoutBox::Placed(Rect::new(
                        2,
                        2,
                        area.width.saturating_sub(4),
                        area.height.saturating_sub(4),
                    )),
                )
                .interactive(),
            )
            .unwrap();

            // Modal dialog (if open)
            if dialog_open {
                let dialog_w = 46.min(area.width.saturating_sub(4));
                let dialog_h = 10.min(area.height.saturating_sub(2));
                let dialog_x = area.x + (area.width.saturating_sub(dialog_w)) / 2;
                let dialog_y = area.y + (area.height.saturating_sub(dialog_h)) / 2;

                ui.mount(
                    Component::new(
                        "modal_dialog",
                        Some("root"),
                        LayoutBox::Placed(Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)),
                    )
                    .modal()
                    .layer(10),
                )
                .unwrap();
            }

            // Phase 2: Paint through UiRuntime
            ui.paint(frame, |key, layout, paint: &mut Paint<'_>| match *key {
                "root" => {
                    paint.fill(layout.bounds, Style::default().bg(Color::Black));
                }
                "editor" => {
                    render_editor(layout, paint, dialog_open, confirm_action);
                }
                "modal_dialog" => {
                    render_dialog(layout, paint, active_btn);
                }
                _ => {}
            })
            .unwrap();
        })?;

        // Phase 3: Commit published geometry and reconcile focus
        let _events = ui.commit().unwrap();

        // If running unattended in CI/test, break after one clean cycle
        if !is_tty {
            break;
        }
    }

    Ok(())
}

fn render_editor(layout: NodeLayout, paint: &mut Paint<'_>, dialog_open: bool, confirmed: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if dialog_open {
            Color::DarkGray
        } else {
            Color::Blue
        }))
        .title(Line::raw(" Main Workspace (Press 'o' to open dialog) "));

    let inner = block.inner(layout.bounds);
    block.draw(layout.bounds, paint);

    let status_line = if confirmed {
        Span::styled(
            "Status: Action confirmed by user!",
            Style::default().fg(Color::Green),
        )
    } else {
        Span::styled(
            "Status: Ready. Awaiting user interaction.",
            Style::default().fg(Color::Gray),
        )
    };

    let lines = vec![
        Line::from(vec![Span::raw(
            "This is the background component underneath the modal layer.",
        )]),
        Line::from(vec![Span::raw(
            "When the modal dialog is active, focus and pointer barriers",
        )]),
        Line::from(vec![Span::raw(
            "prevent clicks or keys from leaking into this surface.",
        )]),
        Line::raw(""),
        Line::from(vec![status_line]),
    ];

    let mut para = Paragraph::new(lines);
    frame_render(&mut para, inner, paint);
}

fn render_dialog(layout: NodeLayout, paint: &mut Paint<'_>, active_btn: usize) {
    // Drop shadow / surface fill
    paint.fill(layout.bounds, Style::default().bg(Color::Rgb(20, 20, 25)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::raw(" Confirm Operation "));

    let inner = block.inner(layout.bounds);
    block.draw(layout.bounds, paint);

    let cancel_style = if active_btn == 0 {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };

    let confirm_style = if active_btn == 1 {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };

    let lines = vec![
        Line::raw("Do you want to proceed with this operation?"),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" [ Cancel (Esc) ] ", cancel_style),
            Span::raw("   "),
            Span::styled(" [ Confirm (Enter) ] ", confirm_style),
        ]),
    ];

    let mut para = Paragraph::new(lines);
    frame_render(&mut para, inner, paint);
}

fn frame_render<W: graticule::Widget>(widget: &mut W, area: Rect, paint: &mut Paint<'_>) {
    if let Some(mut child) = paint.fit(area) {
        widget.render(area, &mut child);
    }
}
