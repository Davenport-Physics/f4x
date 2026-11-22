//! The F4SE-compatible plugin host.
//!
//! ABI: `f4x_abi::f4se` (from CommonLibF4). Behaviour matched to black-box observations
//! of F4SE 0.6.23 (`docs/spikes/S5-f4se-baseline.md`, `baselines/`):
//! - plugins in `Data/F4SE/Plugins/*.dll` load in case-insensitive name order, before the
//!   game's main thread first runs (the loader waits for us), on a non-main thread;
//! - handles count from 1 in load order; `Query` then `Load`; `PostLoad` and
//!   `PostPostLoad` follow immediately on the same thread;
//! - reported versions: F4SE 0.6.23 (`0x00060170`), release index 25;
//! - interfaces 1–5 and 7 (Object, 6, is not offered yet); `RegisterListener` with a
//!   null sender receives plugin messages, not host messages;
//! - `GameDataReady(true)` passes the bool in the pointer (`data = 1`, `data_len = 1`),
//!   followed by `GameLoaded`, once the main menu is up.

mod interfaces;
mod lifecycle;
mod serialization;
mod trampoline;

use std::ffi::{CStr, CString, OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use f4x_abi::f4se::{EXPORT_LOAD, EXPORT_QUERY, LoadFn, PluginInfo, QueryFn, message};
use f4x_proto::PluginState;
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};

pub use interfaces::{dispatch_from_host, interface};

pub(crate) struct Plugin {
    pub handle: u32,
    pub file_name: String,
    pub path: PathBuf,
    /// From `PluginInfo.name`, NUL-terminated (also the message sender name).
    pub name: CString,
    pub version: u32,
    /// Stable `PluginInfo` for `GetPluginInfo`.
    pub info: &'static PluginInfo,
    pub module: usize,
}

// SAFETY: `info` points to a leaked, never-mutated PluginInfo whose name is a leaked
// C string; sharing it across threads is read-only.
unsafe impl Send for Plugin {}

pub(crate) static PLUGINS: Mutex<Vec<Plugin>> = Mutex::new(Vec::new());
/// The plugin whose `Query`/`Load` is running (`u32::MAX`: none).
pub(crate) static CURRENT: AtomicU32 = AtomicU32::new(u32::MAX);
static DATA_READY_SENT: AtomicBool = AtomicBool::new(false);
/// The engine's data-ready call is hooked (the main-menu detector is then unused).
pub(crate) static DATA_READY_HOOKED: AtomicBool = AtomicBool::new(false);

/// `GameDataReady(true)` (bool in the pointer) then `GameLoaded`, once.
pub(crate) fn send_data_ready() {
    if DATA_READY_SENT.swap(true, Relaxed) {
        return;
    }
    log::info!(target: "f4x::f4se", "game data ready: sending GameDataReady + GameLoaded");
    dispatch_from_host(message::GAME_DATA_READY, 1 as *mut c_void, 1);
    dispatch_from_host(message::GAME_LOADED, std::ptr::null_mut(), 0);
}

/// `<game dir>\Data\F4SE\Plugins`.
fn plugin_dir() -> PathBuf {
    let mut buf = [0u16; 1024];
    // SAFETY: valid buffer; null = the game exe.
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let exe = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    exe.parent().unwrap_or(Path::new(".")).join("Data").join("F4SE").join("Plugins")
}

/// Plugins f4x must never load as F4SE plugins.
fn skipped(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    // The f4x agent's F4SE-mode build: the runtime already hosts the agent.
    lower == "f4x_agent.dll" || lower == "f4x_runtime.dll"
}

/// Load every F4SE plugin, then send `PostLoad` and `PostPostLoad`.
pub fn load_all() {
    let dir = plugin_dir();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
            .collect(),
        Err(_) => {
            log::info!(target: "f4x::f4se", "no plugin directory at {}", dir.display());
            Vec::new()
        }
    };
    files.sort_by_key(|p| p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase());
    log::info!(target: "f4x::f4se", "plugin directory {} ({} DLLs)", dir.display(), files.len());

    let mut states = Vec::new();
    let mut next_handle = 1u32;
    for path in files {
        let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if skipped(&file_name) {
            log::info!(target: "f4x::f4se", "skipping {file_name} (f4x's own)");
            continue;
        }
        let handle = next_handle;
        let mut state = PluginState { name: file_name.clone(), path: path.display().to_string(), loaded: false, error: None };
        match load_one(handle, &path, &file_name) {
            Ok((name, version)) => {
                log::info!(target: "f4x::f4se", "plugin {file_name} (handle {handle}, {name} v{version}) loaded");
                state.name = format!("{name} ({file_name})");
                state.loaded = true;
                next_handle += 1;
            }
            Err(error) => {
                log::warn!(target: "f4x::f4se", "plugin {file_name}: {error}");
                state.error = Some(error);
            }
        }
        states.push(state);
    }
    if let Ok(mut plugins) = f4x_agent::PLUGINS.lock() {
        plugins.extend(states);
    }
    lifecycle::install();
    dispatch_from_host(message::POST_LOAD, std::ptr::null_mut(), 0);
    dispatch_from_host(message::POST_POST_LOAD, std::ptr::null_mut(), 0);
}

/// Load one plugin: `Query`, register it (so `GetPluginInfo` and message routing see it
/// during its own `Load`), then `Load`. On failure it is unregistered and unloaded.
fn load_one(handle: u32, path: &Path, file_name: &str) -> Result<(String, u32), String> {
    let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
    // SAFETY: loading a DLL the user installed as an F4SE plugin.
    let module: HMODULE = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        // SAFETY: trivially safe.
        return Err(format!("LoadLibrary failed (error {})", unsafe { GetLastError() }));
    }
    let unload = |reason: String| {
        // SAFETY: the module we just loaded.
        unsafe { FreeLibrary(module) };
        Err(reason)
    };
    // SAFETY: valid module, NUL-terminated names.
    let (query, load) = unsafe {
        (GetProcAddress(module, EXPORT_QUERY.as_ptr().cast()), GetProcAddress(module, EXPORT_LOAD.as_ptr().cast()))
    };
    let (Some(query), Some(load)) = (query, load) else {
        return unload("not an F4SE plugin (no F4SEPlugin_Query/F4SEPlugin_Load)".into());
    };
    // SAFETY: the exports have the F4SE signatures by contract.
    let (query, load): (QueryFn, LoadFn) = unsafe { (std::mem::transmute(query), std::mem::transmute(load)) };

    let info: &'static mut PluginInfo =
        Box::leak(Box::new(PluginInfo { info_version: 0, name: std::ptr::null(), version: 0 }));
    CURRENT.store(handle, Relaxed);
    // SAFETY: calling the plugin's entry point with our interface and a fresh info struct.
    let accepted = unsafe { query(interface(), info) };
    if !accepted {
        CURRENT.store(u32::MAX, Relaxed);
        return unload("rejected by F4SEPlugin_Query".into());
    }
    let name = if info.name.is_null() {
        CString::new(file_name.trim_end_matches(".dll")).unwrap_or_default()
    } else {
        // SAFETY: the plugin's NUL-terminated name, valid while it is loaded.
        unsafe { CStr::from_ptr(info.name) }.to_owned()
    };
    // Make the info self-contained: the name must outlive any buffer of the plugin's.
    let leaked_name: &'static CStr = Box::leak(name.clone().into_boxed_c_str());
    info.name = leaked_name.as_ptr();
    let version = info.version;
    if let Ok(mut plugins) = PLUGINS.lock() {
        plugins.push(Plugin {
            handle,
            file_name: file_name.to_owned(),
            path: path.to_owned(),
            name: name.clone(),
            version,
            info,
            module: module as usize,
        });
    }
    // SAFETY: as above.
    let loaded = unsafe { load(interface()) };
    CURRENT.store(u32::MAX, Relaxed);
    if !loaded {
        if let Ok(mut plugins) = PLUGINS.lock() {
            plugins.retain(|p| p.handle != handle);
        }
        return unload("F4SEPlugin_Load returned false".into());
    }
    Ok((name.to_string_lossy().into_owned(), version))
}

/// Called on every main-loop iteration:
/// - runs plugins' Papyrus registrations once the VM has finished binding the engine's
///   natives (the bind count unchanged for 30 iterations);
/// - sends `GameDataReady(true)` + `GameLoaded` once the data handler exists and the main
///   menu is up (with no loading screen).
pub fn tick() {
    static LAST_NATIVES: AtomicU32 = AtomicU32::new(0);
    static STABLE_FOR: AtomicU32 = AtomicU32::new(0);
    if !interfaces::papyrus_done() && f4x_agent::observe::virtual_machine().is_some() {
        let natives = f4x_agent::observe::native_count() as u32;
        if natives > 0 && LAST_NATIVES.swap(natives, Relaxed) == natives {
            if STABLE_FOR.fetch_add(1, Relaxed) >= 30 {
                register_papyrus();
            }
        } else {
            STABLE_FOR.store(0, Relaxed);
        }
    }
    if DATA_READY_SENT.load(Relaxed) || DATA_READY_HOOKED.load(Relaxed) {
        return;
    }
    let menus = f4x_agent::observe::active_menus();
    if !menus.iter().any(|m| m == "MainMenu") || menus.iter().any(|m| m == "LoadingMenu") {
        return;
    }
    if f4x_engine::DataHandler::get().is_none() {
        return;
    }
    send_data_ready();
}

/// Call the plugins' Papyrus `Register` callbacks with the VM (once, main thread).
pub fn register_papyrus() {
    interfaces::run_papyrus_registrations();
}
