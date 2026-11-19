#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::{
    disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, is_tty, run_event_loop, terminal_size,
};

#[cfg(any(windows, test))]
pub(crate) mod win_decode;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{
    WakeHandle, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, is_tty,
    probe_keyboard_enhancement_sync, run_event_loop, terminal_size,
};
