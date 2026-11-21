//! SIGINT/SIGTERM/SIGHUP as a flag, so long-running host commands can stop the game
//! they own instead of dying and leaving it running.
//!
//! Nothing changes until [`install`] is called; afterwards those signals no longer
//! terminate the process, they set [`interrupted`]. Blocking client calls check the flag
//! and return early.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Route SIGINT, SIGTERM and SIGHUP to [`interrupted`].
pub fn install() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: installing a handler that only stores to an atomic.
        unsafe { libc::signal(signal, handler) };
    }
}

/// A handled signal has arrived since [`install`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
