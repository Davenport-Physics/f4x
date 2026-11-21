//! The f4x runtime: injected into `Fallout4.exe` by `f4x_loader`.
//!
//! - Hosts the f4x agent (control server, logs, crashes, frames, tests).
//! - Hosts F4SE plugins (`f4se`): loaded on an init thread before the game's main thread
//!   first runs. `f4x_loader` waits for `F4X_INIT_EVENT` before resuming it.
//! - Loads f4x-native plugins listed in `F4X_PLUGINS` at the first main-loop iteration.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod f4se;
#[cfg(windows)]
mod plugins;

#[cfg(windows)]
mod entry {
    use std::ffi::c_void;

    use f4x_abi::service::Service;
    use windows_sys::Win32::Foundation::{HMODULE, TRUE};
    use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
    use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
    use windows_sys::core::BOOL;

    static SERVICE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

    /// # Safety
    /// Called by the Windows loader only.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
        if reason == DLL_PROCESS_ATTACH {
            // SAFETY: our own module handle, from the loader.
            unsafe { DisableThreadLibraryCalls(module) };
            // Never let a panic cross into the loader.
            let _ = std::panic::catch_unwind(|| {
                let service = f4x_agent::attach(f4x_agent::AttachOptions { host: "f4x", on_main_loop: Some(on_main_loop) });
                let _ = SERVICE.set(service as *const Service as usize);
                f4x_agent::pump::add_ticker(crate::f4se::tick);
                // Load F4SE plugins once the loader lock is released (this thread starts
                // after LoadLibrary returns), then let the loader resume the game.
                let _ = std::thread::Builder::new().name("f4x-init".into()).spawn(|| {
                    let _ = std::panic::catch_unwind(crate::f4se::load_all);
                    signal_init_done();
                });
            });
        }
        TRUE
    }

    fn on_main_loop() {
        crate::plugins::load_from_env();
    }

    /// Tell `f4x_loader` (waiting on `F4X_INIT_EVENT`) that plugins are loaded.
    fn signal_init_done() {
        use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
        let Some(name) = std::env::var_os("F4X_INIT_EVENT") else { return };
        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name.as_os_str()).chain(Some(0)).collect();
        // SAFETY: opening a named event the loader created; NUL-terminated name.
        unsafe {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr());
            if event.is_null() {
                log::warn!(target: "f4x", "init event not found; the loader may have stopped waiting");
                return;
            }
            SetEvent(event);
            windows_sys::Win32::Foundation::CloseHandle(event);
        }
        log::info!(target: "f4x", "init done; the game's main thread may run");
    }

    /// The f4x service table (`f4x_abi::service`).
    #[unsafe(no_mangle)]
    pub extern "C" fn f4x_service() -> *const Service {
        SERVICE.get().map_or(std::ptr::null(), |&p| p as *const Service)
    }
}
