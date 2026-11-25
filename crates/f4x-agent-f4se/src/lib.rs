//! `f4x_agent.dll`: the f4x agent as an ordinary F4SE plugin.
//!
//! Lets the same control server, logs, crash capture and in-game tests run under real
//! F4SE, so every test can be run against both hosts and the results diffed.
//!
//! Old-gen F4SE (0.6.x) calls `F4SEPlugin_Query` then `F4SEPlugin_Load`. Next-gen F4SE
//! (0.7.x) reads the exported `F4SEPlugin_Version` data instead (built with `ng`).
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod plugin {
    use std::sync::OnceLock;

    use f4x_abi::f4se::{F4seInterface, PluginInfo};
    use f4x_abi::service::Service;

    static SERVICE: OnceLock<usize> = OnceLock::new();

    /// Next-gen declaration: addresses come from f4x's own signature recipes; structure
    /// layouts are the 1.11.240 tables (1.11.137+ generation).
    #[cfg(feature = "ng")]
    #[unsafe(no_mangle)]
    #[used]
    #[allow(non_upper_case_globals)]
    pub static F4SEPlugin_Version: f4x_abi::f4se::PluginVersionData = {
        use f4x_abi::f4se::{PluginVersionData, address_independence, structure_independence};
        use f4x_abi::version::{PackedVersion, runtime};
        PluginVersionData::declare(
            "f4x_agent",
            "f4x",
            PackedVersion::new(0, 0, 1, 0),
            address_independence::SIGNATURES,
            structure_independence::LAYOUT_1_11_137,
            &[runtime::V1_11_240],
            PackedVersion(0),
        )
    };

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
