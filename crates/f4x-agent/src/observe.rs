//! Engine observers that need no per-build addresses, only RTTI:
//! - **menus**: every class deriving from `IMenu` gets vtable slot 4 (`AdvanceMovie`,
//!   called each frame while the menu is open; CommonLibF4 `IMenu.h`) wrapped, so
//!   `state.menus` lists the menus that ticked recently (e.g. `MainMenu`, `HUDMenu`);
//! - **natives**: the Papyrus VM's `BindNativeMethod` (slot 0x1B) is wrapped to catalog
//!   every native the engine binds (spike S3). Natives are never called.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use f4x_pe::rtti::{RttiIndex, class_info, demangle};
use f4x_pe::{Image, MappedImage};
use serde_json::{Value, json};

use crate::hooks;

static IMAGE: OnceLock<MappedImage> = OnceLock::new();

const MENU_BASE: &str = ".?AVIMenu@@";
const ADVANCE_MOVIE_SLOT: usize = 4;
const VM_CLASS: &str = ".?AVVirtualMachine@Internal@BSScript@@";
const BIND_NATIVE_METHOD_SLOT: usize = 0x1B;

struct MenuHook {
    name: String,
    real: usize,
    last_tick_ms: AtomicU64,
}

/// vtable address -> hook record (built once, then read-only).
static MENUS: OnceLock<HashMap<usize, MenuHook>> = OnceLock::new();

pub fn install(base: usize) {
    // SAFETY: `base` is the game exe, mapped for the life of the process.
    let Some(image) = (unsafe { MappedImage::from_base(base) }) else {
        log::warn!(target: "f4x::observe", "could not read game image headers");
        return;
    };
    let started = std::time::Instant::now();
    let index = RttiIndex::scan(&image);
    log::info!(
        target: "f4x::observe",
        "RTTI: {} classes, {} vtables in {:?}",
        index.type_descriptors.len(),
        index.vtables.len(),
        started.elapsed()
    );
    install_menu_hooks(&image, &index, base);
    install_catalog(&index, base);
    let _ = IMAGE.set(image);
}

// ---- menus ----

type AdvanceMovie = unsafe extern "C" fn(*mut c_void, f32, u64);

fn install_menu_hooks(image: &MappedImage, index: &RttiIndex, base: usize) {
    let mut hooks_by_vtable = HashMap::new();
    for vtable in index.vtables.iter().filter(|v| v.offset == 0) {
        let Some(info) = class_info(image, vtable.vtable_rva) else { continue };
        if !info.bases.iter().skip(1).any(|b| b.mangled_name == MENU_BASE) {
            continue;
        }
        let slot = (base + vtable.vtable_rva as usize + 8 * ADVANCE_MOVIE_SLOT) as *mut usize;
        // SAFETY: slot 4 of an IMenu-derived vtable in .rdata.
        match unsafe { hooks::patch_pointer(slot, advance_movie_hook as AdvanceMovie as usize) } {
            Ok(real) => {
                hooks_by_vtable.insert(
                    base + vtable.vtable_rva as usize,
                    MenuHook { name: demangle(&info.mangled_name), real, last_tick_ms: AtomicU64::new(0) },
                );
            }
            Err(error) => log::warn!(target: "f4x::observe", "{}: {error}", info.name()),
        }
    }
    log::info!(target: "f4x::observe", "watching {} menu classes", hooks_by_vtable.len());
    let _ = MENUS.set(hooks_by_vtable);
}

unsafe extern "C" fn advance_movie_hook(this: *mut c_void, delta: f32, time: u64) {
    // SAFETY: `this` is a live IMenu; its first field is the vtable we hooked.
    let vtable = unsafe { *(this as *const usize) };
    let Some(hook) = MENUS.get().and_then(|m| m.get(&vtable)) else { return };
    let now = crate::clock::uptime_ms();
    if hook.last_tick_ms.swap(now, Relaxed) == 0 {
        log::info!(target: "f4x::menus", "menu active: {}", hook.name);
    }
    // SAFETY: the original slot value for this vtable.
    unsafe { std::mem::transmute::<usize, AdvanceMovie>(hook.real)(this, delta, time) }
}

/// Menus that ticked during the last second.
pub fn active_menus() -> Vec<String> {
    let now = crate::clock::uptime_ms();
    let mut names: Vec<String> = MENUS
        .get()
        .into_iter()
        .flatten()
        .filter(|(_, h)| {
            let last = h.last_tick_ms.load(Relaxed);
            last != 0 && now.saturating_sub(last) < 1000
        })
        .map(|(_, h)| h.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

// ---- native catalog ----

static REAL_BIND: AtomicUsize = AtomicUsize::new(0);
static NATIVES: Mutex<Vec<Value>> = Mutex::new(Vec::new());

type BindFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool;

fn install_catalog(index: &RttiIndex, base: usize) {
    let Some(vtable) = index.primary_vtable(VM_CLASS) else {
        log::warn!(target: "f4x::observe", "Papyrus VM vtable not found");
        return;
    };
    let slot = (base + vtable as usize + 8 * BIND_NATIVE_METHOD_SLOT) as *mut usize;
    // SAFETY: slot 0x1B of the VM vtable.
    match unsafe { hooks::patch_pointer(slot, bind_hook as BindFn as usize) } {
        Ok(real) => REAL_BIND.store(real, Relaxed),
        Err(error) => log::warn!(target: "f4x::observe", "BindNativeMethod hook: {error}"),
    }
}

unsafe extern "C" fn bind_hook(vm: *mut c_void, function: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the VM passes a live IFunction.
        if let Some(record) = unsafe { describe_native(function) }
            && let Ok(mut natives) = NATIVES.lock()
        {
            natives.push(record);
        }
    });
    // SAFETY: the original slot value.
    unsafe { std::mem::transmute::<usize, BindFn>(REAL_BIND.load(Relaxed))(vm, function) }
}

pub fn native_count() -> usize {
    NATIVES.lock().map(|n| n.len()).unwrap_or(0)
}

/// The catalog as JSON Lines.
pub fn natives_jsonl() -> String {
    let Ok(natives) = NATIVES.lock() else { return String::new() };
    let mut text = String::with_capacity(natives.len() * 256);
    for record in natives.iter() {
        text.push_str(&record.to_string());
        text.push('\n');
    }
    text
}

/// # Safety
/// `function` must point to a live `IFunction`.
unsafe fn describe_native(function: *mut c_void) -> Option<Value> {
    use crate::engine_strings::{fixed_string, type_info};
    // The image is set after hooks go in; bindings happen later, during game init.
    let image = IMAGE.get()?;
    let base = image.base() as usize;
    let f = function as *const u8;
    // SAFETY: NativeFunctionBase field layout (CommonLibF4 BSScript.h; provenance/engine.jsonl).
    unsafe {
        let vtable = f.cast::<usize>().read();
        let class = class_info(image, (vtable - base) as u32).map(|c| c.mangled_name);
        let read_str = |off: usize| fixed_string(f.add(off).cast::<*const u8>().read_unaligned());
        let entries = f.add(0x30).cast::<*const u8>().read_unaligned();
        let param_count = f.add(0x38).cast::<u16>().read_unaligned() as usize;
        let mut params = Vec::with_capacity(param_count);
        for i in 0..param_count.min(64) {
            let entry = entries.add(16 * i);
            params.push(json!({
                "name": fixed_string(entry.cast::<*const u8>().read_unaligned()),
                "type": type_info(entry.add(8).cast::<u64>().read_unaligned()),
            }));
        }
        let callback = f.add(0x50).cast::<usize>().read_unaligned();
        let callback_rva = callback.checked_sub(base).filter(|&r| r < image.size()).map(|r| format!("{r:#x}"));
        Some(json!({
            "object": read_str(0x18), "name": read_str(0x10), "state": read_str(0x20),
            "returns": type_info(f.add(0x28).cast::<u64>().read_unaligned()),
            "params": params,
            "static": f.add(0x40).read() != 0, "latent": f.add(0x42).read() != 0,
            "class": class, "vtable_rva": format!("{:#x}", vtable - base), "callback_rva": callback_rva,
        }))
    }
}
