//! When the game's code may be patched.
//!
//! A plain exe (GOG builds) can be patched as soon as the runtime is injected: its code
//! in memory is the code on disk. A Steam build carries Steam's DRM stub instead: the
//! exe's entry point is in a `.bind` section, and `.text` stays encrypted (on disk and in
//! memory) until the stub has run and jumped to the C runtime's startup code. Anything
//! patched before that is lost (or garbled) when the stub decrypts over it.
//!
//! For such an exe the runtime waits: it hooks the import that the C runtime's startup
//! calls last before `WinMain` (all static initialisers have run; the game has not
//! started), and starts there, on the game's main thread. That point is the same in both
//! C runtimes the game has shipped with: `__crtGetShowWindowMode` (MSVCR110, 1.10.163;
//! the technique of CommonLibF4's `F4SEStub/steam_loader`, MIT) and
//! `_get_narrow_winmain_command_line` (the UCRT, next-gen builds; our own reading of the
//! import table).
//!
//! Next-gen plugins' `F4SEPlugin_Preload` runs earlier, inside the CRT's `_initterm_e`
//! (the C initializers: the code is decrypted, the C++ static initializers in `_initterm`
//! have not run). That is where F4SE 0.7.9 calls it (black-box: the return address of its
//! preload call is the instruction after `call _initterm_e` in the CRT's startup).

use std::ffi::c_char;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use f4x_pe::{Image, MappedImage};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

fn base() -> usize {
    // SAFETY: GetModuleHandleW(null) is the game image.
    unsafe { GetModuleHandleW(std::ptr::null()) as usize }
}

/// Is the game's code still encrypted by a DRM stub (entry point outside `.text`, in a
/// `.bind` section)?
pub fn code_is_packed() -> bool {
    let base = base();
    // SAFETY: the game image is mapped for the process lifetime.
    let Some(image) = (unsafe { MappedImage::from_base(base) }) else { return false };
    // SAFETY: inside the image's headers (checked by `from_base`).
    let entry = unsafe {
        let nt = (base as *const u8).add((base as *const u8).add(0x3C).cast::<i32>().read_unaligned() as usize);
        nt.add(24 + 16).cast::<u32>().read_unaligned()
    };
    let sections = image.sections();
    let in_bind = sections.iter().any(|s| s.name == ".bind" && s.contains(entry));
    let in_text = sections.iter().any(|s| s.name == ".text" && s.contains(entry));
    in_bind && !in_text
}

static START: OnceLock<fn()> = OnceLock::new();
static REAL_SHOW_WINDOW_MODE: AtomicUsize = AtomicUsize::new(0);
static REAL_WINMAIN_COMMAND_LINE: AtomicUsize = AtomicUsize::new(0);

fn run_start_once() {
    static RAN: std::sync::Once = std::sync::Once::new();
    RAN.call_once(|| {
        if let Some(start) = START.get() {
            let _ = std::panic::catch_unwind(start);
        }
    });
}

unsafe extern "C" fn show_window_mode_hook() -> i32 {
    run_start_once();
    let real = REAL_SHOW_WINDOW_MODE.load(Ordering::Acquire);
    // SAFETY: the original import, saved when the hook was installed.
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> i32>(real)() }
}

unsafe extern "C" fn winmain_command_line_hook() -> *mut c_char {
    run_start_once();
    let real = REAL_WINMAIN_COMMAND_LINE.load(Ordering::Acquire);
    // SAFETY: as above.
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> *mut c_char>(real)() }
}

#[cfg(feature = "ng")]
static PRELOAD: OnceLock<fn()> = OnceLock::new();
#[cfg(feature = "ng")]
static REAL_INITTERM_E: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "ng")]
unsafe extern "C" fn initterm_e_hook(first: *const usize, last: *const usize) -> i32 {
    static RAN: std::sync::Once = std::sync::Once::new();
    RAN.call_once(|| {
        if let Some(preload) = PRELOAD.get() {
            let _ = std::panic::catch_unwind(preload);
        }
    });
    let real = REAL_INITTERM_E.load(Ordering::Acquire);
    // SAFETY: the original import, saved when the hook was installed.
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*const usize, *const usize) -> i32>(real)(first, last) }
}

/// Next-gen: run `preload` on the game's main thread inside the C runtime's
/// `_initterm_e`, before the C++ static initializers (see the module docs).
#[cfg(feature = "ng")]
pub fn defer_until_c_initializers(preload: fn()) -> Result<(), String> {
    PRELOAD.set(preload).map_err(|_| "already deferred".to_owned())?;
    let base = base() as *mut u8;
    // SAFETY: the game image's imports are bound by the time the runtime is injected.
    let real = unsafe {
        f4x_agent::hooks::iat_hook(base, "api-ms-win-crt-runtime-l1-1-0.dll", "_initterm_e", initterm_e_hook as *const () as usize)
    }?;
    REAL_INITTERM_E.store(real, Ordering::Release);
    Ok(())
}

/// Run `start` on the game's main thread just before `WinMain` (see the module docs).
/// Returns which import carries the hook.
pub fn defer_until_crt_startup(start: fn()) -> Result<&'static str, String> {
    START.set(start).map_err(|_| "already deferred".to_owned())?;
    let base = base() as *mut u8;
    let mut errors = Vec::new();
    // SAFETY: the game image's imports are bound by the time the runtime is injected.
    match unsafe {
        f4x_agent::hooks::iat_hook(base, "api-ms-win-crt-runtime-l1-1-0.dll", "_get_narrow_winmain_command_line", winmain_command_line_hook as *const () as usize)
    } {
        Ok(real) => {
            REAL_WINMAIN_COMMAND_LINE.store(real, Ordering::Release);
            return Ok("_get_narrow_winmain_command_line");
        }
        Err(error) => errors.push(error),
    }
    // SAFETY: as above.
    match unsafe { f4x_agent::hooks::iat_hook(base, "MSVCR110.dll", "__crtGetShowWindowMode", show_window_mode_hook as *const () as usize) } {
        Ok(real) => {
            REAL_SHOW_WINDOW_MODE.store(real, Ordering::Release);
            Ok("__crtGetShowWindowMode")
        }
        Err(error) => {
            errors.push(error);
            Err(errors.join("; "))
        }
    }
}
