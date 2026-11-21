//! The f4x runtime: injected into `Fallout4.exe` by `f4x_loader`.
//!
//! Spike S1 scope: prove injection, a main-loop hook and the control channel work under
//! Proton. `DllMain` only installs an import-table hook and starts the control thread;
//! everything else happens later on the game's own threads.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod iat;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod spike;

#[cfg(windows)]
mod entry {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{HMODULE, TRUE};
    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
    use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

    /// # Safety
    /// Called by the Windows loader only.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
        if reason == DLL_PROCESS_ATTACH {
            // SAFETY: `module` is our own module handle, supplied by the loader.
            unsafe { DisableThreadLibraryCalls(module) };
            // Never let a panic cross into the loader.
            let _ = std::panic::catch_unwind(crate::spike::attach);
        }
        TRUE
    }
}
