//! Save/load lifecycle: `PreSaveGame`/`PostSaveGame`, `PreLoadGame`/`PostLoadGame`,
//! plugins' co-save callbacks and the `<save>.f4se` file.
//!
//! The engine saves through one function and loads through another (found from F4SE's
//! black-box message stacks plus our own disassembly; recipes in `f4x-engine-sys`).
//! Every direct `call` to them is redirected through a stub to our wrappers, which keep
//! F4SE's observed order (`docs/formats/cosave.md`):
//! - save:  PreSaveGame(name) → game save → plugin save callbacks + co-save → PostSaveGame(name)
//! - load:  PreLoadGame(name) → read co-save → revert callbacks → game load → load
//!   callbacks → PostLoadGame(success)

use std::ffi::{CStr, c_char, c_void};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_abi::f4se::message;
use f4x_cosave::{CoSave, HOST_UID, PluginBlock, plgn};
use f4x_engine_sys::og_1_10_163_addresses::{DATA_READY_CALLER, LOAD_GAME_WORKER, SAVE_GAME_WORKER};

/// Offset of the data-ready `call` inside `data_ready_caller` (1.10.163: 0xd3f268 -
/// 0xd3eff0; black-box stack + our disassembly). Verified to be `E8` before use.
const DATA_READY_CALL_OFFSET: usize = 0x278;
use f4x_pe::Image;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::interfaces::dispatch_from_host;
use super::{serialization, trampoline};

static SAVE_WORKER: AtomicUsize = AtomicUsize::new(0);
static LOAD_WORKER: AtomicUsize = AtomicUsize::new(0);
static DATA_READY: AtomicUsize = AtomicUsize::new(0);
/// F4SE's own block from the last loaded co-save, carried into the next save.
static HOST_BLOCK: Mutex<Option<PluginBlock>> = Mutex::new(None);

type Worker = unsafe extern "C" fn(usize, usize, usize, usize) -> usize;

pub fn install() {
    let save = f4x_engine::address::resolve(&SAVE_GAME_WORKER);
    let load = f4x_engine::address::resolve(&LOAD_GAME_WORKER);
    let (Some(save), Some(load)) = (save, load) else {
        log::warn!(target: "f4x::f4se", "save/load functions not found in this build; no save lifecycle");
        return;
    };
    SAVE_WORKER.store(save, Relaxed);
    LOAD_WORKER.store(load, Relaxed);
    let saves = redirect_calls(save, save_hook as Worker as usize);
    let loads = redirect_calls(load, load_hook as Worker as usize);
    log::info!(target: "f4x::f4se", "save/load lifecycle hooked ({saves} save call sites, {loads} load call sites)");
    install_data_ready();
}

/// `GameDataReady` + `GameLoaded` from where F4SE sends them: the loader thread's call to
/// `data_ready_callee` inside `data_ready_caller` (black-box stack + our disassembly).
fn install_data_ready() {
    let site = f4x_engine::address::resolve(&DATA_READY_CALLER).map(|caller| caller + DATA_READY_CALL_OFFSET);
    // SAFETY: inside the resolved caller's code.
    let Some(site) = site.filter(|&site| unsafe { *(site as *const u8) } == 0xE8) else {
        log::warn!(target: "f4x::f4se", "data-ready call not found; falling back to the main-menu detector");
        return;
    };
    // SAFETY: an `E8 rel32` call in the game's code.
    let callee = unsafe { (site + 5).wrapping_add_signed((site as *const u8).add(1).cast::<i32>().read_unaligned() as isize) };
    DATA_READY.store(callee, Relaxed);
    let sites = redirect_calls_in(site, 5, callee, data_ready_hook as Worker as usize);
    if sites == 1 {
        super::DATA_READY_HOOKED.store(true, Relaxed);
        log::info!(target: "f4x::f4se", "data-ready hooked");
    } else {
        log::warn!(target: "f4x::f4se", "expected one data-ready call site, found {sites}; using the main-menu detector");
    }
}

unsafe extern "C" fn data_ready_hook(a: usize, b: usize, c: usize, d: usize) -> usize {
    // SAFETY: the engine's own function with its own arguments.
    let result = unsafe { std::mem::transmute::<usize, Worker>(DATA_READY.load(Relaxed))(a, b, c, d) };
    super::send_data_ready();
    result
}

/// Point every `call rel32` to `target` in the game's code at `hook` (via a stub near the
/// image). Returns the number of sites.
fn redirect_calls(target: usize, hook: usize) -> usize {
    let game = f4x_engine::game();
    let image = game.image();
    let Some(text) = image.section(".text").cloned() else { return 0 };
    redirect_calls_in(game.base() + text.rva as usize, text.readable_size as usize, target, hook)
}

/// Like [`redirect_calls`], restricted to `len` bytes of code at `start`.
fn redirect_calls_in(start: usize, len: usize, target: usize, hook: usize) -> usize {
    let game = f4x_engine::game();
    // SAFETY: a range inside the mapped game code.
    let bytes = unsafe { std::slice::from_raw_parts(start as *const u8, len) };
    let sites: Vec<usize> = (0..(bytes.len() + 1).saturating_sub(5))
        .filter(|&i| bytes[i] == 0xE8 && {
            let rel = i32::from_le_bytes(bytes[i + 1..i + 5].try_into().unwrap());
            (start + i + 5).wrapping_add_signed(rel as isize) == target
        })
        .map(|i| start + i)
        .collect();
    let stub = trampoline::BRANCH.lock().map(|mut pool| pool.allocate(game.base(), 14)).unwrap_or(std::ptr::null_mut()) as usize;
    if stub == 0 {
        return 0;
    }
    // SAFETY: freshly allocated RWX stub memory; call sites are `E8 rel32` in .text.
    unsafe {
        let code = stub as *mut u8;
        std::ptr::copy_nonoverlapping([0xFFu8, 0x25, 0, 0, 0, 0].as_ptr(), code, 6);
        (code.add(6) as *mut u64).write_unaligned(hook as u64);
        for &site in &sites {
            let rel = (stub as isize - (site as isize + 5)) as i32;
            let mut old: PAGE_PROTECTION_FLAGS = 0;
            VirtualProtect((site + 1) as *const c_void, 4, PAGE_EXECUTE_READWRITE, &mut old);
            ((site + 1) as *mut i32).write_unaligned(rel);
            VirtualProtect((site + 1) as *const c_void, 4, old, &mut old);
            FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 5);
        }
    }
    sites.len()
}

fn save_name(ptr: usize) -> Option<String> {
    if ptr == 0 || !f4x_engine::mem::readable(ptr, 1) {
        return None;
    }
    // SAFETY: readable; the engine passes a NUL-terminated save name.
    let name = unsafe { CStr::from_ptr(ptr as *const c_char) }.to_string_lossy().into_owned();
    (!name.is_empty() && name.len() < 260).then_some(name)
}

unsafe extern "C" fn save_hook(manager: usize, name: usize, flag: usize, extra: usize) -> usize {
    let save = save_name(name);
    if let Some(save) = &save {
        dispatch_from_host(message::PRE_SAVE_GAME, name as *mut c_void, save.len() as u32);
    }
    // SAFETY: calling the engine's own save function with its arguments.
    let result = unsafe { std::mem::transmute::<usize, Worker>(SAVE_WORKER.load(Relaxed))(manager, name, flag, extra) };
    if let Some(save) = &save {
        let _ = std::panic::catch_unwind(|| write_cosave(save));
        dispatch_from_host(message::POST_SAVE_GAME, name as *mut c_void, save.len() as u32);
    }
    result
}

unsafe extern "C" fn load_hook(manager: usize, name: usize, flag: usize, out: usize) -> usize {
    let save = save_name(name);
    let mut cosave = None;
    if let Some(save) = &save {
        dispatch_from_host(message::PRE_LOAD_GAME, name as *mut c_void, save.len() as u32);
        cosave = read_cosave(save);
        for (_, registration) in serialization::registrations() {
            if let Some(revert) = registration.revert {
                serialization::run_revert(revert);
            }
        }
    }
    // SAFETY: calling the engine's own load function with its arguments.
    let result = unsafe { std::mem::transmute::<usize, Worker>(LOAD_WORKER.load(Relaxed))(manager, name, flag, out) };
    if save.is_some() {
        let _ = std::panic::catch_unwind(|| run_load_callbacks(cosave.as_ref()));
        dispatch_from_host(message::POST_LOAD_GAME, (result & 0xFF) as *mut c_void, 1);
    }
    result
}

fn write_cosave(save: &str) {
    let mut cosave = CoSave::new(super::interfaces::F4SE_VERSION.0, f4x_abi::version::runtime::V1_10_163.0);
    let order = serialization::current_load_order();
    // F4SE's own block: carried through from the last load (with the current PLGN), or
    // the empty block F4SE writes when nothing is registered.
    let host = HOST_BLOCK.lock().ok().and_then(|h| h.clone());
    let host_chunks = match host {
        Some(mut block) => {
            for chunk in &mut block.chunks {
                if chunk.kind == f4x_cosave::tag(&plgn::KIND) {
                    chunk.data = plgn::write(&order);
                }
            }
            block.chunks
        }
        None => f4x_cosave::empty_host_chunks(&order),
    };
    cosave.plugins.push(PluginBlock { uid: HOST_UID, chunks: host_chunks });
    for (handle, registration) in serialization::registrations() {
        let (Some(uid), Some(save_callback)) = (registration.uid, registration.save) else { continue };
        let chunks = serialization::collect_save(save_callback);
        log::debug!(target: "f4x::f4se", "plugin {handle} wrote {} co-save records", chunks.len());
        // F4SE leaves out plugins that wrote nothing.
        if !chunks.is_empty() {
            cosave.plugins.push(PluginBlock { uid, chunks });
        }
    }
    match saves_dir() {
        Some(dir) => {
            let path = dir.join(format!("{save}.f4se"));
            let temp = dir.join(format!("{save}.f4se.tmp"));
            let written = std::fs::write(&temp, cosave.to_bytes()).and_then(|()| std::fs::rename(&temp, &path));
            match written {
                Ok(()) => log::info!(target: "f4x::f4se", "co-save written: {} ({} blocks)", path.display(), cosave.plugins.len()),
                Err(e) => log::error!(target: "f4x::f4se", "co-save {}: {e}", path.display()),
            }
        }
        None => log::error!(target: "f4x::f4se", "no saves directory; co-save not written"),
    }
}

fn read_cosave(save: &str) -> Option<CoSave> {
    let path = saves_dir()?.join(format!("{save}.f4se"));
    let bytes = std::fs::read(&path).ok()?;
    match CoSave::parse(&bytes) {
        Ok(cosave) => {
            log::info!(target: "f4x::f4se", "co-save read: {} ({} blocks)", path.display(), cosave.plugins.len());
            Some(cosave)
        }
        Err(e) => {
            log::error!(target: "f4x::f4se", "co-save {}: {e}", path.display());
            None
        }
    }
}

fn run_load_callbacks(cosave: Option<&CoSave>) {
    let host = cosave.and_then(|c| c.plugin(HOST_UID)).cloned();
    let order = host
        .as_ref()
        .and_then(|h| h.chunks.iter().find(|c| c.kind == f4x_cosave::tag(&plgn::KIND)))
        .and_then(|c| plgn::parse(&c.data))
        .unwrap_or_default();
    serialization::set_save_order(order);
    if let Ok(mut slot) = HOST_BLOCK.lock() {
        *slot = host;
    }
    for (_, registration) in serialization::registrations() {
        let (Some(uid), Some(load)) = (registration.uid, registration.load) else { continue };
        let chunks = cosave.and_then(|c| c.plugin(uid)).map(|p| p.chunks.clone()).unwrap_or_default();
        serialization::run_load(load, chunks);
    }
}

/// `Documents\My Games\Fallout4\<sLocalSavePath>` (default `Saves\`).
fn saves_dir() -> Option<PathBuf> {
    let my_games = documents()?.join("My Games").join("Fallout4");
    let mut local = String::from("Saves\\");
    for ini in ["Fallout4.ini", "Fallout4Custom.ini"] {
        if let Some(value) = ini_value(&my_games.join(ini), "General", "sLocalSavePath") {
            local = value;
        }
    }
    Some(my_games.join(local.trim_end_matches(['\\', '/'])))
}

fn ini_value(path: &std::path::Path, section: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_section = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[') {
            in_section = name.trim_end_matches(']').eq_ignore_ascii_case(section);
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case(key)
        {
            found = Some(v.trim().to_owned());
        }
    }
    found
}

fn documents() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: standard shell call; the buffer is freed below.
    unsafe {
        if SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut path) < 0 || path.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path.cast());
        Some(PathBuf::from(text))
    }
}
