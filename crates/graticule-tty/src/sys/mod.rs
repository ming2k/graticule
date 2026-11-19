#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::{run_event_loop, terminal_size};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{probe_keyboard_enhancement_sync, run_event_loop, terminal_size};
