//! Pure-native Unix TTY driver: `termios` raw mode, `TIOCGWINSZ` viewport
//! sizing, and `poll(2)` + `SIGWINCH` self-pipe single-reader input loop.
//!
//! Never sets `O_NONBLOCK` on `STDIN_FILENO` (which shares an open file
//! description with `stdout`/`stderr` on most PTYs and would cause `stdout`
//! writes to fail with `EAGAIN`). Instead, every `read(2)` is strictly guarded
//! by `poll(2)` reporting `POLLIN`.

#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use graticule_vt::event::Event;
use graticule_vt::parser::{ParsedItem, VtParser};

use crate::reader::KeyboardProbe;

const READ_BUF_SIZE: usize = 4096;

struct SavedTermios {
    orig: libc::termios,
    fd: RawFd,
    /// Keeps `/dev/tty` open until raw mode is disabled when `STDIN_FILENO` is
    /// not a TTY.
    _owned_tty: Option<File>,
}

static SAVED_TERMIOS: Mutex<Option<SavedTermios>> = Mutex::new(None);

enum TtySource {
    BorrowedFd(RawFd),
    OwnedFile(File),
}

impl TtySource {
    fn open() -> io::Result<Self> {
        if fd_is_tty(libc::STDIN_FILENO) {
            Ok(Self::BorrowedFd(libc::STDIN_FILENO))
        } else {
            let file = OpenOptions::new().read(true).open("/dev/tty")?;
            Ok(Self::OwnedFile(file))
        }
    }

    fn raw_fd(&self) -> RawFd {
        match self {
            Self::BorrowedFd(fd) => *fd,
            Self::OwnedFile(f) => f.as_raw_fd(),
        }
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `fd` is open for reading (either valid STDIN_FILENO, test fd,
        // or owned File), and `buf` provides `buf.len()` writable bytes.
        let n = unsafe {
            libc::read(
                self.raw_fd(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                buf.len(),
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

fn fd_is_tty(fd: RawFd) -> bool {
    // SAFETY: `isatty` takes an integer fd with no pointer dereference.
    unsafe { libc::isatty(fd) == 1 }
}

/// Whether the process is attached to an interactive TTY on stdin or stdout.
pub(crate) fn is_tty() -> bool {
    fd_is_tty(libc::STDIN_FILENO) || fd_is_tty(libc::STDOUT_FILENO)
}

/// Read the current `termios` configuration of `fd`, retrying on `EINTR`.
fn tcgetattr_retry(fd: RawFd) -> io::Result<libc::termios> {
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    loop {
        // SAFETY: `termios.as_mut_ptr()` is a valid writable `libc::termios`.
        let rc = unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) };
        if rc == 0 {
            // SAFETY: `tcgetattr` succeeded and initialized the struct.
            return Ok(unsafe { termios.assume_init() });
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Apply `termios` immediately (`TCSANOW`), retrying on `EINTR`.
fn tcsetattr_retry(fd: RawFd, termios: &libc::termios) -> io::Result<()> {
    loop {
        // SAFETY: `termios` points to a valid initialized `libc::termios`.
        let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Put a specific TTY descriptor into raw mode, returning the original cooked
/// `termios` so the caller can restore it bit-for-bit.
fn enable_raw_mode_on_fd(fd: RawFd) -> io::Result<libc::termios> {
    let orig = tcgetattr_retry(fd)?;
    let mut raw = orig;
    // SAFETY: `raw` is a valid initialized `libc::termios`.
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    tcsetattr_retry(fd, &raw)?;
    Ok(orig)
}

/// Enable raw mode on `STDIN_FILENO` (or `/dev/tty` if stdin is redirected).
///
/// Idempotent: calling while already in raw mode succeeds without overwriting
/// the saved original cooked `termios`.
pub(crate) fn enable_raw_mode() -> io::Result<()> {
    let mut guard = SAVED_TERMIOS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_some() {
        return Ok(());
    }

    let (fd, owned_tty) = if fd_is_tty(libc::STDIN_FILENO) {
        (libc::STDIN_FILENO, None)
    } else {
        let file = OpenOptions::new().read(true).open("/dev/tty")?;
        let raw_fd = file.as_raw_fd();
        (raw_fd, Some(file))
    };

    let orig = enable_raw_mode_on_fd(fd)?;
    *guard = Some(SavedTermios {
        orig,
        fd,
        _owned_tty: owned_tty,
    });
    Ok(())
}

/// Restore the original cooked `termios` saved by [`enable_raw_mode`].
///
/// Idempotent: safe to call multiple times or when raw mode is not active.
pub(crate) fn disable_raw_mode() -> io::Result<()> {
    let saved = {
        let mut guard = SAVED_TERMIOS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take()
    };
    if let Some(saved) = saved {
        tcsetattr_retry(saved.fd, &saved.orig)?;
    }
    Ok(())
}

/// Whether Graticule currently holds the TTY in raw mode.
pub(crate) fn is_raw_mode_enabled() -> bool {
    SAVED_TERMIOS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

/// Query terminal dimensions directly via `TIOCGWINSZ`, avoiding any subprocess
/// fallback (`tput`).
pub(crate) fn terminal_size() -> io::Result<(u16, u16)> {
    if let Some(sz) = ioctl_winsize(libc::STDOUT_FILENO)
        .or_else(|| ioctl_winsize(libc::STDIN_FILENO))
        .or_else(|| ioctl_winsize(libc::STDERR_FILENO))
    {
        return Ok(sz);
    }
    if let Ok(file) = OpenOptions::new().read(true).open("/dev/tty")
        && let Some(sz) = ioctl_winsize(file.as_raw_fd())
    {
        return Ok(sz);
    }
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "failed to query terminal dimensions via TIOCGWINSZ",
    ))
}

fn ioctl_winsize(fd: RawFd) -> Option<(u16, u16)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `ws` is a valid `libc::winsize` pointer for `TIOCGWINSZ`.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

pub(crate) fn run_event_loop(
    sink: &mut dyn FnMut(Event) -> bool,
    shutdown: Arc<AtomicBool>,
    wake_rx: UnixStream,
    probe: Arc<KeyboardProbe>,
) -> io::Result<()> {
    let tty = TtySource::open()?;
    run_event_loop_on_source(&tty, sink, shutdown, wake_rx, probe)
}

fn run_event_loop_on_source(
    tty: &TtySource,
    sink: &mut dyn FnMut(Event) -> bool,
    shutdown: Arc<AtomicBool>,
    mut wake_rx: UnixStream,
    probe: Arc<KeyboardProbe>,
) -> io::Result<()> {
    wake_rx.set_nonblocking(true)?;

    let (mut sig_rx, sig_tx) = UnixStream::pair()?;
    sig_rx.set_nonblocking(true)?;
    sig_tx.set_nonblocking(true)?;
    let sig_id = signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, sig_tx)?;

    let res = poll_loop(tty, &mut wake_rx, &mut sig_rx, sink, &shutdown, &probe);
    signal_hook::low_level::unregister(sig_id);
    res
}

fn poll_loop(
    tty: &TtySource,
    wake_rx: &mut UnixStream,
    sig_rx: &mut UnixStream,
    sink: &mut dyn FnMut(Event) -> bool,
    shutdown: &AtomicBool,
    probe: &KeyboardProbe,
) -> io::Result<()> {
    let mut parser = VtParser::new();
    let mut buf = [0u8; READ_BUF_SIZE];
    let mut items = Vec::with_capacity(32);

    while !shutdown.load(Ordering::Acquire) {
        let now = Instant::now();
        parser.flush_timeout(now, &mut items);
        if !dispatch_items(&mut items, sink, probe) {
            break;
        }

        let timeout_ms = match parser.next_deadline() {
            Some(deadline) => {
                let rem = deadline.saturating_duration_since(Instant::now());
                let ms = rem.as_millis();
                if ms == 0 {
                    0
                } else {
                    ms.min(i32::MAX as u128) as i32
                }
            }
            None => -1,
        };

        let mut fds = [
            libc::pollfd {
                fd: tty.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_rx.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sig_rx.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        // SAFETY: `fds` points to 3 valid `pollfd` structs.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }

        if shutdown.load(Ordering::Acquire)
            || (fds[1].revents & (libc::POLLIN | libc::POLLHUP)) != 0
        {
            drain_stream(wake_rx);
            break;
        }

        if (fds[2].revents & libc::POLLIN) != 0 {
            drain_stream(sig_rx);
            if let Ok((cols, rows)) = ioctl_winsize(tty.raw_fd())
                .ok_or_else(|| io::Error::other("no winsize"))
                .or_else(|_| terminal_size())
                && !sink(Event::Resize(cols, rows))
            {
                break;
            }
        }

        if (fds[0].revents & libc::POLLIN) != 0 {
            // Drain all currently-readable bytes using non-blocking 0 ms poll probes
            // so batched events arrive together without making `stdin`/`stdout` O_NONBLOCK.
            loop {
                match tty.read_bytes(&mut buf) {
                    Ok(0) => {
                        // Clean EOF / closed PTY — never spin.
                        return Ok(());
                    }
                    Ok(n) => {
                        parser.feed(&buf[..n], Instant::now(), &mut items);
                        if !dispatch_items(&mut items, sink, probe) {
                            return Ok(());
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }

                if !tty_has_more_bytes(tty.raw_fd())? {
                    break;
                }
            }
        } else if (fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            break;
        }
    }

    Ok(())
}

fn tty_has_more_bytes(fd: RawFd) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `pfd` points to 1 valid `pollfd` struct.
        let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(rc > 0 && (pfd.revents & libc::POLLIN) != 0);
    }
}

fn dispatch_items(
    items: &mut Vec<ParsedItem>,
    sink: &mut dyn FnMut(Event) -> bool,
    probe: &KeyboardProbe,
) -> bool {
    for item in items.drain(..) {
        match item {
            ParsedItem::Event(ev) => {
                if !sink(ev) {
                    return false;
                }
            }
            ParsedItem::KeyboardEnhancementFlags(_) => {
                probe.mark_supported(true);
            }
            ParsedItem::PrimaryDeviceAttributes => {
                probe.mark_da1_seen();
            }
        }
    }
    true
}

fn drain_stream(stream: &mut UnixStream) {
    let mut scratch = [0u8; 64];
    while let Ok(n) = stream.read(&mut scratch) {
        if n == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::io::Write;
    use std::os::unix::io::FromRawFd;
    use std::sync::mpsc;
    use std::time::Duration;

    use graticule_vt::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};

    struct LoopHarness {
        ev_rx: mpsc::Receiver<Event>,
        shutdown: Arc<AtomicBool>,
        wake_tx: UnixStream,
        probe: Arc<KeyboardProbe>,
        join: std::thread::JoinHandle<io::Result<()>>,
    }

    fn spawn_loop_on_stream(tty_rx: UnixStream) -> LoopHarness {
        let (ev_tx, ev_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let (wake_rx, wake_tx) = UnixStream::pair().unwrap();
        let probe = Arc::new(KeyboardProbe::default());
        let thread_probe = probe.clone();

        let join = std::thread::spawn(move || {
            let tty = TtySource::BorrowedFd(tty_rx.as_raw_fd());
            let mut sink = move |ev| ev_tx.send(ev).is_ok();
            let res =
                run_event_loop_on_source(&tty, &mut sink, thread_shutdown, wake_rx, thread_probe);
            drop(tty_rx);
            res
        });

        LoopHarness {
            ev_rx,
            shutdown,
            wake_tx,
            probe,
            join,
        }
    }

    #[test]
    fn poll_loop_reassembles_split_sgr_across_syscalls_and_flushes_bare_esc_on_timeout() {
        let (tty_rx, mut tty_tx) = UnixStream::pair().unwrap();
        let mut h = spawn_loop_on_stream(tty_rx);

        // 1. Split SGR mouse report right after ESC across two real writes separated by 3ms.
        tty_tx.write_all(b"\x1b").unwrap();
        tty_tx.flush().unwrap();
        std::thread::sleep(Duration::from_millis(3));
        tty_tx.write_all(b"[<35;52;28M").unwrap();
        tty_tx.flush().unwrap();

        let ev = h.ev_rx.recv_timeout(Duration::from_millis(200)).unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 51,
                row: 27,
                modifiers: KeyModifiers::NONE,
            })
        );

        // 2. Bare ESC with no follow-up flushes via poll timeout (~20ms).
        tty_tx.write_all(b"\x1b").unwrap();
        tty_tx.flush().unwrap();
        let esc = h.ev_rx.recv_timeout(Duration::from_millis(200)).unwrap();
        assert_eq!(esc, Event::Key(KeyCode::Esc.into()));

        // 3. Clean wakeup via wake_tx exits immediately.
        let t0 = Instant::now();
        h.shutdown.store(true, Ordering::Release);
        let _ = h.wake_tx.write_all(b"q");
        h.join.join().unwrap().unwrap();
        assert!(
            t0.elapsed() < Duration::from_millis(100),
            "shutdown wakeup should not stall"
        );
    }

    #[test]
    fn poll_loop_routes_probe_replies_fast_and_exits_cleanly_on_eof() {
        let (tty_rx, mut tty_tx) = UnixStream::pair().unwrap();
        let h = spawn_loop_on_stream(tty_rx);

        // Terminal replies DA1 only (no Kitty support) -> wait returns false immediately.
        let probe_clone = h.probe.clone();
        let waiter = std::thread::spawn(move || {
            let start = Instant::now();
            let supported = probe_clone.wait_for_test(Duration::from_secs(2));
            (supported, start.elapsed())
        });
        std::thread::sleep(Duration::from_millis(5));
        tty_tx.write_all(b"\x1b[?64;1c").unwrap();
        tty_tx.flush().unwrap();

        let (supported, elapsed) = waiter.join().unwrap();
        assert!(!supported);
        assert!(
            elapsed < Duration::from_millis(200),
            "DA1 should resolve unsupported immediately, took {elapsed:?}"
        );

        // Zero probe bytes leaked into user event channel.
        assert!(h.ev_rx.try_recv().is_err());

        // Closing the write end triggers clean EOF exit without spin.
        drop(tty_tx);
        h.join.join().unwrap().unwrap();
    }

    #[test]
    fn real_pty_termios_raw_mode_roundtrip_and_winsize_ioctl() {
        // Open a real kernel PTY master/slave pair to exercise actual
        // `tcgetattr`/`cfmakeraw`/`tcsetattr`/`TIOCGWINSZ` syscalls.
        let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master_fd < 0 {
            // Skip cleanly only if sandboxed environment blocks /dev/ptmx.
            return;
        }
        // SAFETY: `master_fd` is a freshly opened valid descriptor.
        let master = unsafe { File::from_raw_fd(master_fd) };
        assert_eq!(unsafe { libc::grantpt(master.as_raw_fd()) }, 0);
        assert_eq!(unsafe { libc::unlockpt(master.as_raw_fd()) }, 0);

        let slave_name = unsafe {
            let ptr = libc::ptsname(master.as_raw_fd());
            assert!(!ptr.is_null());
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        };
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_name)
            .unwrap();
        let slave_fd = slave.as_raw_fd();
        assert!(fd_is_tty(slave_fd));

        // 1. Verify TIOCSWINSZ -> ioctl_winsize exact dimension roundtrip.
        let ws = libc::winsize {
            ws_row: 43,
            ws_col: 132,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws) },
            0
        );
        assert_eq!(ioctl_winsize(slave_fd), Some((132, 43)));

        // 2. Verify raw mode clears canonical/echo/signal flags and sets VMIN=1, VTIME=0.
        let before = tcgetattr_retry(slave_fd).unwrap();
        let saved = enable_raw_mode_on_fd(slave_fd).unwrap();
        assert_eq!(before.c_lflag, saved.c_lflag);

        let during = tcgetattr_retry(slave_fd).unwrap();
        assert_eq!(during.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG), 0);
        assert_eq!(during.c_iflag & (libc::IXON | libc::ICRNL), 0);
        assert_eq!(during.c_oflag & libc::OPOST, 0);
        assert_eq!(during.c_cc[libc::VMIN], 1);
        assert_eq!(during.c_cc[libc::VTIME], 0);

        // 3. Verify restore returns original termios bit-for-bit.
        tcsetattr_retry(slave_fd, &saved).unwrap();
        let after = tcgetattr_retry(slave_fd).unwrap();
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        assert_eq!(after.c_cflag, before.c_cflag);
        assert_eq!(after.c_lflag, before.c_lflag);
        assert_eq!(after.c_cc, before.c_cc);
    }
}
