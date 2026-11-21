//! Phase 1 plugin loading: f4x-native plugins from `F4X_PLUGINS` (';'-separated
//! paths), loaded on the main thread at the first main-loop iteration. Each plugin's
//! `f4x_plugin_load(*const Service)` export is called.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use f4x_abi::service::{PLUGIN_LOAD_EXPORT, PluginLoadFn};
use f4x_proto::PluginState;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub fn load_from_env() {
    let Some(list) = std::env::var_os("F4X_PLUGINS") else { return };
    let Some(service) = f4x_agent::service::get() else { return };
    let mut states = Vec::new();
    for path in list.to_string_lossy().split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned();
        let mut state = PluginState { name: name.clone(), path: path.to_owned(), loaded: false, error: None };
        let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
        // SAFETY: loading a plugin DLL the launcher asked for, on the main thread.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            // SAFETY: trivially safe.
            state.error = Some(format!("LoadLibrary failed: error {}", unsafe { GetLastError() }));
        } else {
            // SAFETY: valid module, NUL-terminated name.
            match unsafe { GetProcAddress(module, PLUGIN_LOAD_EXPORT.as_ptr().cast()) } {
                None => state.error = Some("no f4x_plugin_load export".into()),
                Some(load) => {
                    // SAFETY: the export has the PluginLoadFn signature by contract.
                    let load: PluginLoadFn = unsafe { std::mem::transmute(load) };
                    let ok = std::panic::catch_unwind(|| unsafe { load(service) }).unwrap_or(false);
                    state.loaded = ok;
                    if !ok {
                        state.error = Some("f4x_plugin_load returned false".into());
                    }
                }
            }
        }
        match &state.error {
            None => log::info!(target: "f4x::plugins", "loaded {name}"),
            Some(error) => log::error!(target: "f4x::plugins", "{name}: {error}"),
        }
        states.push(state);
    }
    if let Ok(mut plugins) = f4x_agent::PLUGINS.lock() {
        plugins.extend(states);
    }
}
