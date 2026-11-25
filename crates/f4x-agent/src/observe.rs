//! Engine observers that need no per-build addresses, only RTTI:
//! - **menus**: every class deriving from `IMenu` gets vtable slot 4 (`AdvanceMovie`,
//!   called each frame while the menu is open; CommonLibF4 `IMenu.h`) wrapped, so
//!   `state.menus` lists the menus that ticked recently (e.g. `MainMenu`, `HUDMenu`);
//! - **natives**: the Papyrus VM's `BindNativeMethod` (slot 0x1B) is wrapped to catalog
//!   every native the engine binds (spike S3). Natives are never called.
//! - **script events**: the VM's `SendEvent` (0x2B) and `DispatchMethodCall` (0x2D object,
//!   0x2E handle) are wrapped to record each event/call name with the frame it was queued
//!   in (`papyrus.events`). This shows black-box what a host sends scripts, and when.

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
// Slots 4 and 0x1B on both 1.10.163 and 1.11.240: next-gen CommonLibF4 40a93f69 `IMenu.h`,
// `BSScript_IVirtualMachine.h`; f4x-vtcmp shows `IMenu` (19 slots) and
// `Internal::VirtualMachine` (62) unchanged, with AdvanceMovie matched at slot 4.
const ADVANCE_MOVIE_SLOT: usize = f4x_engine::layout::RE::IMenu::vtbl::AdvanceMovie;
const VM_CLASS: &str = ".?AVVirtualMachine@Internal@BSScript@@";
const BIND_NATIVE_METHOD_SLOT: usize = f4x_engine::layout::RE::BSScript::IVirtualMachine::vtbl::BindNativeMethod;

/// Work to run inside a menu's next `AdvanceMovie` (on the thread that owns its movie),
/// keyed by menu class name. The job gets the `IMenu*`.
pub type MenuJob = Box<dyn FnOnce(*mut c_void) + Send>;
static MENU_JOBS: Mutex<Vec<(String, MenuJob)>> = Mutex::new(Vec::new());

/// Run `job` in the next `AdvanceMovie` of the menu class `name` (e.g. `MainMenu`).
pub fn on_menu(name: &str, job: MenuJob) {
    if let Ok(mut jobs) = MENU_JOBS.lock() {
        jobs.push((name.to_owned(), job));
    }
}

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
    unsafe { std::mem::transmute::<usize, AdvanceMovie>(hook.real)(this, delta, time) };
    let jobs: Vec<MenuJob> = match MENU_JOBS.lock() {
        Ok(mut jobs) if jobs.iter().any(|(name, _)| *name == hook.name) => {
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut *jobs).into_iter().partition(|(name, _)| *name == hook.name);
            *jobs = rest;
            mine.into_iter().map(|(_, job)| job).collect()
        }
        _ => Vec::new(),
    };
    for job in jobs {
        let this = this as usize;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(this as *mut c_void)));
    }
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
/// The Papyrus VM (`IVirtualMachine*`), as seen by the first `BindNativeMethod` call.
static VM: AtomicUsize = AtomicUsize::new(0);

/// The Papyrus VM, once the engine has started binding natives into it.
pub fn virtual_machine() -> Option<usize> {
    Some(VM.load(Relaxed)).filter(|&v| v != 0)
}
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
    let hooks: [(usize, usize, &AtomicUsize, &str); 3] = [
        (SEND_EVENT_SLOT, send_event_hook as SendEventFn as usize, &REAL_SEND_EVENT, "SendEvent"),
        (DISPATCH_ON_OBJECT_SLOT, dispatch_object_hook as DispatchObjectFn as usize, &REAL_DISPATCH_OBJECT, "DispatchMethodCall(object)"),
        (DISPATCH_BY_HANDLE_SLOT, dispatch_handle_hook as DispatchHandleFn as usize, &REAL_DISPATCH_HANDLE, "DispatchMethodCall(handle)"),
    ];
    for (index, hook, real, name) in hooks {
        let slot = (base + vtable as usize + 8 * index) as *mut usize;
        // SAFETY: a slot of the VM vtable (generated layout, same on both generations).
        match unsafe { hooks::patch_pointer(slot, hook) } {
            Ok(original) => real.store(original, Relaxed),
            Err(error) => log::warn!(target: "f4x::observe", "{name} hook: {error}"),
        }
    }
}

// ---- script events ----

const SEND_EVENT_SLOT: usize = f4x_engine::layout::RE::BSScript::IVirtualMachine::vtbl::SendEvent;
const DISPATCH_ON_OBJECT_SLOT: usize = f4x_engine::layout::RE::BSScript::IVirtualMachine::vtbl::DispatchMethodCall;
const DISPATCH_BY_HANDLE_SLOT: usize = f4x_engine::layout::RE::BSScript::IVirtualMachine::vtbl::DispatchMethodCall_2;
const _: () = assert!(SEND_EVENT_SLOT == 0x2B && DISPATCH_ON_OBJECT_SLOT == 0x2D && DISPATCH_BY_HANDLE_SLOT == 0x2E);

/// `SendEvent(vm, handle, const BSFixedString& event, const args functor&)`.
type SendEventFn = unsafe extern "C" fn(*mut c_void, u64, *const *const u8, *const c_void);
/// `DispatchMethodCall(vm, const BSTSmartPointer<Object>&, const BSFixedString& fn, args, callback)`.
type DispatchObjectFn = unsafe extern "C" fn(*mut c_void, *const c_void, *const *const u8, *const c_void, *const c_void) -> bool;
/// `DispatchMethodCall(vm, handle, const BSFixedString& script, const BSFixedString& fn, args, callback)`.
type DispatchHandleFn =
    unsafe extern "C" fn(*mut c_void, u64, *const *const u8, *const *const u8, *const c_void, *const c_void) -> bool;

static REAL_SEND_EVENT: AtomicUsize = AtomicUsize::new(0);
static REAL_DISPATCH_OBJECT: AtomicUsize = AtomicUsize::new(0);
static REAL_DISPATCH_HANDLE: AtomicUsize = AtomicUsize::new(0);
static EVENTS: Mutex<std::collections::VecDeque<Value>> = Mutex::new(std::collections::VecDeque::new());
const EVENTS_KEPT: usize = 1024;

/// Recording starts with the first `papyrus.events` request (the hooks only pass through
/// until then, so normal play pays nothing).
static EVENTS_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn record_event(kind: &str, handle: Option<u64>, script: Option<*const *const u8>, name: *const *const u8) {
    if !EVENTS_ON.load(Relaxed) {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: BSFixedString references the engine passed us (pool entry or null).
        let text = |s: *const *const u8| unsafe {
            if s.is_null() { None } else { crate::engine_strings::fixed_string(*s) }
        };
        let Some(name) = text(name) else { return };
        let mut entry = json!({"frame": crate::clock::frames(), "kind": kind, "name": name});
        if let Some(handle) = handle {
            entry["handle"] = json!(format!("{handle:#x}"));
        }
        if let Some(script) = script.and_then(text) {
            entry["script"] = json!(script);
        }
        if let Ok(mut events) = EVENTS.lock() {
            events.push_back(entry);
            while events.len() > EVENTS_KEPT {
                events.pop_front();
            }
        }
    });
}

unsafe extern "C" fn send_event_hook(vm: *mut c_void, handle: u64, name: *const *const u8, args: *const c_void) {
    record_event("event", Some(handle), None, name);
    // SAFETY: the original slot value, called with the caller's arguments.
    unsafe { std::mem::transmute::<usize, SendEventFn>(REAL_SEND_EVENT.load(Relaxed))(vm, handle, name, args) }
}

unsafe extern "C" fn dispatch_object_hook(
    vm: *mut c_void,
    object: *const c_void,
    name: *const *const u8,
    args: *const c_void,
    callback: *const c_void,
) -> bool {
    record_event("call", None, None, name);
    // SAFETY: as above.
    unsafe { std::mem::transmute::<usize, DispatchObjectFn>(REAL_DISPATCH_OBJECT.load(Relaxed))(vm, object, name, args, callback) }
}

unsafe extern "C" fn dispatch_handle_hook(
    vm: *mut c_void,
    handle: u64,
    script: *const *const u8,
    name: *const *const u8,
    args: *const c_void,
    callback: *const c_void,
) -> bool {
    record_event("call", Some(handle), Some(script), name);
    // SAFETY: as above.
    unsafe { std::mem::transmute::<usize, DispatchHandleFn>(REAL_DISPATCH_HANDLE.load(Relaxed))(vm, handle, script, name, args, callback) }
}

/// RPC `papyrus.events {filter?, since_frame?, clear?}`: recorded script events and calls
/// (most recent 1024), optionally only names containing `filter` (case-insensitive). The
/// first call starts the recording. Not every delivery path goes through these two VM
/// functions (F4SE 0.7.9's furniture events don't).
pub fn events_rpc(params: &Value) -> Value {
    EVENTS_ON.store(true, Relaxed);
    let filter = params.get("filter").and_then(Value::as_str).map(str::to_ascii_lowercase);
    let since = params.get("since_frame").and_then(Value::as_u64).unwrap_or(0);
    let Ok(mut events) = EVENTS.lock() else { return json!([]) };
    let out: Vec<Value> = events
        .iter()
        .filter(|e| e["frame"].as_u64().unwrap_or(0) >= since)
        .filter(|e| filter.as_ref().is_none_or(|f| e["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().contains(f.as_str()))))
        .cloned()
        .collect();
    if params.get("clear").and_then(Value::as_bool).unwrap_or(false) {
        events.clear();
    }
    json!(out)
}

unsafe extern "C" fn bind_hook(vm: *mut c_void, function: *mut c_void) -> bool {
    let _ = VM.compare_exchange(0, vm as usize, Relaxed, Relaxed);
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
    use f4x_engine::layout::RE::BSScript::{Internal::VDescTable as DT, NF_util::NativeFunctionBase as NF};
    // SAFETY: NativeFunctionBase field layout (CommonLibF4 BSScript.h; provenance/engine.jsonl),
    // the same on 1.11.240 (next-gen CommonLibF4 `BSScript_Internal_NativeFunctionBase.h`; the
    // engine's NativeFunctionN::HasStub tests the callback at +0x50 in both exes).
    unsafe {
        let vtable = f.cast::<usize>().read();
        let class = class_info(image, (vtable - base) as u32).map(|c| c.mangled_name);
        let read_str = |off: usize| fixed_string(f.add(off).cast::<*const u8>().read_unaligned());
        let entries = f.add(NF::descTable + DT::entries).cast::<*const u8>().read_unaligned();
        let param_count = f.add(NF::descTable + DT::paramCount).cast::<u16>().read_unaligned() as usize;
        let mut params = Vec::with_capacity(param_count);
        for i in 0..param_count.min(64) {
            let entry = entries.add(16 * i);
            params.push(json!({
                "name": fixed_string(entry.cast::<*const u8>().read_unaligned()),
                "type": type_info(entry.add(8).cast::<u64>().read_unaligned()),
            }));
        }
        let callback = f.add(NF::SIZE).cast::<usize>().read_unaligned();
        let callback_rva = callback.checked_sub(base).filter(|&r| r < image.size()).map(|r| format!("{r:#x}"));
        Some(json!({
            "object": read_str(NF::objName), "name": read_str(NF::name), "state": read_str(NF::stateName),
            "returns": type_info(f.add(NF::retType).cast::<u64>().read_unaligned()),
            "params": params,
            "static": f.add(NF::isStatic).read() != 0, "latent": f.add(NF::isLatent).read() != 0,
            "class": class, "vtable_rva": format!("{:#x}", vtable - base), "callback_rva": callback_rva,
        }))
    }
}
