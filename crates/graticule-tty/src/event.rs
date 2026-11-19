//! Terminal input event types owned by Graticule.
//!
//! These types form the stable boundary between the physical TTY parser/reader
//! and higher-level UI routers so callers never depend on a specific terminal
//! backend crate's event types.

use bitflags::bitflags;

/// An input or lifecycle event delivered from the host terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    /// A keyboard press, repeat, or release event.
    Key(KeyEvent),
    /// A mouse press, release, drag, move, or scroll wheel event.
    Mouse(MouseEvent),
    /// Terminal viewport resized to `(columns, rows)`.
    Resize(u16, u16),
    /// Bracketed paste payload (`ESC [ 200 ~ ... ESC [ 201 ~`).
    Paste(String),
    /// The terminal window gained focus (`ESC [ I`).
    FocusGained,
    /// The terminal window lost focus (`ESC [ O`).
    FocusLost,
}

/// A keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// Logical key identity.
    pub code: KeyCode,
    /// Active modifier keys.
    pub modifiers: KeyModifiers,
    /// Press, repeat, or release.
    pub kind: KeyEventKind,
    /// Extra key state flags (e.g. keypad origin, lock states).
    pub state: KeyEventState,
}

impl KeyEvent {
    /// Construct a press [`KeyEvent`] with empty state.
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Construct a [`KeyEvent`] with explicit kind and empty state.
    pub const fn new_with_kind(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Self {
        Self {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    /// Construct a [`KeyEvent`] with explicit kind and state.
    pub const fn new_with_state(
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
        state: KeyEventState,
    ) -> Self {
        Self {
            code,
            modifiers,
            kind,
            state,
        }
    }

    /// Whether this event represents an initial key press.
    pub const fn is_press(&self) -> bool {
        matches!(self.kind, KeyEventKind::Press)
    }

    /// Whether this event represents an auto-repeat key event.
    pub const fn is_repeat(&self) -> bool {
        matches!(self.kind, KeyEventKind::Repeat)
    }

    /// Whether this event represents a key release.
    pub const fn is_release(&self) -> bool {
        matches!(self.kind, KeyEventKind::Release)
    }
}

impl From<KeyCode> for KeyEvent {
    fn from(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }
}

/// Logical key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Backspace / DEL (`0x7f` or `0x08`).
    Backspace,
    /// Enter / Return (`\r`).
    Enter,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page Up key.
    PageUp,
    /// Page Down key.
    PageDown,
    /// Tab key.
    Tab,
    /// BackTab (Shift+Tab).
    BackTab,
    /// Forward Delete key.
    Delete,
    /// Insert key.
    Insert,
    /// Function key (`F1`..`F24`).
    F(u8),
    /// Printable character.
    Char(char),
    /// Null (`Ctrl+Space` / `NUL`).
    Null,
    /// Escape key.
    Esc,
    /// Caps Lock key (Kitty protocol).
    CapsLock,
    /// Scroll Lock key (Kitty protocol).
    ScrollLock,
    /// Num Lock key (Kitty protocol).
    NumLock,
    /// Print Screen key (Kitty protocol).
    PrintScreen,
    /// Pause/Break key (Kitty protocol).
    Pause,
    /// Context Menu key (Kitty protocol).
    Menu,
    /// Keypad 5 / Begin key.
    KeypadBegin,
    /// Media control key.
    Media(MediaKeyCode),
    /// Standalone modifier key event (Kitty protocol).
    Modifier(ModifierKeyCode),
}

/// Media key variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKeyCode {
    /// Play.
    Play,
    /// Pause.
    Pause,
    /// Play/Pause toggle.
    PlayPause,
    /// Reverse.
    Reverse,
    /// Stop.
    Stop,
    /// Fast-forward.
    FastForward,
    /// Rewind.
    Rewind,
    /// Next track.
    TrackNext,
    /// Previous track.
    TrackPrevious,
    /// Record.
    Record,
    /// Lower volume.
    LowerVolume,
    /// Raise volume.
    RaiseVolume,
    /// Mute volume.
    MuteVolume,
}

/// Standalone modifier key variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifierKeyCode {
    /// Left Shift.
    LeftShift,
    /// Left Control.
    LeftControl,
    /// Left Alt.
    LeftAlt,
    /// Left Super.
    LeftSuper,
    /// Left Hyper.
    LeftHyper,
    /// Left Meta.
    LeftMeta,
    /// Right Shift.
    RightShift,
    /// Right Control.
    RightControl,
    /// Right Alt.
    RightAlt,
    /// Right Super.
    RightSuper,
    /// Right Hyper.
    RightHyper,
    /// Right Meta.
    RightMeta,
    /// ISO Level 3 Shift.
    IsoLevel3Shift,
    /// ISO Level 5 Shift.
    IsoLevel5Shift,
}

bitflags! {
    /// Modifier keys held during an input event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct KeyModifiers: u8 {
        /// Shift modifier.
        const SHIFT = 0b0000_0001;
        /// Control modifier.
        const CONTROL = 0b0000_0010;
        /// Alt / Option modifier.
        const ALT = 0b0000_0100;
        /// Super / Command / Windows modifier.
        const SUPER = 0b0000_1000;
        /// Hyper modifier.
        const HYPER = 0b0001_0000;
        /// Meta modifier.
        const META = 0b0010_0000;
        /// No modifiers held.
        const NONE = 0b0000_0000;
    }
}

/// Kind of keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyEventKind {
    /// Key pressed down.
    #[default]
    Press,
    /// Key held and auto-repeating.
    Repeat,
    /// Key released.
    Release,
}

bitflags! {
    /// Extra state associated with a key event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct KeyEventState: u8 {
        /// No extra state.
        const NONE = 0b0000_0000;
        /// Originated from the numeric keypad.
        const KEYPAD = 0b0000_0001;
        /// Caps Lock was active.
        const CAPS_LOCK = 0b0000_0010;
        /// Num Lock was active.
        const NUM_LOCK = 0b0000_0100;
    }
}

/// A mouse event with 0-based terminal cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// Action kind (press, release, drag, move, scroll).
    pub kind: MouseEventKind,
    /// 0-based column index.
    pub column: u16,
    /// 0-based row index.
    pub row: u16,
    /// Active modifier keys.
    pub modifiers: KeyModifiers,
}

/// Kind of mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    /// Button pressed at `(column, row)`.
    Down(MouseButton),
    /// Button released at `(column, row)`.
    Up(MouseButton),
    /// Pointer moved while holding `MouseButton`.
    Drag(MouseButton),
    /// Pointer moved with no button held (`?1003h` all-motion tracking).
    Moved,
    /// Wheel scrolled down.
    ScrollDown,
    /// Wheel scrolled up.
    ScrollUp,
    /// Wheel scrolled left.
    ScrollLeft,
    /// Wheel scrolled right.
    ScrollRight,
}

/// Mouse button identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary (left) button.
    Left,
    /// Secondary (right) button.
    Right,
    /// Middle (wheel) button.
    Middle,
}
