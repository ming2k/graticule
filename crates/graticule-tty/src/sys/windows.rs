//! Windows Console input adapter backed privately by `crossterm`.
//!
//! Windows delivers structured `INPUT_RECORD`s (`ReadConsoleInputW`) rather than
//! a split-prone VT byte stream, so wrapping crossterm internally gives Day-1
//! Windows parity without exposing any `crossterm` types in `graticule-tty`'s
//! public API.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    self as ct, Event as CtEvent, KeyCode as CtKeyCode, KeyEvent as CtKeyEvent,
    KeyEventKind as CtKeyEventKind, KeyEventState as CtKeyEventState,
    KeyModifiers as CtKeyModifiers, MediaKeyCode as CtMediaKeyCode,
    ModifierKeyCode as CtModifierKeyCode, MouseButton as CtMouseButton, MouseEvent as CtMouseEvent,
    MouseEventKind as CtMouseEventKind,
};

use crate::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};

pub(crate) fn terminal_size() -> io::Result<(u16, u16)> {
    crossterm::terminal::size()
}

pub(crate) fn run_event_loop(
    sink: &mut dyn FnMut(Event) -> bool,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    while !shutdown.load(Ordering::Acquire) {
        match ct::poll(Duration::from_millis(200)) {
            Ok(true) => match ct::read() {
                Ok(ev) => {
                    if !sink(map_event(ev)) {
                        break;
                    }
                }
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

pub(crate) fn probe_keyboard_enhancement_sync() -> bool {
    crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
}

fn map_event(ev: CtEvent) -> Event {
    match ev {
        CtEvent::Key(k) => Event::Key(map_key_event(k)),
        CtEvent::Mouse(m) => Event::Mouse(map_mouse_event(m)),
        CtEvent::Resize(cols, rows) => Event::Resize(cols, rows),
        CtEvent::Paste(text) => Event::Paste(text),
        CtEvent::FocusGained => Event::FocusGained,
        CtEvent::FocusLost => Event::FocusLost,
    }
}

fn map_key_event(k: CtKeyEvent) -> KeyEvent {
    KeyEvent::new_with_state(
        map_key_code(k.code),
        map_modifiers(k.modifiers),
        match k.kind {
            CtKeyEventKind::Press => KeyEventKind::Press,
            CtKeyEventKind::Repeat => KeyEventKind::Repeat,
            CtKeyEventKind::Release => KeyEventKind::Release,
        },
        map_key_state(k.state),
    )
}

fn map_modifiers(m: CtKeyModifiers) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if m.contains(CtKeyModifiers::SHIFT) {
        out |= KeyModifiers::SHIFT;
    }
    if m.contains(CtKeyModifiers::CONTROL) {
        out |= KeyModifiers::CONTROL;
    }
    if m.contains(CtKeyModifiers::ALT) {
        out |= KeyModifiers::ALT;
    }
    if m.contains(CtKeyModifiers::SUPER) {
        out |= KeyModifiers::SUPER;
    }
    if m.contains(CtKeyModifiers::HYPER) {
        out |= KeyModifiers::HYPER;
    }
    if m.contains(CtKeyModifiers::META) {
        out |= KeyModifiers::META;
    }
    out
}

fn map_key_state(s: CtKeyEventState) -> KeyEventState {
    let mut out = KeyEventState::NONE;
    if s.contains(CtKeyEventState::KEYPAD) {
        out |= KeyEventState::KEYPAD;
    }
    if s.contains(CtKeyEventState::CAPS_LOCK) {
        out |= KeyEventState::CAPS_LOCK;
    }
    if s.contains(CtKeyEventState::NUM_LOCK) {
        out |= KeyEventState::NUM_LOCK;
    }
    out
}

fn map_key_code(code: CtKeyCode) -> KeyCode {
    match code {
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => KeyCode::BackTab,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::Insert => KeyCode::Insert,
        CtKeyCode::F(n) => KeyCode::F(n),
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Null => KeyCode::Null,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::CapsLock => KeyCode::CapsLock,
        CtKeyCode::ScrollLock => KeyCode::ScrollLock,
        CtKeyCode::NumLock => KeyCode::NumLock,
        CtKeyCode::PrintScreen => KeyCode::PrintScreen,
        CtKeyCode::Pause => KeyCode::Pause,
        CtKeyCode::Menu => KeyCode::Menu,
        CtKeyCode::KeypadBegin => KeyCode::KeypadBegin,
        CtKeyCode::Media(m) => KeyCode::Media(match m {
            CtMediaKeyCode::Play => MediaKeyCode::Play,
            CtMediaKeyCode::Pause => MediaKeyCode::Pause,
            CtMediaKeyCode::PlayPause => MediaKeyCode::PlayPause,
            CtMediaKeyCode::Reverse => MediaKeyCode::Reverse,
            CtMediaKeyCode::Stop => MediaKeyCode::Stop,
            CtMediaKeyCode::FastForward => MediaKeyCode::FastForward,
            CtMediaKeyCode::Rewind => MediaKeyCode::Rewind,
            CtMediaKeyCode::TrackNext => MediaKeyCode::TrackNext,
            CtMediaKeyCode::TrackPrevious => MediaKeyCode::TrackPrevious,
            CtMediaKeyCode::Record => MediaKeyCode::Record,
            CtMediaKeyCode::LowerVolume => MediaKeyCode::LowerVolume,
            CtMediaKeyCode::RaiseVolume => MediaKeyCode::RaiseVolume,
            CtMediaKeyCode::MuteVolume => MediaKeyCode::MuteVolume,
        }),
        CtKeyCode::Modifier(m) => KeyCode::Modifier(match m {
            CtModifierKeyCode::LeftShift => ModifierKeyCode::LeftShift,
            CtModifierKeyCode::LeftControl => ModifierKeyCode::LeftControl,
            CtModifierKeyCode::LeftAlt => ModifierKeyCode::LeftAlt,
            CtModifierKeyCode::LeftSuper => ModifierKeyCode::LeftSuper,
            CtModifierKeyCode::LeftHyper => ModifierKeyCode::LeftHyper,
            CtModifierKeyCode::LeftMeta => ModifierKeyCode::LeftMeta,
            CtModifierKeyCode::RightShift => ModifierKeyCode::RightShift,
            CtModifierKeyCode::RightControl => ModifierKeyCode::RightControl,
            CtModifierKeyCode::RightAlt => ModifierKeyCode::RightAlt,
            CtModifierKeyCode::RightSuper => ModifierKeyCode::RightSuper,
            CtModifierKeyCode::RightHyper => ModifierKeyCode::RightHyper,
            CtModifierKeyCode::RightMeta => ModifierKeyCode::RightMeta,
            CtModifierKeyCode::IsoLevel3Shift => ModifierKeyCode::IsoLevel3Shift,
            CtModifierKeyCode::IsoLevel5Shift => ModifierKeyCode::IsoLevel5Shift,
        }),
    }
}

fn map_mouse_event(m: CtMouseEvent) -> MouseEvent {
    MouseEvent {
        kind: match m.kind {
            CtMouseEventKind::Down(b) => MouseEventKind::Down(map_button(b)),
            CtMouseEventKind::Up(b) => MouseEventKind::Up(map_button(b)),
            CtMouseEventKind::Drag(b) => MouseEventKind::Drag(map_button(b)),
            CtMouseEventKind::Moved => MouseEventKind::Moved,
            CtMouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
            CtMouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
            CtMouseEventKind::ScrollLeft => MouseEventKind::ScrollLeft,
            CtMouseEventKind::ScrollRight => MouseEventKind::ScrollRight,
        },
        column: m.column,
        row: m.row,
        modifiers: map_modifiers(m.modifiers),
    }
}

fn map_button(b: CtMouseButton) -> MouseButton {
    match b {
        CtMouseButton::Left => MouseButton::Left,
        CtMouseButton::Right => MouseButton::Right,
        CtMouseButton::Middle => MouseButton::Middle,
    }
}
