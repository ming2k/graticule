//! Dedicated background TTY input reader thread.
//!
//! Enforces the single-reader invariant: only one thread ever reads from the
//! TTY descriptor, routing user [`Event`]s to the application sink and routing
//! protocol probe replies (`CSI ? ... u`, `CSI ? ... c`) to [`KeyboardProbe`].

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use graticule_vt::csi::PROBE_KITTY_AND_DA1;
use graticule_vt::event::Event;

use crate::sys;

/// Shared synchronization state between [`InputReader`] and the off-frame
/// keyboard enhancement negotiation probe.
#[derive(Debug, Default)]
pub struct KeyboardProbe {
    state: Mutex<ProbeState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct ProbeState {
    supported: Option<bool>,
    da1_seen: bool,
}

impl KeyboardProbe {
    /// Record that the terminal replied with Kitty enhancement flags.
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn mark_supported(&self, supported: bool) {
        if let Ok(mut guard) = self.state.lock() {
            guard.supported = Some(supported);
            self.cv.notify_all();
        }
    }

    /// Record that Primary Device Attributes arrived. Since `ESC[?u` is sent
    /// ahead of `ESC[c`, a DA1 reply with no prior `ESC[?...u` means Kitty
    /// keyboard enhancement is unsupported, letting us return immediately
    /// instead of waiting out the full timeout.
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn mark_da1_seen(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.da1_seen = true;
            if guard.supported.is_none() {
                guard.supported = Some(false);
            }
            self.cv.notify_all();
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn reset(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.supported = None;
            guard.da1_seen = false;
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn wait_for_test(&self, timeout: Duration) -> bool {
        self.wait(timeout)
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let Ok(mut guard) = self.state.lock() else {
            return false;
        };
        while guard.supported.is_none() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if remaining.is_zero() {
                break;
            }
            let Ok((next, res)) = self.cv.wait_timeout(guard, remaining) else {
                return false;
            };
            guard = next;
            if res.timed_out() {
                break;
            }
        }
        guard.supported.unwrap_or(false)
    }
}

/// Cloneable handle for probing terminal capabilities off the reader thread
/// without borrowing [`InputReader`].
#[derive(Clone)]
pub struct ProbeHandle {
    probe: Arc<KeyboardProbe>,
    cached_kitty_support: Arc<OnceLock<bool>>,
}

impl ProbeHandle {
    /// Probe whether the terminal supports the Kitty keyboard protocol.
    ///
    /// Memoized per [`InputReader`] instance so slow/unresponsive terminals pay
    /// `timeout` at most once. Call off the first-frame render path.
    pub fn probe_keyboard_enhancement(&self, timeout: Duration) -> bool {
        *self.cached_kitty_support.get_or_init(|| {
            #[cfg(unix)]
            {
                self.probe.reset();
                let mut stdout = io::stdout();
                if stdout.write_all(PROBE_KITTY_AND_DA1).is_err() || stdout.flush().is_err() {
                    return false;
                }
                self.probe.wait(timeout)
            }
            #[cfg(windows)]
            {
                let _ = timeout;
                let _ = &self.probe;
                let _ = PROBE_KITTY_AND_DA1;
                sys::probe_keyboard_enhancement_sync()
            }
        })
    }
}

/// Handle owning the dedicated TTY reader thread. Dropping it wakes and stops
/// the background thread cleanly with zero polling delay.
pub struct InputReader {
    shutdown: Arc<AtomicBool>,
    probe: Arc<KeyboardProbe>,
    cached_kitty_support: Arc<OnceLock<bool>>,
    #[cfg(unix)]
    wake_tx: Option<std::os::unix::net::UnixStream>,
    #[cfg(windows)]
    wake: Arc<sys::WakeHandle>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        #[cfg(unix)]
        if let Some(mut tx) = self.wake_tx.take() {
            let _ = tx.write_all(b"q");
            let _ = tx.flush();
        }
        #[cfg(windows)]
        self.wake.wake();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl InputReader {
    /// Spawn a reader thread backed by a standard `mpsc` channel.
    pub fn spawn_channel() -> io::Result<(Self, mpsc::Receiver<Event>)> {
        let (tx, rx) = mpsc::channel();
        let reader = Self::spawn_with(move |ev| tx.send(ev).is_ok())?;
        Ok((reader, rx))
    }

    /// Spawn a reader thread that forwards each decoded [`Event`] to `sink`.
    ///
    /// Returning `false` from `sink` stops the reader thread cleanly.
    pub fn spawn_with<F>(mut sink: F) -> io::Result<Self>
    where
        F: FnMut(Event) -> bool + Send + 'static,
    {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let probe = Arc::new(KeyboardProbe::default());
        let thread_probe = probe.clone();

        #[cfg(unix)]
        let (wake_rx, wake_tx) = std::os::unix::net::UnixStream::pair()?;

        #[cfg(windows)]
        let wake = Arc::new(sys::WakeHandle::new()?);
        #[cfg(windows)]
        let thread_wake = wake.clone();

        let join = std::thread::Builder::new()
            .name("graticule-tty-input".into())
            .spawn(move || {
                #[cfg(unix)]
                let _ = sys::run_event_loop(&mut sink, thread_shutdown, wake_rx, thread_probe);
                #[cfg(windows)]
                {
                    let _ = thread_probe;
                    let _ = sys::run_event_loop(&mut sink, thread_shutdown, thread_wake);
                }
            })?;

        Ok(Self {
            shutdown,
            probe,
            cached_kitty_support: Arc::new(OnceLock::new()),
            #[cfg(unix)]
            wake_tx: Some(wake_tx),
            #[cfg(windows)]
            wake,
            join: Some(join),
        })
    }

    /// Return a cloneable handle that can run off-frame capability probes from
    /// another thread while [`InputReader`] stays owned by the event loop.
    pub fn probe_handle(&self) -> ProbeHandle {
        ProbeHandle {
            probe: self.probe.clone(),
            cached_kitty_support: self.cached_kitty_support.clone(),
        }
    }

    /// Probe whether the terminal supports the Kitty keyboard protocol.
    ///
    /// Memoized per [`InputReader`] instance so slow/unresponsive terminals pay
    /// `timeout` at most once. Call off the first-frame render path.
    pub fn probe_keyboard_enhancement(&self, timeout: Duration) -> bool {
        self.probe_handle().probe_keyboard_enhancement(timeout)
    }
}
