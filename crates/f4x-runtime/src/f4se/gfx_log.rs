//! Scaleform's own log output (`bEnableGFXLog=1` under `[Interface]` in
//! `Data\F4SE\f4se.ini`, the setting F4SE documents in its changelog 0.1.4).
//!
//! The game silences Scaleform: the BSScaleformManager constructor gives the GFx loader a
//! log state (`State::StateType::kLog` = 2) whose log is a `LoggingDisabler` (vtable slot
//! 1, `LogMessageVarg`, is `ret`; our disassembly of 1.10.163, 0x2110430 and 0x212a0e0).
//! The log state keeps its `Ptr<GFx::Log>` at +0x20 (its constructor, 0x2129bb0). NG
//! 1.11.240 is the same except the silencer's class, `DejaGFxLogger` (slot 1 is `ret 0`):
//! its `SetLog` (0x1b30e90) builds a 0x28-byte log state with the log at +0x20 and calls
//! `SetState(2, ...)` (next-gen fork `GFx_LogState.h` agrees). With
//! the setting on, f4x points that at its own log, which writes the messages to f4x's
//! log (target `f4x::gfx`), one record per line of text.
//!
//! Black-box (F4SE 0.6.23): with the setting on, F4SE swaps in its own log object the
//! same way; messages sent to it (`ui.gfxlog.emit`) appear verbatim in `f4se.log`, with
//! no prefix and no newline added, whatever the message id. No Scaleform message came
//! from the game itself in our scenarios (main menu, in game, failing AS3 calls). This runs inside the constructor (see `translations`), before
//! any movie exists. f4x only reads F4SE's ini; it never writes to F4SE's folders.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::Relaxed};

use f4x_engine::translation::{STATE_LOG, loader_state};

/// `LogState::pLog` (a `Ptr<Log>`).
const LOG_STATE_LOG: usize = 0x20;

static ENABLED: AtomicBool = AtomicBool::new(false);
static MESSAGES: AtomicU64 = AtomicU64::new(0);
/// Text of the current, unfinished line.
static PENDING: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// A line longer than this is written out unfinished.
const PENDING_MAX: usize = 16 * 1024;

/// A `GFx::Log`: vtable (destructor, `LogMessageVarg`), then the reference count.
#[repr(C)]
struct Log {
    vtable: &'static LogVtable,
    /// Atomic: the engine adds and drops references (so the static is writable).
    references: AtomicI32,
    _pad: u32,
}

// SAFETY: the vtable pointer is never changed; the engine only touches the reference
// count, atomically.
unsafe impl Sync for Log {}

/// `GFx::Log`'s virtual functions: the destructor, then `LogMessageVarg`.
#[repr(C)]
struct LogVtable {
    destroy: unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void,
    log_message: unsafe extern "C" fn(*mut c_void, i32, *const c_char, *mut c_void),
}

static VTABLE: LogVtable = LogVtable { destroy, log_message };

/// Never freed: it lives for the process, with a count that cannot reach zero.
unsafe extern "C" fn destroy(this: *mut c_void, _flags: u32) -> *mut c_void {
    this
}

unsafe extern "C" {
    /// The UCRT's printf engine (`vsnprintf` is an inline wrapper around it).
    fn __stdio_common_vsprintf(
        options: u64,
        buffer: *mut c_char,
        count: usize,
        format: *const c_char,
        locale: *mut c_void,
        args: *mut c_void,
    ) -> i32;
}

/// `Log::LogMessageVarg(LogMessageId, const char* format, va_list)`.
unsafe extern "C" fn log_message(_this: *mut c_void, id: i32, format: *const c_char, args: *mut c_void) {
    if format.is_null() {
        return;
    }
    let mut buffer = [0u8; 4096];
    // SAFETY: the engine's format and va_list; the UCRT truncates to the buffer (option
    // 1: legacy vsprintf NUL termination).
    let written = unsafe {
        __stdio_common_vsprintf(
            1,
            buffer.as_mut_ptr().cast(),
            buffer.len() - 1,
            format,
            std::ptr::null_mut(),
            args,
        )
    };
    let len = if written < 0 {
        buffer.iter().position(|&b| b == 0).unwrap_or(0)
    } else {
        (written as usize).min(buffer.len() - 1)
    };
    MESSAGES.fetch_add(1, Relaxed);
    let _ = id; // F4SE writes every message the same way, whatever its id (black-box)
    // F4SE appends the text to its log as is (no prefix, no added newline), so messages
    // without a trailing newline run into the next one. f4x keeps those line boundaries:
    // each complete line becomes one record of f4x's log.
    let Ok(mut pending) = PENDING.lock() else { return };
    pending.push_str(&String::from_utf8_lossy(&buffer[..len]));
    while let Some(end) = pending.find('\n') {
        let line: String = pending.drain(..=end).collect();
        log::info!(target: "f4x::gfx", "{}", line.trim_end_matches(['\r', '\n']));
    }
    if pending.len() > PENDING_MAX {
        log::info!(target: "f4x::gfx", "{pending}");
        pending.clear();
    }
}

/// Whether F4SE's ini (`<game>\Data\F4SE\f4se.ini`) has `bEnableGFXLog=1` under
/// `[Interface]` (read once, at install).
fn setting_enabled() -> bool {
    let Some(ini) = super::translations::game_dir().map(|d| d.join("Data").join("F4SE").join("f4se.ini"))
    else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&ini) else { return false };
    let mut section = String::new();
    let mut enabled = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[') {
            section = name.trim_end_matches(']').trim().to_ascii_lowercase();
        } else if section == "interface"
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("bEnableGFXLog")
        {
            enabled = value.trim().parse::<i64>().is_ok_and(|v| v != 0);
        }
    }
    enabled
}

pub(super) fn install() {
    ENABLED.store(setting_enabled(), Relaxed);
    if ENABLED.load(Relaxed) {
        log::info!(target: "f4x::gfx", "bEnableGFXLog=1: Scaleform log output goes to f4x's log");
    }
}

/// Called inside the BSScaleformManager constructor, once the loader has its log state.
pub(super) fn on_scaleform_manager(loader: *mut u8) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    let Some(state) = loader_state(loader, STATE_LOG) else {
        log::warn!(target: "f4x::gfx", "the Scaleform loader has no log state");
        return;
    };
    static LOG: Log = Log { vtable: &VTABLE, references: AtomicI32::new(i32::MAX / 2), _pad: 0 };
    // SAFETY: the loader's LogState; its Ptr<Log> at +0x20. The disabler it held keeps
    // its reference (leaked, harmless); ours is static.
    unsafe {
        if !f4x_engine::mem::readable(state as usize, LOG_STATE_LOG + 8) {
            return;
        }
        state.add(LOG_STATE_LOG).cast::<*const Log>().write(&raw const LOG);
    }
    log::info!(target: "f4x::gfx", "Scaleform log enabled");
}
