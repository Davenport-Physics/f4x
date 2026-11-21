//! The f4x service table: what the f4x agent offers plugins across the DLL boundary.
//!
//! Rust has no stable ABI, so everything that must be process-unique (logging, the test
//! registry, the main-thread queue) lives in the agent and is reached through this
//! `#[repr(C)]` table. Rules: append-only; readers check `struct_size` before touching a
//! field added after version 1.
//!
//! Discovery:
//! - Host `f4x`: the runtime calls each plugin's `f4x_plugin_load(*const Service)`.
//! - Host `f4se`: the agent runs as the F4SE plugin `f4x_agent.dll`; plugins find it
//!   with `GetProcAddress(GetModuleHandle("f4x_agent.dll"), "f4x_service")`.
//! Both modules export `f4x_service() -> *const Service`.

use core::ffi::{CStr, c_void};

pub const SERVICE_EXPORT: &CStr = c"f4x_service";
pub const PLUGIN_LOAD_EXPORT: &CStr = c"f4x_plugin_load";
/// Module names that may export [`SERVICE_EXPORT`].
pub const SERVICE_MODULES: [&CStr; 2] = [c"f4x_runtime.dll", c"f4x_agent.dll"];

pub type ServiceFn = unsafe extern "C" fn() -> *const Service;
pub type PluginLoadFn = unsafe extern "C" fn(service: *const Service) -> bool;

pub const SERVICE_VERSION: u32 = 1;

/// A borrowed UTF-8 string.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

// SAFETY: a `Str` is an immutable view; the service table only holds `Str`s that point
// to `'static` data.
unsafe impl Send for Str {}
// SAFETY: as above.
unsafe impl Sync for Str {}

impl Str {
    pub const EMPTY: Str = Str { ptr: core::ptr::null(), len: 0 };

    #[must_use]
    pub const fn new(s: &str) -> Str {
        Str { ptr: s.as_ptr(), len: s.len() }
    }

    /// # Safety
    /// `ptr..ptr+len` must be valid UTF-8 for `'a`.
    #[must_use]
    pub unsafe fn as_str<'a>(self) -> &'a str {
        if self.ptr.is_null() {
            return "";
        }
        // SAFETY: guaranteed by the caller.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.ptr, self.len)) }
    }
}

/// Log levels (same numbering as the `log` crate).
pub mod level {
    pub const ERROR: u32 = 1;
    pub const WARN: u32 = 2;
    pub const INFO: u32 = 3;
    pub const DEBUG: u32 = 4;
    pub const TRACE: u32 = 5;
}

/// Test poll results.
pub mod test_status {
    pub const PENDING: u32 = 0;
    pub const PASSED: u32 = 1;
    pub const FAILED: u32 = 2;
    pub const PANICKED: u32 = 3;
}

/// One registered test. The plugin owns the state; the agent drives it from the
/// main-thread pump: `start` once, `poll` once per main-loop iteration until it is no
/// longer pending, `message` for the outcome text, then `drop`.
#[repr(C)]
pub struct TestVtbl {
    pub start: unsafe extern "C" fn(user: *mut c_void) -> *mut c_void,
    pub poll: unsafe extern "C" fn(instance: *mut c_void) -> u32,
    /// Writes the failure/panic message (valid until `drop`), or an empty string.
    pub message: unsafe extern "C" fn(instance: *mut c_void, out: *mut Str),
    pub drop: unsafe extern "C" fn(instance: *mut c_void),
}

#[repr(C)]
pub struct Service {
    pub struct_size: u32,
    pub version: u32,
    /// `"f4x"` or `"f4se"`.
    pub host: Str,
    pub log: unsafe extern "C" fn(level: u32, plugin: Str, target: Str, message: Str),
    pub register_test:
        unsafe extern "C" fn(plugin: Str, name: Str, vtbl: *const TestVtbl, user: *mut c_void) -> bool,
    /// Frames presented so far.
    pub frames: unsafe extern "C" fn() -> u64,
    /// Main-loop iterations so far.
    pub pumps: unsafe extern "C" fn() -> u64,
    /// Run `job(user)` on the game's main thread at the next main-loop iteration.
    pub run_on_main: unsafe extern "C" fn(job: unsafe extern "C" fn(*mut c_void), user: *mut c_void),
}

const _: () = {
    assert!(core::mem::size_of::<Str>() == 16);
    assert!(core::mem::offset_of!(Service, log) == 0x18);
};
