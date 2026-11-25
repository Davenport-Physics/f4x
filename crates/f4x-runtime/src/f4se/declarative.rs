//! Next-gen plugin loading: declarative version data, preload, load.
//!
//! ABI: the next-gen CommonLibF4 fork (`F4SEPlugin_Version` [`PluginVersionData`],
//! `F4SEPlugin_Preload`, `F4SEPlugin_Load`; `f4x_abi::f4se`). Behaviour matched to F4SE
//! 0.7.9 on 1.11.240, observed black-box with `tests/plugins/ng-abi` and its patched copies
//! (`tools/ng_abi.py`, `provenance/ng-plugin-abi.jsonl`):
//! - **check** each DLL in name order: without `F4SEPlugin_Version` it is not loaded
//!   (`F4SEPlugin_Query` is never called, even next to version data); the version data
//!   decides ([`PluginVersionData::compatibility`]); an accepted plugin gets the next
//!   handle (agent included; rejected ones take none);
//! - **preload**: every accepted plugin's `F4SEPlugin_Preload`, in order. F4SE 0.7.9 calls
//!   it on the main thread from inside the game's call to `_initterm_e`, i.e. after the DRM
//!   stub has decrypted the code and before the C++ static initializers (`_initterm`) run
//!   (return address into the CRT's startup, `Fallout4.exe+0x22bf4ff`, the instruction
//!   after `call _initterm_e`); f4x starts there too (`bootstrap.rs`). All interfaces are
//!   offered already (1-5 and 7 here; F4SE also 6). Returning false drops the plugin (its
//!   handle stays used) and unloads it;
//! - **load**: every remaining plugin's `F4SEPlugin_Load`, in order, from inside
//!   `_get_narrow_winmain_command_line` (just before `WinMain`). A plugin without
//!   `F4SEPlugin_Load` stays loaded; returning false drops and unloads it;
//! - `GetPluginInfo` answers from then on; `PostLoad` / `PostPostLoad` follow (`mod.rs`).
//!
//! `F4X_NG_LEGACY_PLUGINS=1` also loads old-gen-shaped plugins (`F4SEPlugin_Query` +
//! `F4SEPlugin_Load`, no version data) the old-gen way, after the check: an f4x extension
//! for plugins whose authors forgot the version data; real F4SE skips them.

use std::ffi::{CString, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;

use f4x_abi::f4se::{EXPORT_LOAD, EXPORT_PRELOAD, EXPORT_QUERY, EXPORT_VERSION, LoadFn, PluginInfo, PluginVersionData, PreloadFn};
use f4x_proto::PluginState;
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use super::interfaces::{F4SE_VERSION, PLUGIN_INFO_READY, runtime_version};
use super::{CURRENT, PLUGINS, Plugin, interface, load_one, skipped};

/// A plugin between the check and the load phase.
enum Pending {
    Declared { handle: u32, module: usize, file_name: String, preload: Option<PreloadFn>, load: Option<LoadFn> },
    /// `F4X_NG_LEGACY_PLUGINS`: loaded the old-gen way in the load phase.
    Legacy { path: PathBuf, file_name: String },
}

struct Phases {
    pending: Vec<Pending>,
    states: Vec<PluginState>,
    next_handle: u32,
}

/// `None` until the check and preload phases ran.
static PHASES: Mutex<Option<Phases>> = Mutex::new(None);

fn legacy_allowed() -> bool {
    std::env::var("F4X_NG_LEGACY_PLUGINS").is_ok_and(|v| v == "1")
}

/// The exports `path` has, read from the file (so a DLL that is not loaded never runs).
fn exports(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let Ok(pe) = f4x_pe::PeFile::parse(&bytes) else { return Vec::new() };
    pe.exports().into_iter().map(|(name, _)| name).collect()
}

fn unload(module: usize) {
    // SAFETY: a module this loader loaded.
    unsafe { FreeLibrary(module as HMODULE) };
}

fn unregister(handle: u32) {
    if let Ok(mut plugins) = PLUGINS.lock() {
        plugins.retain(|p| p.handle != handle);
    }
}

/// Check every plugin's version data and run the accepted ones' `F4SEPlugin_Preload`.
/// Runs once; later calls do nothing.
pub(super) fn check_and_preload(files: Vec<PathBuf>) {
    let Ok(mut phases) = PHASES.lock() else { return };
    if phases.is_some() {
        return;
    }
    let runtime = runtime_version();
    log::info!(target: "f4x::f4se", "next-gen plugin check: runtime {runtime}, reporting F4SE {F4SE_VERSION}");
    let mut done = Phases { pending: Vec::new(), states: Vec::new(), next_handle: 1 };
    for path in files {
        let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if skipped(&file_name) {
            log::info!(target: "f4x::f4se", "skipping {file_name} (f4x's own)");
            continue;
        }
        let mut state = PluginState { name: file_name.clone(), path: path.display().to_string(), loaded: false, error: None };
        match check_one(&path, &file_name, runtime, &mut done.next_handle) {
            Ok(Some(pending)) => done.pending.push(pending),
            Ok(None) => continue,
            Err(error) => {
                log::warn!(target: "f4x::f4se", "plugin {file_name}: {error}");
                state.error = Some(error);
                done.states.push(state);
            }
        }
    }
    preload(&mut done);
    *phases = Some(done);
}

/// The check: `Ok(None)` for DLLs that are not F4SE plugins at all.
fn check_one(path: &Path, file_name: &str, runtime: f4x_abi::version::PackedVersion, next_handle: &mut u32) -> Result<Option<Pending>, String> {
    let names = exports(path);
    let has = |export: &std::ffi::CStr| names.iter().any(|n| n.as_bytes() == export.to_bytes());
    if !has(EXPORT_VERSION) {
        if has(EXPORT_QUERY) && has(EXPORT_LOAD) {
            if legacy_allowed() {
                log::warn!(target: "f4x::f4se", "plugin {file_name}: no version data; loading it the old-gen way (F4X_NG_LEGACY_PLUGINS=1)");
                return Ok(Some(Pending::Legacy { path: path.to_owned(), file_name: file_name.to_owned() }));
            }
            return Err("no F4SEPlugin_Version: an old-gen plugin (F4SE 0.7+ loads only plugins with next-gen version data; \
                        set F4X_NG_LEGACY_PLUGINS=1 to load it anyway)"
                .into());
        }
        if names.iter().any(|n| n.starts_with("F4SEPlugin_")) {
            return Err("no F4SEPlugin_Version export".into());
        }
        log::info!(target: "f4x::f4se", "{file_name}: not an F4SE plugin");
        return Ok(None);
    }
    let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
    // SAFETY: loading a DLL the user installed as an F4SE plugin.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        // SAFETY: trivially safe.
        return Err(format!("LoadLibrary failed (error {})", unsafe { GetLastError() }));
    }
    // SAFETY: valid module, NUL-terminated names.
    let (version, preload, load) = unsafe {
        (
            GetProcAddress(module, EXPORT_VERSION.as_ptr().cast()),
            GetProcAddress(module, EXPORT_PRELOAD.as_ptr().cast()),
            GetProcAddress(module, EXPORT_LOAD.as_ptr().cast()),
        )
    };
    let Some(version) = version else {
        unload(module as usize);
        return Err("no F4SEPlugin_Version export".into());
    };
    // SAFETY: the export is the plugin's PluginVersionData (contract); copied out.
    let data: PluginVersionData = unsafe { std::ptr::read_unaligned(version as *const PluginVersionData) };
    let name = String::from_utf8_lossy(data.name_bytes()).into_owned();
    if let Err(rejection) = data.compatibility(runtime, F4SE_VERSION) {
        unload(module as usize);
        return Err(format!(
            "{name:?} v{} not loaded: {rejection} (declares addresses {:#x}, structures {:#x}, versions [{}], F4SE >= {})",
            data.plugin_version,
            data.address_independence,
            data.structure_independence,
            data.listed_versions().map(|v| v.to_string()).collect::<Vec<_>>().join(", "),
            data.xse_minimum,
        ));
    }
    let handle = *next_handle;
    *next_handle += 1;
    let c_name = CString::new(name.clone()).unwrap_or_default();
    let leaked: &'static std::ffi::CStr = Box::leak(c_name.clone().into_boxed_c_str());
    let info: &'static PluginInfo =
        Box::leak(Box::new(PluginInfo { info_version: PluginInfo::VERSION, name: leaked.as_ptr(), version: data.plugin_version.0 }));
    if let Ok(mut plugins) = PLUGINS.lock() {
        plugins.push(Plugin {
            handle,
            file_name: file_name.to_owned(),
            path: path.to_owned(),
            name: c_name,
            version: data.plugin_version.0,
            info,
            module: module as usize,
        });
    }
    log::info!(target: "f4x::f4se", "plugin {file_name}: {name:?} v{} accepted (handle {handle})", data.plugin_version);
    // SAFETY: the exports have the F4SE signatures by contract.
    let (preload, load) = unsafe {
        (preload.map(|f| std::mem::transmute::<_, PreloadFn>(f)), load.map(|f| std::mem::transmute::<_, LoadFn>(f)))
    };
    Ok(Some(Pending::Declared { handle, module: module as usize, file_name: file_name.to_owned(), preload, load }))
}

fn preload(phases: &mut Phases) {
    let mut kept = Vec::new();
    for pending in std::mem::take(&mut phases.pending) {
        let Pending::Declared { handle, module, ref file_name, preload: Some(preload), .. } = pending else {
            kept.push(pending);
            continue;
        };
        log::info!(target: "f4x::f4se", "preloading {file_name} (handle {handle})");
        CURRENT.store(handle, Relaxed);
        // SAFETY: the plugin's entry point with our table.
        let ok = unsafe { preload(interface()) };
        CURRENT.store(u32::MAX, Relaxed);
        if ok {
            kept.push(pending);
        } else {
            let error = "F4SEPlugin_Preload returned false".to_owned();
            log::warn!(target: "f4x::f4se", "plugin {file_name}: {error}");
            let path = PLUGINS.lock().ok().and_then(|p| p.iter().find(|p| p.handle == handle).map(|p| p.path.display().to_string()));
            unregister(handle);
            unload(module);
            phases.states.push(PluginState { name: file_name.clone(), path: path.unwrap_or_default(), loaded: false, error: Some(error) });
        }
    }
    phases.pending = kept;
}

/// The load phase (running the check and preload first if they have not run).
pub(super) fn load(files: impl FnOnce() -> Vec<PathBuf>) -> Vec<PluginState> {
    let not_yet = PHASES.lock().map(|p| p.is_none()).unwrap_or(false);
    if not_yet {
        check_and_preload(files());
    }
    let Some(mut phases) = PHASES.lock().ok().and_then(|mut p| p.take()) else { return Vec::new() };
    let mut states = std::mem::take(&mut phases.states);
    for pending in std::mem::take(&mut phases.pending) {
        match pending {
            Pending::Declared { handle, module, file_name, load, .. } => {
                let path = PLUGINS
                    .lock()
                    .ok()
                    .and_then(|p| p.iter().find(|p| p.handle == handle).map(|p| (p.path.display().to_string(), p.name.to_string_lossy().into_owned())));
                let (path, name) = path.unwrap_or_default();
                let mut state = PluginState { name: format!("{name} ({file_name})"), path, loaded: true, error: None };
                if let Some(load) = load {
                    CURRENT.store(handle, Relaxed);
                    // SAFETY: the plugin's entry point with our table.
                    let ok = unsafe { load(interface()) };
                    CURRENT.store(u32::MAX, Relaxed);
                    if !ok {
                        unregister(handle);
                        unload(module);
                        state.loaded = false;
                        state.error = Some("F4SEPlugin_Load returned false".into());
                    }
                }
                match &state.error {
                    None => log::info!(target: "f4x::f4se", "plugin {file_name} (handle {handle}, {name}) loaded"),
                    Some(error) => log::warn!(target: "f4x::f4se", "plugin {file_name}: {error}"),
                }
                states.push(state);
            }
            Pending::Legacy { path, file_name } => {
                let handle = phases.next_handle;
                let mut state = PluginState { name: file_name.clone(), path: path.display().to_string(), loaded: false, error: None };
                match load_one(handle, &path, &file_name) {
                    Ok((name, version)) => {
                        log::info!(target: "f4x::f4se", "old-gen plugin {file_name} (handle {handle}, {name} v{version}) loaded");
                        state.name = format!("{name} ({file_name})");
                        state.loaded = true;
                        phases.next_handle += 1;
                    }
                    Err(error) => {
                        log::warn!(target: "f4x::f4se", "plugin {file_name}: {error}");
                        state.error = Some(error);
                    }
                }
                states.push(state);
            }
        }
    }
    PLUGIN_INFO_READY.store(true, Relaxed);
    states
}
