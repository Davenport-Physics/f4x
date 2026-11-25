//! The f4x runtime: injected into `Fallout4.exe` by `f4x_loader`.
//!
//! - Hosts the f4x agent (control server, logs, crashes, frames, tests).
//! - Hosts F4SE plugins (`f4se`): loaded on an init thread before the game's main thread
//!   first runs. `f4x_loader` waits for `F4X_INIT_EVENT` before resuming it. For a
//!   DRM-packed exe (Steam builds) everything starts later, just before `WinMain`, once
//!   the code is decrypted (`bootstrap`).
//! - Loads f4x-native plugins listed in `F4X_PLUGINS` at the first main-loop iteration.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod bootstrap;
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
                if crate::bootstrap::code_is_packed() {
                    // Steam's DRM stub has not decrypted the code yet: start just before
                    // WinMain instead (on the main thread), and let the game run to there.
                    // Next-gen starts earlier, at the C initializers, for plugins' Preload.
                    #[cfg(feature = "ng")]
                    {
                        let preload = crate::bootstrap::defer_until_c_initializers(preload_packed);
                        let _ = PRELOAD_HOOK.set(preload.err());
                    }
                    let deferred = crate::bootstrap::defer_until_crt_startup(start_packed);
                    signal_init_done();
                    if let Err(error) = deferred {
                        // No hook point: start now; hooks into the code will not take.
                        attach();
                        load_plugins();
                        log::error!(target: "f4x", "packed exe but no startup hook ({error}); started early");
                    }
                    return;
                }
                attach();
                // Load F4SE plugins once the loader lock is released (this thread starts
                // after LoadLibrary returns), then let the loader resume the game.
                let _ = std::thread::Builder::new().name("f4x-init".into()).spawn(|| {
                    let _ = std::panic::catch_unwind(load_plugins);
                    signal_init_done();
                });
            });
        }
        TRUE
    }

    fn attach() {
        let service = f4x_agent::attach(f4x_agent::AttachOptions { host: "f4x", on_main_loop: Some(on_main_loop) });
        let _ = SERVICE.set(service as *const Service as usize);
        if !generation_ok() {
            return;
        }
        f4x_agent::pump::add_ticker(crate::f4se::tick);
    }

    /// This runtime's tables are for one game generation; on the other it must not touch
    /// the engine (the control server still runs, so the mismatch is visible).
    fn generation_ok() -> bool {
        let ok = f4x_engine::game::generation_matches();
        if !ok {
            log::error!(
                target: "f4x",
                "this f4x runtime is built for {} but the game is {:?}; F4SE plugins and natives stay off (use the {} build)",
                if f4x_engine::NG { "next-gen" } else { "1.10.163" },
                f4x_engine::game::running_generation(),
                if f4x_engine::NG { "1.10.163" } else { "next-gen (`--features ng`)" },
            );
        }
        ok
    }

    fn load_plugins() {
        if generation_ok() {
            crate::f4se::load_all();
        }
    }

    /// The start for a DRM-packed exe, on the main thread before `WinMain`: the code is
    /// decrypted and nothing has patched it yet, so keep a copy as the unpatched
    /// reference, then start as usual and load plugins synchronously.
    fn start_packed() {
        attach_packed("before WinMain");
        load_plugins();
    }

    /// Capture the unpatched code and attach, once (next-gen may do it at the C
    /// initializers already).
    fn attach_packed(when: &str) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let captured = f4x_engine::address::capture_reference_text();
            attach();
            log::info!(target: "f4x", "DRM-packed exe: started {when} (reference code captured: {captured})");
            #[cfg(feature = "ng")]
            if let Some(Some(error)) = PRELOAD_HOOK.get() {
                log::warn!(target: "f4x", "no hook at the C initializers ({error}); plugins preload just before WinMain");
            }
        });
    }

    /// Next-gen, DRM-packed: why the `_initterm_e` hook failed (`None`: it is in place).
    #[cfg(feature = "ng")]
    static PRELOAD_HOOK: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

    /// Next-gen, DRM-packed: at the C initializers (`_initterm_e`), start and run the
    /// plugins' `F4SEPlugin_Preload`; their `F4SEPlugin_Load` follows before `WinMain`.
    #[cfg(feature = "ng")]
    fn preload_packed() {
        attach_packed("at the C initializers");
        if generation_ok() {
            crate::f4se::preload_all();
        }
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
