//! Pure-Rust VT / ECMA-48 / DEC / xterm / Kitty terminal wire protocol.
//!
//! This crate is strictly pure memory (`#![deny(unsafe_code)]`, zero OS or I/O
//! driver dependencies) and owns both halves of the terminal byte stream:
//!
//! - **Outbound encoding**:
//!   - [`csi`]: cursor positioning (`CUP`), cursor visibility (`DECTCEM`),
//!     cursor shape (`DECSCUSR`), screen/line erasure (`ED`/`EL`), scroll
//!     regions (`DECSTBM`, `SU`/`SD`), line wrap (`DECAWM`), alternate screen
//!     (`?1049`), synchronized updates (`?2026`), bracketed paste (`?2004`),
//!     focus reporting (`?1004`), and Kitty keyboard negotiation.
//!   - [`sgr`]: zero-allocation combined SGR (Select Graphic Rendition) encoder
//!     covering DEC VT100 attributes, ECMA-48 16-color, 8-bit 256-color, and
//!     ITU-T T.416 24-bit Direct Color.
//!   - [`osc`]: control-byte-sanitized OSC sequences (window title, OSC 8
//!     hyperlinks, OSC 52 clipboard).
//!   - [`mouse`]: xterm mouse-tracking mode control (`?1002`/`?1003` + `?1006`).
//! - **Inbound decoding**:
//!   - [`event`]: Graticule-owned input [`Event`] model.
//!   - [`parser`]: incremental, timeout-aware [`VtParser`] state machine.

#![warn(missing_docs)]
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod csi;
pub mod event;
pub mod mouse;
pub mod osc;
pub mod parser;
pub mod sgr;

pub use csi::CursorStyle;
pub use event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};
pub use mouse::MouseMode;
pub use parser::{DEFAULT_ESC_TIMEOUT, MAX_PASTE_BYTES, MAX_SEQUENCE_BYTES, ParsedItem, VtParser};
pub use sgr::{SgrColor, SgrEncoder};
