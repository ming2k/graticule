//! Demonstrates the streaming-transcript scroll optimization.
//!
//! A streaming transcript (build logs, chat messages, execution traces) grows
//! at the bottom while following the tail. In an immediate-mode library, every
//! frame repaints the entire screen. In `graticule`, `diff` *proves* that history
//! shifted up by one row, issues a single terminal scroll (`\x1b[1S`), and
//! repaints only the new row at the bottom — costing ~50 bytes instead of a full
//! screen repaint.
//!
//! Notice how [`TerminalSession`] and [`InputReader`] handle raw mode,
//! alternate screen, and input natively with zero third-party terminal wrapper
//! crates.
//!
//! Run with:
//! ```sh
//! cargo run --example streaming
//! ```
//! Press `q` or `Esc` to exit early.

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use graticule::widgets::{Block, BorderType, Borders, Line, Paragraph, Span};
use graticule::{
    Backend, Color, Event, InputReader, KeyCode, KeyModifiers, MouseMode, SessionConfig, Style,
    TerminalSession, live_terminal,
};

fn main() -> io::Result<()> {
    let config = SessionConfig {
        mouse_mode: MouseMode::None,
        ..SessionConfig::default()
    };
    let _session = TerminalSession::enter(config)?;
    run_streaming()
}

fn run_streaming() -> io::Result<()> {
    let mut terminal = live_terminal(Backend::new(stdout()));
    let (_reader, rx) = InputReader::spawn_channel()?;

    let mut messages: Vec<String> = Vec::new();
    let mut counter = 0usize;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(80);

    // Initial message batch
    for i in 1..=5 {
        messages.push(format!("System initialized. Component {i} ready."));
    }

    loop {
        if let Ok(Event::Key(key)) = rx.recv_timeout(Duration::from_millis(10)) {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break;
                }
                _ => {}
            }
        }

        // Stream a new row periodically
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
            counter += 1;
            let msg = match counter % 4 {
                0 => format!("[info]  Step {counter:>3}: validating AST invariants"),
                1 => format!(
                    "[debug] Step {counter:>3}: cache hit for partition {}",
                    counter % 7
                ),
                2 => format!("[trace] Step {counter:>3}: dispatching frame token"),
                _ => format!("[info]  Step {counter:>3}: stream chunk committed"),
            };
            messages.push(msg);

            // Cap in-memory history to the last 200 items
            if messages.len() > 200 {
                messages.remove(0);
            }
        }

        // Render frame
        terminal.draw(|frame| {
            let area = frame.area();
            let mut block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Line::raw(
                    " graticule streaming transcript (press 'q' to quit) ",
                ));

            let inner = block.inner(area);
            frame.render_widget(&mut block, area);

            // Show as many lines as fit inside the panel, following the bottom
            let visible_count = inner.height as usize;
            let start = messages.len().saturating_sub(visible_count);
            let visible_lines: Vec<Line<'_>> = messages[start..]
                .iter()
                .map(|s| {
                    let style = if s.contains("[info]") {
                        Style::default().fg(Color::Green)
                    } else if s.contains("[debug]") {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(vec![Span::styled(s.as_str(), style)])
                })
                .collect();

            let mut para = Paragraph::new(visible_lines);
            frame.render_widget(&mut para, inner);
        })?;

        // Stop automatically after 80 steps if running unattended
        if counter >= 80 {
            break;
        }
    }

    Ok(())
}
