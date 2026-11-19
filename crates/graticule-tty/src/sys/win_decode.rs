//! Pure cross-platform decoder translating Win32 Console `INPUT_RECORD`
//! payloads into Graticule [`Event`]s.
//!
//! Kept free of Win32 FFI types so every edge case — UTF-16 surrogate pairs,
//! AltGr composition, `Ctrl+<letter>`, mouse button press/release/drag
//! transitions, scroll wheels, and resize/focus events — is deterministically
//! unit-tested on all platforms (including Linux CI).

use graticule_vt::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};

// Control key state bitmasks (Win32 `dwControlKeyState`).
pub(crate) const RIGHT_ALT_PRESSED: u32 = 0x0001;
pub(crate) const LEFT_ALT_PRESSED: u32 = 0x0002;
pub(crate) const RIGHT_CTRL_PRESSED: u32 = 0x0004;
pub(crate) const LEFT_CTRL_PRESSED: u32 = 0x0008;
pub(crate) const SHIFT_PRESSED: u32 = 0x0010;
pub(crate) const NUMLOCK_ON: u32 = 0x0020;
pub(crate) const CAPSLOCK_ON: u32 = 0x0080;
pub(crate) const ENHANCED_KEY: u32 = 0x0100;

// Mouse button bitmasks (`dwButtonState`).
pub(crate) const FROM_LEFT_1ST_BUTTON_PRESSED: u32 = 0x0001;
pub(crate) const RIGHTMOST_BUTTON_PRESSED: u32 = 0x0002;
pub(crate) const FROM_LEFT_2ND_BUTTON_PRESSED: u32 = 0x0004;

// Mouse event flags (`dwEventFlags`).
pub(crate) const MOUSE_MOVED: u32 = 0x0001;
pub(crate) const MOUSE_WHEELED: u32 = 0x0004;
pub(crate) const MOUSE_HWHEELED: u32 = 0x0008;

// Win32 Virtual-Key codes (`VK_*`).
const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_CLEAR: u16 = 0x0C;
const VK_RETURN: u16 = 0x0D;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_PAUSE: u16 = 0x13;
const VK_CAPITAL: u16 = 0x14;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_SNAPSHOT: u16 = 0x2C;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_APPS: u16 = 0x5D;
const VK_NUMPAD0: u16 = 0x60;
const VK_DIVIDE: u16 = 0x6F;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;
const VK_NUMLOCK: u16 = 0x90;
const VK_SCROLL: u16 = 0x91;
const VK_LSHIFT: u16 = 0xA0;
const VK_RSHIFT: u16 = 0xA1;
const VK_LCONTROL: u16 = 0xA2;
const VK_RCONTROL: u16 = 0xA3;
const VK_LMENU: u16 = 0xA4;
const VK_RMENU: u16 = 0xA5;
const VK_VOLUME_MUTE: u16 = 0xAD;
const VK_VOLUME_DOWN: u16 = 0xAE;
const VK_VOLUME_UP: u16 = 0xAF;
const VK_MEDIA_NEXT_TRACK: u16 = 0xB0;
const VK_MEDIA_PREV_TRACK: u16 = 0xB1;
const VK_MEDIA_STOP: u16 = 0xB2;
const VK_MEDIA_PLAY_PAUSE: u16 = 0xB3;

const MAX_REPEAT_COUNT: u16 = 64;

/// Platform-agnostic mirror of a Win32 `KEY_EVENT_RECORD`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawKeyRecord {
    pub key_down: bool,
    pub repeat_count: u16,
    pub virtual_key_code: u16,
    pub unicode_char: u16,
    pub control_key_state: u32,
}

/// Platform-agnostic mirror of a Win32 `MOUSE_EVENT_RECORD`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawMouseRecord {
    pub x: i16,
    pub y: i16,
    pub button_state: u32,
    pub control_key_state: u32,
    pub event_flags: u32,
    /// Viewport top-left origin (`srWindow.Left`, `srWindow.Top`).
    pub window_origin: (i16, i16),
}

/// Platform-agnostic mirror of the Win32 `INPUT_RECORD` variants we handle.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RawWinRecord {
    Key(RawKeyRecord),
    Mouse(RawMouseRecord),
    Resize(u16, u16),
    Focus(bool),
}

/// Stateful decoder for Win32 Console `INPUT_RECORD` streams.
#[derive(Debug, Default)]
pub(crate) struct WinInputDecoder {
    pending_high_surrogate: Option<u16>,
    prev_mouse_buttons: u32,
}

impl WinInputDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn decode(&mut self, record: RawWinRecord, out: &mut Vec<Event>) {
        match record {
            RawWinRecord::Key(k) => self.decode_key(k, out),
            RawWinRecord::Mouse(m) => self.decode_mouse(m, out),
            RawWinRecord::Resize(cols, rows) => {
                if cols > 0 && rows > 0 {
                    out.push(Event::Resize(cols, rows));
                }
            }
            RawWinRecord::Focus(focused) => {
                out.push(if focused {
                    Event::FocusGained
                } else {
                    Event::FocusLost
                });
            }
        }
    }

    fn decode_key(&mut self, rec: RawKeyRecord, out: &mut Vec<Event>) {
        // Ignore pure key-up for standalone modifier keys unless needed; keep
        // regular key releases so callers inspecting `KeyEventKind::Release` see them.
        if !rec.key_down && is_standalone_modifier_vk(rec.virtual_key_code) {
            return;
        }

        let kind = if rec.key_down {
            KeyEventKind::Press
        } else {
            KeyEventKind::Release
        };

        let mut modifiers = decode_modifiers(rec.control_key_state);
        let mut state = decode_key_state(rec.control_key_state);
        if (VK_NUMPAD0..=VK_DIVIDE).contains(&rec.virtual_key_code)
            || (rec.virtual_key_code == VK_RETURN && (rec.control_key_state & ENHANCED_KEY) != 0)
        {
            state |= KeyEventState::KEYPAD;
        }

        // UTF-16 surrogate pair reassembly (emoji / supplementary-plane Unicode).
        if rec.key_down && (0xD800..=0xDBFF).contains(&rec.unicode_char) {
            self.pending_high_surrogate = Some(rec.unicode_char);
            return;
        }
        if rec.key_down && (0xDC00..=0xDFFF).contains(&rec.unicode_char) {
            if let Some(high) = self.pending_high_surrogate.take() {
                let cp = 0x10000
                    + (((u32::from(high) - 0xD800) << 10) | (u32::from(rec.unicode_char) - 0xDC00));
                if let Some(ch) = char::from_u32(cp) {
                    emit_repeated(
                        out,
                        KeyEvent::new_with_state(
                            KeyCode::Char(ch),
                            KeyModifiers::NONE,
                            kind,
                            state,
                        ),
                        rec.repeat_count,
                    );
                }
            }
            return;
        }
        if rec.key_down {
            self.pending_high_surrogate = None;
        }

        // AltGr on Windows sets LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED while
        // supplying the composed Unicode character (e.g. `@`, `€`, `{`).
        let is_alt_gr = (rec.control_key_state & LEFT_CTRL_PRESSED) != 0
            && (rec.control_key_state & RIGHT_ALT_PRESSED) != 0;
        if is_alt_gr && rec.unicode_char >= 0x20 {
            modifiers.remove(KeyModifiers::CONTROL | KeyModifiers::ALT);
        }

        let Some(code) = map_key_code(&rec, &mut modifiers) else {
            return;
        };

        // Only deliver key presses for standalone modifiers on key-down.
        if matches!(code, KeyCode::Modifier(_)) {
            return;
        }

        emit_repeated(
            out,
            KeyEvent::new_with_state(code, modifiers, kind, state),
            rec.repeat_count,
        );
    }

    fn decode_mouse(&mut self, rec: RawMouseRecord, out: &mut Vec<Event>) {
        let column = rec.x.saturating_sub(rec.window_origin.0).max(0) as u16;
        let row = rec.y.saturating_sub(rec.window_origin.1).max(0) as u16;
        let modifiers = decode_modifiers(rec.control_key_state);

        if (rec.event_flags & MOUSE_WHEELED) != 0 {
            let delta = (rec.button_state >> 16) as i16;
            let kind = if delta > 0 {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            };
            out.push(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            }));
            return;
        }

        if (rec.event_flags & MOUSE_HWHEELED) != 0 {
            let delta = (rec.button_state >> 16) as i16;
            let kind = if delta > 0 {
                MouseEventKind::ScrollRight
            } else {
                MouseEventKind::ScrollLeft
            };
            out.push(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            }));
            return;
        }

        let current_buttons = rec.button_state
            & (FROM_LEFT_1ST_BUTTON_PRESSED
                | RIGHTMOST_BUTTON_PRESSED
                | FROM_LEFT_2ND_BUTTON_PRESSED);

        if (rec.event_flags & MOUSE_MOVED) != 0 {
            let kind = if (current_buttons & FROM_LEFT_1ST_BUTTON_PRESSED) != 0 {
                MouseEventKind::Drag(MouseButton::Left)
            } else if (current_buttons & RIGHTMOST_BUTTON_PRESSED) != 0 {
                MouseEventKind::Drag(MouseButton::Right)
            } else if (current_buttons & FROM_LEFT_2ND_BUTTON_PRESSED) != 0 {
                MouseEventKind::Drag(MouseButton::Middle)
            } else {
                MouseEventKind::Moved
            };
            out.push(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            }));
            self.prev_mouse_buttons = current_buttons;
            return;
        }

        let prev = self.prev_mouse_buttons;
        self.prev_mouse_buttons = current_buttons;

        const BUTTONS: [(u32, MouseButton); 3] = [
            (FROM_LEFT_1ST_BUTTON_PRESSED, MouseButton::Left),
            (RIGHTMOST_BUTTON_PRESSED, MouseButton::Right),
            (FROM_LEFT_2ND_BUTTON_PRESSED, MouseButton::Middle),
        ];

        for (mask, btn) in BUTTONS {
            let was_down = (prev & mask) != 0;
            let is_down = (current_buttons & mask) != 0;
            if !was_down && is_down {
                out.push(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(btn),
                    column,
                    row,
                    modifiers,
                }));
            } else if was_down && !is_down {
                out.push(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Up(btn),
                    column,
                    row,
                    modifiers,
                }));
            }
        }
    }
}

fn emit_repeated(out: &mut Vec<Event>, ev: KeyEvent, repeat_count: u16) {
    let count = repeat_count.clamp(1, MAX_REPEAT_COUNT);
    for i in 0..count {
        let mut copy = ev;
        if i > 0 && copy.kind == KeyEventKind::Press {
            copy.kind = KeyEventKind::Repeat;
        }
        out.push(Event::Key(copy));
    }
}

fn decode_modifiers(control_key_state: u32) -> KeyModifiers {
    let mut mods = KeyModifiers::NONE;
    if (control_key_state & SHIFT_PRESSED) != 0 {
        mods |= KeyModifiers::SHIFT;
    }
    if (control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED)) != 0 {
        mods |= KeyModifiers::CONTROL;
    }
    if (control_key_state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED)) != 0 {
        mods |= KeyModifiers::ALT;
    }
    mods
}

fn decode_key_state(control_key_state: u32) -> KeyEventState {
    let mut state = KeyEventState::NONE;
    if (control_key_state & CAPSLOCK_ON) != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if (control_key_state & NUMLOCK_ON) != 0 {
        state |= KeyEventState::NUM_LOCK;
    }
    state
}

fn is_standalone_modifier_vk(vk: u16) -> bool {
    matches!(
        vk,
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
    )
}

fn map_key_code(rec: &RawKeyRecord, modifiers: &mut KeyModifiers) -> Option<KeyCode> {
    match rec.virtual_key_code {
        VK_BACK => return Some(KeyCode::Backspace),
        VK_RETURN => return Some(KeyCode::Enter),
        VK_ESCAPE => return Some(KeyCode::Esc),
        VK_TAB => {
            return Some(if modifiers.contains(KeyModifiers::SHIFT) {
                KeyCode::BackTab
            } else {
                KeyCode::Tab
            });
        }
        VK_LEFT => return Some(KeyCode::Left),
        VK_RIGHT => return Some(KeyCode::Right),
        VK_UP => return Some(KeyCode::Up),
        VK_DOWN => return Some(KeyCode::Down),
        VK_HOME => return Some(KeyCode::Home),
        VK_END => return Some(KeyCode::End),
        VK_PRIOR => return Some(KeyCode::PageUp),
        VK_NEXT => return Some(KeyCode::PageDown),
        VK_INSERT => return Some(KeyCode::Insert),
        VK_DELETE => return Some(KeyCode::Delete),
        VK_CLEAR => return Some(KeyCode::KeypadBegin),
        VK_CAPITAL => return Some(KeyCode::CapsLock),
        VK_NUMLOCK => return Some(KeyCode::NumLock),
        VK_SCROLL => return Some(KeyCode::ScrollLock),
        VK_SNAPSHOT => return Some(KeyCode::PrintScreen),
        VK_PAUSE => return Some(KeyCode::Pause),
        VK_APPS => return Some(KeyCode::Menu),
        VK_VOLUME_MUTE => return Some(KeyCode::Media(MediaKeyCode::MuteVolume)),
        VK_VOLUME_DOWN => return Some(KeyCode::Media(MediaKeyCode::LowerVolume)),
        VK_VOLUME_UP => return Some(KeyCode::Media(MediaKeyCode::RaiseVolume)),
        VK_MEDIA_NEXT_TRACK => return Some(KeyCode::Media(MediaKeyCode::TrackNext)),
        VK_MEDIA_PREV_TRACK => return Some(KeyCode::Media(MediaKeyCode::TrackPrevious)),
        VK_MEDIA_STOP => return Some(KeyCode::Media(MediaKeyCode::Stop)),
        VK_MEDIA_PLAY_PAUSE => return Some(KeyCode::Media(MediaKeyCode::PlayPause)),
        vk @ VK_F1..=VK_F24 => return Some(KeyCode::F((vk - VK_F1 + 1) as u8)),
        VK_SHIFT | VK_LSHIFT => return Some(KeyCode::Modifier(ModifierKeyCode::LeftShift)),
        VK_RSHIFT => return Some(KeyCode::Modifier(ModifierKeyCode::RightShift)),
        VK_CONTROL | VK_LCONTROL => return Some(KeyCode::Modifier(ModifierKeyCode::LeftControl)),
        VK_RCONTROL => return Some(KeyCode::Modifier(ModifierKeyCode::RightControl)),
        VK_MENU | VK_LMENU => return Some(KeyCode::Modifier(ModifierKeyCode::LeftAlt)),
        VK_RMENU => return Some(KeyCode::Modifier(ModifierKeyCode::RightAlt)),
        VK_LWIN => return Some(KeyCode::Modifier(ModifierKeyCode::LeftSuper)),
        VK_RWIN => return Some(KeyCode::Modifier(ModifierKeyCode::RightSuper)),
        _ => {}
    }

    // Printable UTF-16 BMP character (not a C0 control byte).
    if rec.unicode_char >= 0x20
        && rec.unicode_char != 0x7f
        && let Some(ch) = char::from_u32(u32::from(rec.unicode_char))
    {
        // Match Unix convention: plain shifted punctuation (e.g. Shift+1 -> '!')
        // drops SHIFT when SHIFT is the sole modifier, while uppercase ASCII
        // keeps SHIFT so keymaps remain consistent across platforms.
        if *modifiers == KeyModifiers::SHIFT && !ch.is_uppercase() {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        return Some(KeyCode::Char(ch));
    }

    // Control + ASCII letter (`A`..=`Z`, VK 0x41..=0x5A) where Windows delivers
    // control code 0x01..=0x1A or 0x00 in `unicode_char`.
    if modifiers.contains(KeyModifiers::CONTROL) && (0x41..=0x5A).contains(&rec.virtual_key_code) {
        let letter = (rec.virtual_key_code as u8).to_ascii_lowercase() as char;
        return Some(KeyCode::Char(letter));
    }

    // Control + Space (`VK_SPACE == 0x20`, `unicode_char == 0x00`).
    if modifiers.contains(KeyModifiers::CONTROL) && rec.virtual_key_code == 0x20 {
        return Some(KeyCode::Char(' '));
    }

    // Alt + ASCII letter / digit when Windows leaves `unicode_char == 0`.
    if modifiers.contains(KeyModifiers::ALT) {
        if (0x41..=0x5A).contains(&rec.virtual_key_code) {
            let letter = (rec.virtual_key_code as u8).to_ascii_lowercase() as char;
            return Some(KeyCode::Char(letter));
        }
        if (0x30..=0x39).contains(&rec.virtual_key_code) {
            return Some(KeyCode::Char(rec.virtual_key_code as u8 as char));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16_surrogate_pair_into_single_emoji_char() {
        let mut dec = WinInputDecoder::new();
        let mut out = Vec::new();

        // '😀' = U+1F600 = high 0xD83D, low 0xDE00
        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: 0,
                unicode_char: 0xD83D,
                control_key_state: 0,
            }),
            &mut out,
        );
        assert!(out.is_empty());

        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: 0,
                unicode_char: 0xDE00,
                control_key_state: 0,
            }),
            &mut out,
        );
        assert_eq!(out, vec![Event::Key(KeyCode::Char('😀').into())]);
    }

    #[test]
    fn decodes_alt_gr_character_without_leaking_ctrl_alt() {
        let mut dec = WinInputDecoder::new();
        let mut out = Vec::new();
        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: 0x45, // 'E'
                unicode_char: '€' as u16,
                control_key_state: LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED,
            }),
            &mut out,
        );
        assert_eq!(out, vec![Event::Key(KeyCode::Char('€').into())]);
    }

    #[test]
    fn decodes_ctrl_shift_p_and_backtab_and_function_keys() {
        let mut dec = WinInputDecoder::new();
        let mut out = Vec::new();

        // Ctrl+Shift+P
        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: 0x50,
                unicode_char: 0x10,
                control_key_state: LEFT_CTRL_PRESSED | SHIFT_PRESSED,
            }),
            &mut out,
        );
        // Shift+Tab -> BackTab
        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: VK_TAB,
                unicode_char: b'\t' as u16,
                control_key_state: SHIFT_PRESSED,
            }),
            &mut out,
        );
        // F12
        dec.decode(
            RawWinRecord::Key(RawKeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key_code: VK_F1 + 11,
                unicode_char: 0,
                control_key_state: 0,
            }),
            &mut out,
        );

        assert_eq!(
            out,
            vec![
                Event::Key(KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                )),
                Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                Event::Key(KeyCode::F(12).into()),
            ]
        );
    }

    #[test]
    fn decodes_mouse_press_drag_release_hover_and_wheels_relative_to_window_origin() {
        let mut dec = WinInputDecoder::new();
        let mut out = Vec::new();
        let origin = (2, 10);

        // Hover
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 12,
                y: 15,
                button_state: 0,
                control_key_state: 0,
                event_flags: MOUSE_MOVED,
                window_origin: origin,
            }),
            &mut out,
        );
        // Left Down
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 12,
                y: 15,
                button_state: FROM_LEFT_1ST_BUTTON_PRESSED,
                control_key_state: 0,
                event_flags: 0,
                window_origin: origin,
            }),
            &mut out,
        );
        // Drag while Left held
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 14,
                y: 15,
                button_state: FROM_LEFT_1ST_BUTTON_PRESSED,
                control_key_state: SHIFT_PRESSED,
                event_flags: MOUSE_MOVED,
                window_origin: origin,
            }),
            &mut out,
        );
        // Left Up
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 14,
                y: 15,
                button_state: 0,
                control_key_state: 0,
                event_flags: 0,
                window_origin: origin,
            }),
            &mut out,
        );
        // Wheel up (+120 in high word)
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 14,
                y: 15,
                button_state: (120i16 as u16 as u32) << 16,
                control_key_state: LEFT_CTRL_PRESSED,
                event_flags: MOUSE_WHEELED,
                window_origin: origin,
            }),
            &mut out,
        );
        // Wheel down (-120 in high word)
        dec.decode(
            RawWinRecord::Mouse(RawMouseRecord {
                x: 14,
                y: 15,
                button_state: ((-120i16) as u16 as u32) << 16,
                control_key_state: 0,
                event_flags: MOUSE_WHEELED,
                window_origin: origin,
            }),
            &mut out,
        );

        assert_eq!(
            out,
            vec![
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: 10,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 10,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: 12,
                    row: 5,
                    modifiers: KeyModifiers::SHIFT,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: 12,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 12,
                    row: 5,
                    modifiers: KeyModifiers::CONTROL,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 12,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                }),
            ]
        );
    }

    #[test]
    fn decodes_resize_and_focus_events() {
        let mut dec = WinInputDecoder::new();
        let mut out = Vec::new();
        dec.decode(RawWinRecord::Resize(120, 40), &mut out);
        dec.decode(RawWinRecord::Focus(true), &mut out);
        dec.decode(RawWinRecord::Focus(false), &mut out);
        assert_eq!(
            out,
            vec![Event::Resize(120, 40), Event::FocusGained, Event::FocusLost,]
        );
    }
}
