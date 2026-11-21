//! The f4x runtime: injected into `Fallout4.exe` by `f4x_loader`.
//!
//! Phase 1 scope: host the f4x agent (control server, logs, crashes, frames, tests) and
//! load f4x-native plugins listed in `F4X_PLUGINS`. The F4SE-compatible plugin host
//! arrives in Phase 3.
#![cfg_attr(not(windows), allow(dead_code))]

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
                let service = f4x_agent::attach(f4x_agent::AttachOptions {
                    host: "f4x",
                    on_main_loop: Some(crate::plugins::load_from_env),
                });
                let _ = SERVICE.set(service as *const Service as usize);
            });
        }
        TRUE
    }

    /// The f4x service table (`f4x_abi::service`).
    #[unsafe(no_mangle)]
    pub extern "C" fn f4x_service() -> *const Service {
        SERVICE.get().map_or(std::ptr::null(), |&p| p as *const Service)
    }
}
