//! Pure-native Win32 Console API driver backed directly by `windows-sys`.
//!
//! - Enables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the console output handle
//!   so `graticule-vt` ANSI sequences render natively on Windows 10/11 & ConPTY.
//! - Saves and restores exact original input & output console modes.
//! - Uses `WaitForMultipleObjects([conin, wake_event])` + `ReadConsoleInputW`
//!   feeding the cross-platform [`super::win_decode::WinInputDecoder`], giving
//!   instant 0 ms shutdown wakeup and structured UTF-16 / mouse / resize / focus
//!   decoding.

#![allow(unsafe_code)]

use std::io;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use graticule_vt::event::Event;
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT,
    ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, FOCUS_EVENT,
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD, KEY_EVENT, MOUSE_EVENT,
    ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

use super::win_decode::{RawKeyRecord, RawMouseRecord, RawWinRecord, WinInputDecoder};

const READ_BATCH_SIZE: usize = 128;

struct ConsoleHandle {
    raw: HANDLE,
    owned: bool,
}

// SAFETY: Win32 console handles are process-global kernel handles safe to pass
// across threads for synchronized console API calls.
unsafe impl Send for ConsoleHandle {}

impl ConsoleHandle {
    fn open_input() -> io::Result<Self> {
        // SAFETY: `GetStdHandle` has no memory-safety preconditions.
        let std_in = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if is_valid_console_handle(std_in) {
            return Ok(Self {
                raw: std_in,
                owned: false,
            });
        }
        Self::open_device("CONIN$\0")
    }

    fn open_output() -> io::Result<Self> {
        // SAFETY: `GetStdHandle` has no memory-safety preconditions.
        let std_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if is_valid_console_handle(std_out) {
            return Ok(Self {
                raw: std_out,
                owned: false,
            });
        }
        Self::open_device("CONOUT$\0")
    }

    fn open_device(name_nul: &str) -> io::Result<Self> {
        let wide: Vec<u16> = name_nul.encode_utf16().collect();
        // SAFETY: `wide` is NUL-terminated UTF-16 valid for the call duration.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                raw: handle,
                owned: true,
            })
        }
    }

    fn raw(&self) -> HANDLE {
        self.raw
    }
}

impl Drop for ConsoleHandle {
    fn drop(&mut self) {
        if self.owned && !self.raw.is_null() && self.raw != INVALID_HANDLE_VALUE {
            // SAFETY: We own this kernel handle from `CreateFileW`.
            unsafe {
                CloseHandle(self.raw);
            }
        }
    }
}

fn is_valid_console_handle(handle: HANDLE) -> bool {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let mut mode = 0u32;
    // SAFETY: `mode` is a valid writable `u32`.
    unsafe { GetConsoleMode(handle, &mut mode) != 0 }
}

struct SavedConsoleModes {
    in_handle: ConsoleHandle,
    out_handle: ConsoleHandle,
    orig_in_mode: u32,
    orig_out_mode: u32,
}

static SAVED_MODES: Mutex<Option<SavedConsoleModes>> = Mutex::new(None);

/// Thread-safe manual-reset Win32 event used to wake `WaitForMultipleObjects`
/// immediately when [`crate::InputReader`] is dropped.
pub(crate) struct WakeHandle {
    raw: HANDLE,
}

// SAFETY: Win32 event handles are thread-safe kernel objects for `SetEvent` and
// `WaitForMultipleObjects`, closed once on drop.
unsafe impl Send for WakeHandle {}
unsafe impl Sync for WakeHandle {}

impl WakeHandle {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Manual-reset, initially unsignaled anonymous Win32 event.
        let raw = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if raw.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self { raw })
        }
    }

    pub(crate) fn wake(&self) {
        // SAFETY: `self.raw` is a live Win32 event handle.
        unsafe {
            let _ = SetEvent(self.raw);
        }
    }

    fn raw(&self) -> HANDLE {
        self.raw
    }
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: Owned handle created by `CreateEventW`.
            unsafe {
                CloseHandle(self.raw);
            }
        }
    }
}

/// Whether the process is attached to an interactive Windows console.
pub(crate) fn is_tty() -> bool {
    // SAFETY: `GetStdHandle` has no memory-safety preconditions.
    let std_in = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let std_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    is_valid_console_handle(std_in) || is_valid_console_handle(std_out)
}

/// Put the Windows console into raw input + native VT output mode, saving the
/// exact original modes for restoration.
pub(crate) fn enable_raw_mode() -> io::Result<()> {
    let mut guard = SAVED_MODES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_some() {
        return Ok(());
    }

    let in_handle = ConsoleHandle::open_input()?;
    let out_handle = ConsoleHandle::open_output()?;

    let mut orig_in_mode = 0u32;
    let mut orig_out_mode = 0u32;
    // SAFETY: Valid console handles and writable `u32` pointers.
    if unsafe { GetConsoleMode(in_handle.raw(), &mut orig_in_mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetConsoleMode(out_handle.raw(), &mut orig_out_mode) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let raw_in = (orig_in_mode
        & !(ENABLE_LINE_INPUT
            | ENABLE_ECHO_INPUT
            | ENABLE_PROCESSED_INPUT
            | ENABLE_QUICK_EDIT_MODE))
        | ENABLE_EXTENDED_FLAGS
        | ENABLE_WINDOW_INPUT
        | ENABLE_MOUSE_INPUT;
    let vt_out = orig_out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;

    // SAFETY: Valid console handles.
    if unsafe { SetConsoleMode(in_handle.raw(), raw_in) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // Enable VT output; if `DISABLE_NEWLINE_AUTO_RETURN` is rejected by an
    // older host, retry with `ENABLE_VIRTUAL_TERMINAL_PROCESSING` alone.
    if unsafe { SetConsoleMode(out_handle.raw(), vt_out) } == 0 {
        let fallback_out = orig_out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if unsafe { SetConsoleMode(out_handle.raw(), fallback_out) } == 0 {
            let err = io::Error::last_os_error();
            let _ = unsafe { SetConsoleMode(in_handle.raw(), orig_in_mode) };
            return Err(err);
        }
    }

    *guard = Some(SavedConsoleModes {
        in_handle,
        out_handle,
        orig_in_mode,
        orig_out_mode,
    });
    Ok(())
}

/// Restore the original Windows console input and output modes.
pub(crate) fn disable_raw_mode() -> io::Result<()> {
    let saved = {
        let mut guard = SAVED_MODES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take()
    };
    if let Some(saved) = saved {
        // SAFETY: Valid console handles saved from `enable_raw_mode`.
        let r1 = unsafe { SetConsoleMode(saved.in_handle.raw(), saved.orig_in_mode) };
        let r2 = unsafe { SetConsoleMode(saved.out_handle.raw(), saved.orig_out_mode) };
        if r1 == 0 || r2 == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Whether Graticule currently holds the console in raw mode.
pub(crate) fn is_raw_mode_enabled() -> bool {
    SAVED_MODES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

/// Query the visible Windows console viewport `(columns, rows)` via
/// `GetConsoleScreenBufferInfo`.
pub(crate) fn terminal_size() -> io::Result<(u16, u16)> {
    let out_handle = ConsoleHandle::open_output()?;
    let info = query_screen_buffer_info(out_handle.raw())?;
    let cols = info
        .srWindow
        .Right
        .saturating_sub(info.srWindow.Left)
        .saturating_add(1)
        .max(1) as u16;
    let rows = info
        .srWindow
        .Bottom
        .saturating_sub(info.srWindow.Top)
        .saturating_add(1)
        .max(1) as u16;
    Ok((cols, rows))
}

fn query_screen_buffer_info(handle: HANDLE) -> io::Result<CONSOLE_SCREEN_BUFFER_INFO> {
    let mut info = MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::uninit();
    // SAFETY: `info.as_mut_ptr()` is a valid writable `CONSOLE_SCREEN_BUFFER_INFO`.
    if unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: Succeeded and initialized `info`.
        Ok(unsafe { info.assume_init() })
    }
}

pub(crate) fn run_event_loop(
    sink: &mut dyn FnMut(Event) -> bool,
    shutdown: Arc<AtomicBool>,
    wake: Arc<WakeHandle>,
) -> io::Result<()> {
    let in_handle = ConsoleHandle::open_input()?;
    let out_handle = ConsoleHandle::open_output().ok();
    let handles = [in_handle.raw(), wake.raw()];

    let mut decoder = WinInputDecoder::new();
    let mut raw_records = [MaybeUninit::<INPUT_RECORD>::uninit(); READ_BATCH_SIZE];
    let mut events = Vec::with_capacity(32);

    while !shutdown.load(Ordering::Acquire) {
        // SAFETY: `handles` contains two valid kernel handles.
        let wait_rc =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
        if wait_rc == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if shutdown.load(Ordering::Acquire) || wait_rc == WAIT_OBJECT_0 + 1 {
            break;
        }
        if wait_rc != WAIT_OBJECT_0 {
            continue;
        }

        let mut read_count = 0u32;
        // SAFETY: `raw_records` has space for `READ_BATCH_SIZE` `INPUT_RECORD`s.
        let ok = unsafe {
            ReadConsoleInputW(
                in_handle.raw(),
                raw_records.as_mut_ptr().cast::<INPUT_RECORD>(),
                READ_BATCH_SIZE as u32,
                &mut read_count,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let window_info = out_handle
            .as_ref()
            .and_then(|h| query_screen_buffer_info(h.raw()).ok());
        let window_origin = window_info
            .map(|i| (i.srWindow.Left, i.srWindow.Top))
            .unwrap_or((0, 0));

        for slot in raw_records.iter().take(read_count as usize) {
            // SAFETY: First `read_count` records were initialized by `ReadConsoleInputW`.
            let rec = unsafe { slot.assume_init_ref() };
            let parsed = match u32::from(rec.EventType) {
                KEY_EVENT => {
                    // SAFETY: `EventType == KEY_EVENT` selects `Event.KeyEvent`.
                    let k = unsafe { rec.Event.KeyEvent };
                    // SAFETY: Read UTF-16 `UnicodeChar` union field.
                    let unicode_char = unsafe { k.uChar.UnicodeChar };
                    Some(RawWinRecord::Key(RawKeyRecord {
                        key_down: k.bKeyDown != 0,
                        repeat_count: k.wRepeatCount,
                        virtual_key_code: k.wVirtualKeyCode,
                        unicode_char,
                        control_key_state: k.dwControlKeyState,
                    }))
                }
                MOUSE_EVENT => {
                    // SAFETY: `EventType == MOUSE_EVENT` selects `Event.MouseEvent`.
                    let m = unsafe { rec.Event.MouseEvent };
                    Some(RawWinRecord::Mouse(RawMouseRecord {
                        x: m.dwMousePosition.X,
                        y: m.dwMousePosition.Y,
                        button_state: m.dwButtonState,
                        control_key_state: m.dwControlKeyState,
                        event_flags: m.dwEventFlags,
                        window_origin,
                    }))
                }
                WINDOW_BUFFER_SIZE_EVENT => {
                    if let Ok((cols, rows)) = terminal_size() {
                        Some(RawWinRecord::Resize(cols, rows))
                    } else {
                        // SAFETY: `EventType == WINDOW_BUFFER_SIZE_EVENT` selects `WindowBufferSizeEvent`.
                        let s = unsafe { rec.Event.WindowBufferSizeEvent };
                        Some(RawWinRecord::Resize(
                            s.dwSize.X.max(0) as u16,
                            s.dwSize.Y.max(0) as u16,
                        ))
                    }
                }
                FOCUS_EVENT => {
                    // SAFETY: `EventType == FOCUS_EVENT` selects `Event.FocusEvent`.
                    let f = unsafe { rec.Event.FocusEvent };
                    Some(RawWinRecord::Focus(f.bSetFocus != 0))
                }
                _ => None,
            };

            if let Some(raw_ev) = parsed {
                decoder.decode(raw_ev, &mut events);
                for ev in events.drain(..) {
                    if !sink(ev) {
                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}

/// On Windows, structured `ReadConsoleInputW` already disambiguates every key
/// and modifier natively without pushing Kitty CSI-u escape codes.
pub(crate) fn probe_keyboard_enhancement_sync() -> bool {
    false
}
