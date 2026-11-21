//! Session time and the frame/pump counters.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Frames presented (D3D11 `Present` calls on the game's swap chain).
pub static FRAMES: AtomicU64 = AtomicU64::new(0);
/// Main-loop iterations (the main-thread pump).
pub static PUMPS: AtomicU64 = AtomicU64::new(0);
/// The game's main thread, once the pump has run (0 before).
pub static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

pub fn start() {
    let _ = START.get_or_init(Instant::now);
}

/// Milliseconds since the agent started, with 0.1 ms resolution.
pub fn t_ms() -> f64 {
    let elapsed = START.get_or_init(Instant::now).elapsed();
    (elapsed.as_secs_f64() * 10_000.0).round() / 10.0
}

pub fn uptime_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn frames() -> u64 {
    FRAMES.load(Relaxed)
}

pub fn pumps() -> u64 {
    PUMPS.load(Relaxed)
}

/// The OS thread id of the caller.
pub fn thread_id() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: trivially safe.
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    }
    #[cfg(not(windows))]
    {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        hasher.finish() as u32
    }
}

pub fn on_main_thread() -> bool {
    let main = MAIN_THREAD.load(Relaxed);
    main != 0 && main == thread_id()
}
