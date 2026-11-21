//! `f4x_agent.dll`: the f4x agent as an ordinary F4SE plugin.
//!
//! Lets the same control server, logs, crash capture and in-game tests run under real
//! F4SE, so every test can be run against both hosts and the results diffed.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod plugin {
    use std::sync::OnceLock;

    use f4x_abi::f4se::{F4seInterface, PluginInfo};
    use f4x_abi::service::Service;

    static SERVICE: OnceLock<usize> = OnceLock::new();

    /// # Safety
    /// Called by F4SE with valid pointers.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn F4SEPlugin_Query(_f4se: *const F4seInterface, info: *mut PluginInfo) -> bool {
        // SAFETY: host-provided pointer.
        unsafe {
            (*info).info_version = PluginInfo::VERSION;
            (*info).name = c"f4x_agent".as_ptr();
            (*info).version = 1;
        }
        true
    }

    /// # Safety
    /// Called by F4SE with a valid interface.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn F4SEPlugin_Load(_f4se: *const F4seInterface) -> bool {
        std::panic::catch_unwind(|| {
            let service = f4x_agent::attach(f4x_agent::AttachOptions { host: "f4se", on_main_loop: None });
            let _ = SERVICE.set(service as *const Service as usize);
            true
        })
        .unwrap_or(false)
    }

    /// The f4x service table (`f4x_abi::service`).
    #[unsafe(no_mangle)]
    pub extern "C" fn f4x_service() -> *const Service {
        SERVICE.get().map_or(std::ptr::null(), |&p| p as *const Service)
    }
}
