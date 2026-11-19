//! Precise xterm mouse-tracking mode control.
//!
//! Always pairs the selected tracking mode with SGR-1006 encoding (`?1006h`) so
//! coordinates above 223 work and button press (`M`) vs release (`m`) are
//! unambiguous.

use std::fmt;

/// Mouse-tracking mode negotiated with the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// Do not request mouse reporting from the terminal.
    #[default]
    None,
    /// Button-event tracking (`?1002h`) + SGR-1006 (`?1006h`): press, release,
    /// drag while a button is held, and scroll wheel.
    Button,
    /// Any-event / all-motion tracking (`?1003h`) + SGR-1006 (`?1006h`):
    /// everything in [`Self::Button`] plus hover motion (`Moved`) with no
    /// button held.
    AllMotion,
}

impl MouseMode {
    /// Write the ANSI CSI sequence that enables this mode.
    pub fn write_enable(self, f: &mut impl fmt::Write) -> fmt::Result {
        match self {
            Self::None => Ok(()),
            Self::Button => f.write_str("\x1b[?1002h\x1b[?1006h"),
            Self::AllMotion => f.write_str("\x1b[?1003h\x1b[?1006h"),
        }
    }

    /// Write the ANSI CSI sequence that disables this mode (in reverse order).
    pub fn write_disable(self, f: &mut impl fmt::Write) -> fmt::Result {
        match self {
            Self::None => Ok(()),
            Self::Button => f.write_str("\x1b[?1006l\x1b[?1002l"),
            Self::AllMotion => f.write_str("\x1b[?1006l\x1b[?1003l"),
        }
    }

    /// Exact ANSI byte sequence to enable this mode.
    pub const fn enable_bytes(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Button => "\x1b[?1002h\x1b[?1006h",
            Self::AllMotion => "\x1b[?1003h\x1b[?1006h",
        }
    }

    /// Exact ANSI byte sequence to disable this mode.
    pub const fn disable_bytes(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Button => "\x1b[?1006l\x1b[?1002l",
            Self::AllMotion => "\x1b[?1006l\x1b[?1003l",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_mode_emits_1002_and_sgr_only() {
        let mut on = String::new();
        MouseMode::Button.write_enable(&mut on).unwrap();
        assert_eq!(on, "\x1b[?1002h\x1b[?1006h");

        let mut off = String::new();
        MouseMode::Button.write_disable(&mut off).unwrap();
        assert_eq!(off, "\x1b[?1006l\x1b[?1002l");
    }

    #[test]
    fn all_motion_mode_emits_1003_and_sgr() {
        let mut on = String::new();
        MouseMode::AllMotion.write_enable(&mut on).unwrap();
        assert_eq!(on, "\x1b[?1003h\x1b[?1006h");

        let mut off = String::new();
        MouseMode::AllMotion.write_disable(&mut off).unwrap();
        assert_eq!(off, "\x1b[?1006l\x1b[?1003l");
    }

    #[test]
    fn none_mode_emits_nothing() {
        assert_eq!(MouseMode::None.enable_bytes(), "");
        assert_eq!(MouseMode::None.disable_bytes(), "");
    }
}
