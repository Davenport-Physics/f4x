//! `ScriptObject` registrations for keys, the player camera's state and furniture, and
//! their events (F4SE's `RegisterForKey` → `OnKeyDown`/`OnKeyUp`,
//! `RegisterForCameraState` → `OnPlayerCameraState`, `RegisterForFurnitureEvent` →
//! `OnFurnitureEvent`).
//!
//! CommonLibF4's `F4SEStub/runtime/src/Papyrus/ScriptObject.h` only declares these, so
//! the behaviour is matched black-box to F4SE 0.6.23 (`provenance/natives-events.jsonl`):
//! - key codes: DirectInput scan codes for the keyboard (0..256), 256 + button for the
//!   mouse (256..266) and 266 + the XInput button index for the gamepad (266..282), as in
//!   the stub's `Input.h`. `OnKeyDown` on a press, `OnKeyUp(code, heldDownSecs)` on the
//!   release, menus open or not;
//! - camera states: `RE::CameraStates::CameraState` (`TESCamera.h`), the player camera's
//!   current state, polled once per main-loop pass;
//! - furniture: `RE::TESFurnitureEvent` (`Events.h`), observed through the sink
//!   `PlayerControls` already registers (its vtable found by RTTI). Every registrant gets
//!   every event: F4SE ignores the filter, and its `UnregisterForFurnitureEvent` removes
//!   nothing;
//! - one registration per (handle, script) for each; saved in F4SE's co-save sections
//!   `KEYR`, `CAMR` and `FRNR` (`f4x_cosave::regs`).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering::Relaxed};

use f4x_cosave::Chunk;
use f4x_cosave::regs::{self, Registrant};
use f4x_engine::layout::RE;
use f4x_engine::mem;
use f4x_engine::papyrus::{Arg, NativeCall, Vm};
use f4x_engine::papyrus_value::PValue;
use f4x_engine_sys::og_1_10_163_addresses::PLAYER_CAMERA_SINGLETON;

use super::Entry;
use crate::f4se::natives::registrant;

pub(crate) const NATIVES: &[Entry] = &[
    ("ScriptObject", "RegisterForKey", register_for_key),
    ("ScriptObject", "UnregisterForKey", unregister_for_key),
    ("ScriptObject", "RegisterForCameraState", register_for_camera_state),
    ("ScriptObject", "UnregisterForCameraState", unregister_for_camera_state),
    ("ScriptObject", "RegisterForFurnitureEvent", register_for_furniture_event),
    ("ScriptObject", "UnregisterForFurnitureEvent", unregister_for_furniture_event),
];

fn vm() -> Option<Vm> {
    f4x_agent::observe::virtual_machine().map(|vm| Vm(vm as *mut _))
}

fn by_handle(a: &Registrant, b: &Registrant) -> std::cmp::Ordering {
    a.handle.cmp(&b.handle).then_with(|| a.script.cmp(&b.script))
}

/// Insert `who` keeping handle order; false if it was already there.
fn insert(list: &mut Vec<Registrant>, who: Registrant) -> bool {
    match list.binary_search_by(|r| by_handle(r, &who)) {
        Ok(_) => false,
        Err(at) => {
            list.insert(at, who);
            true
        }
    }
}

/// Handles carry a form id in their low 32 bits: map it to the current load order.
fn remap(r: Registrant) -> Option<Registrant> {
    let id = crate::f4se::serialization::remap_form_id(r.handle as u32)?;
    Some(Registrant { handle: (r.handle & !0xFFFF_FFFF) | u64::from(id), script: r.script })
}

fn dispatch(targets: &[Registrant], event: &str, args: impl Fn() -> Vec<Arg>) -> usize {
    let Some(vm) = vm() else { return 0 };
    let mut sent = 0;
    for r in targets {
        if vm.dispatch_on_bound(r.handle, &r.script, event, args()) {
            sent += 1;
        }
    }
    sent
}

// ---- keys ----

/// Key code → registered scripts (handle order).
static KEYS: Mutex<BTreeMap<u32, Vec<Registrant>>> = Mutex::new(BTreeMap::new());
static ANY_KEYS: AtomicBool = AtomicBool::new(false);

fn register_for_key(call: &mut NativeCall) -> Result<(), String> {
    let key = call.int(0) as u32;
    let who = registrant(call)?;
    let mut keys = KEYS.lock().map_err(|_| "poisoned")?;
    let script = who.script.clone();
    if insert(keys.entry(key).or_default(), who) {
        log::info!(target: "f4x::papyrus", "{script} registered for key {key}");
    }
    ANY_KEYS.store(true, Relaxed);
    Ok(())
}

fn unregister_for_key(call: &mut NativeCall) -> Result<(), String> {
    let key = call.int(0) as u32;
    let who = registrant(call)?;
    let mut keys = KEYS.lock().map_err(|_| "poisoned")?;
    if let Some(list) = keys.get_mut(&key) {
        list.retain(|r| r != &who);
        if list.is_empty() {
            keys.remove(&key);
        }
    }
    ANY_KEYS.store(!keys.is_empty(), Relaxed);
    Ok(())
}

/// The key code F4SE reports for a button: the scan code (keyboard), 256 + the button
/// (mouse), 266 + the index of the XInput button mask (gamepad; triggers have their own
/// codes). `None` for buttons without one.
fn key_code(device: i32, id: u32) -> Option<u32> {
    const MOUSE: u32 = 256;
    const GAMEPAD: u32 = 266;
    // XINPUT_GAMEPAD masks in key order (F4SEStub `Input.h`: Up..Y, then LT, RT).
    const PAD: [u32; 16] =
        [0x1, 0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x1000, 0x2000, 0x4000, 0x8000, 0x9, 0xA];
    match device {
        0 if id < MOUSE => Some(id),
        1 if id < GAMEPAD - MOUSE => Some(MOUSE + id),
        2 => PAD.iter().position(|&m| m == id).map(|i| GAMEPAD + i as u32),
        _ => None,
    }
}

const BUTTON_EVENT: i32 = 0; // INPUT_EVENT_TYPE::kButton

/// Input observer: `OnKeyDown(code)` on a press, `OnKeyUp(code, held seconds)` on a
/// release, for every registered key in this frame's input list.
fn scan_input(head: *const u8) {
    if !ANY_KEYS.load(Relaxed) {
        return;
    }
    let mut event = head;
    for _ in 0..256 {
        if event.is_null() || !mem::readable(event as usize, RE::ButtonEvent::SIZE) {
            break;
        }
        // SAFETY: a live InputEvent (readable for a ButtonEvent's size).
        let kind: i32 = unsafe { mem::read(event, RE::InputEvent::eventType) };
        if kind == BUTTON_EVENT {
            // SAFETY: a ButtonEvent: device, idCode, value, heldDownSecs.
            let (device, id, value, held): (i32, u32, f32, f32) = unsafe {
                (
                    mem::read(event, RE::InputEvent::device),
                    mem::read(event, RE::IDEvent::idCode),
                    mem::read(event, RE::ButtonEvent::value),
                    mem::read(event, RE::ButtonEvent::heldDownSecs),
                )
            };
            let pressed = value != 0.0 && held == 0.0;
            let released = value == 0.0 && held != 0.0;
            if (pressed || released)
                && let Some(code) = key_code(device, id)
            {
                let targets = KEYS.lock().ok().and_then(|k| k.get(&code).cloned()).unwrap_or_default();
                if !targets.is_empty() {
                    let sent = if pressed {
                        dispatch(&targets, "OnKeyDown", || vec![Arg::Int(code as i32)])
                    } else {
                        dispatch(&targets, "OnKeyUp", || vec![Arg::Int(code as i32), Arg::Float(held)])
                    };
                    log::debug!(target: "f4x::papyrus", "key {code} {}: {sent} event(s)", if pressed { "down" } else { "up" });
                }
            }
        }
        // SAFETY: as above.
        event = unsafe { mem::read(event, RE::InputEvent::next) };
    }
}

fn key_entries() -> Vec<Vec<u8>> {
    let keys = KEYS.lock().map(|k| k.clone()).unwrap_or_default();
    keys.iter().map(|(&key, list)| regs::write_key(key, list)).collect()
}

fn load_keys(host: &[Chunk]) -> BTreeMap<u32, Vec<Registrant>> {
    let mut out: BTreeMap<u32, Vec<Registrant>> = BTreeMap::new();
    for chunk in regs::section(host, regs::KEYS) {
        let Some((key, list)) = regs::parse_key(&chunk.data) else { continue };
        for r in list.into_iter().filter_map(remap) {
            insert(out.entry(key).or_default(), r);
        }
    }
    out.retain(|_, l| !l.is_empty());
    out
}

// ---- camera state ----

static CAMERA: Mutex<Vec<Registrant>> = Mutex::new(Vec::new());
static ANY_CAMERA: AtomicBool = AtomicBool::new(false);
/// The camera state seen last (-1: not seen yet).
static LAST_STATE: AtomicI64 = AtomicI64::new(-1);
static PLAYER_CAMERA: AtomicUsize = AtomicUsize::new(0);
/// Ticks to wait before looking for the player camera again (the lookup scans memory).
static CAMERA_RETRY: AtomicUsize = AtomicUsize::new(0);

fn register_for_camera_state(call: &mut NativeCall) -> Result<(), String> {
    let who = registrant(call)?;
    let mut list = CAMERA.lock().map_err(|_| "poisoned")?;
    insert(&mut list, who);
    ANY_CAMERA.store(true, Relaxed);
    Ok(())
}

fn unregister_for_camera_state(call: &mut NativeCall) -> Result<(), String> {
    let who = registrant(call)?;
    let mut list = CAMERA.lock().map_err(|_| "poisoned")?;
    list.retain(|r| r != &who);
    ANY_CAMERA.store(!list.is_empty(), Relaxed);
    Ok(())
}

/// `PlayerCamera::GetSingleton()->currentState->id` (main thread). The singleton is the
/// global CommonLibF4 reads (`REL::ID(1171980)`): a memory scan for PlayerCamera
/// objects also finds objects that are not the live camera.
fn camera_state() -> Option<u32> {
    let mut camera = PLAYER_CAMERA.load(Relaxed);
    if camera == 0 {
        if CAMERA_RETRY.load(Relaxed) > 0 {
            CAMERA_RETRY.fetch_sub(1, Relaxed);
            return None;
        }
        let global = f4x_engine::address::resolve(&PLAYER_CAMERA_SINGLETON).filter(|&g| mem::readable(g, 8));
        // SAFETY: the singleton's global slot (readable).
        let found = global.map(|g| unsafe { mem::read::<usize>(g as *const u8, 0) });
        match found.filter(|&c| f4x_engine::game().is_instance_of(c, RE::TESCamera::RTTI)) {
            Some(c) => {
                log::info!(target: "f4x::papyrus", "player camera at {c:#x}");
                camera = c;
                PLAYER_CAMERA.store(c, Relaxed);
            }
            None => {
                if global.is_none() {
                    log::warn!(target: "f4x::papyrus", "PlayerCamera singleton not found; no OnPlayerCameraState");
                }
                CAMERA_RETRY.store(600, Relaxed);
                return None;
            }
        }
    }
    if !mem::readable(camera, RE::PlayerCamera::SIZE) {
        return None;
    }
    // SAFETY: the live PlayerCamera singleton (readable).
    let state: usize = unsafe { mem::read(camera as *const u8, RE::TESCamera::currentState) };
    if state == 0 || !mem::readable(state, RE::TESCameraState::SIZE) {
        return None;
    }
    // SAFETY: a live TESCameraState (readable).
    let id: u32 = unsafe { mem::read(state as *const u8, RE::TESCameraState::id) };
    (id < 13).then_some(id) // CameraStates::kTotal
}

/// Main-loop ticker: `OnPlayerCameraState(old, new)` when the camera's state changed.
fn poll_camera() {
    if !ANY_CAMERA.load(Relaxed) {
        LAST_STATE.store(-1, Relaxed);
        return;
    }
    let Some(now) = camera_state() else { return };
    let before = LAST_STATE.swap(i64::from(now), Relaxed);
    if before < 0 || before == i64::from(now) {
        return;
    }
    let targets = CAMERA.lock().map(|c| c.clone()).unwrap_or_default();
    let sent =
        dispatch(&targets, "OnPlayerCameraState", || vec![Arg::Int(before as i32), Arg::Int(now as i32)]);
    log::info!(target: "f4x::papyrus", "camera state {before} -> {now}: {sent} event(s)");
}

// ---- furniture ----

/// Scripts registered for furniture events (handle order). F4SE 0.6.23 keeps one
/// registration per script and ignores the filter (black-box: a registrant whose filter
/// is an unrelated form still gets every event, and the co-save holds no filter).
static FURNITURE: Mutex<Vec<Registrant>> = Mutex::new(Vec::new());
static REAL_PROCESS_EVENT: AtomicUsize = AtomicUsize::new(0);

fn register_for_furniture_event(call: &mut NativeCall) -> Result<(), String> {
    // F4SE crashes when the filter holds None; f4x registers (the filter is unused).
    let who = registrant(call)?;
    let mut all = FURNITURE.lock().map_err(|_| "poisoned")?;
    insert(&mut all, who);
    Ok(())
}

/// F4SE 0.6.23 keeps the registration whatever the filter (black-box: forms, the filter
/// it was registered with, Int, String and Bool values all leave it in place and in the
/// co-save; None crashes F4SE). f4x matches it, so saves and events stay the same.
fn unregister_for_furniture_event(call: &mut NativeCall) -> Result<(), String> {
    let who = registrant(call)?;
    log::debug!(target: "f4x::papyrus", "{} ({:#x}): UnregisterForFurnitureEvent keeps the registration, as F4SE does", who.script, who.handle);
    Ok(())
}

type ProcessEvent = unsafe extern "C" fn(*mut u8, *const u8, *mut u8) -> i32;

/// `PlayerControls`' `BSTEventSink<TESFurnitureEvent>::ProcessEvent`: see the event, then
/// let the game handle it.
unsafe extern "C" fn furniture_hook(this: *mut u8, event: *const u8, source: *mut u8) -> i32 {
    let _ = std::panic::catch_unwind(|| on_furniture_event(event));
    // SAFETY: the original slot value.
    unsafe {
        std::mem::transmute::<usize, ProcessEvent>(REAL_PROCESS_EVENT.load(Relaxed))(this, event, source)
    }
}

fn on_furniture_event(event: *const u8) {
    if event.is_null() || !mem::readable(event as usize, RE::TESFurnitureEvent::SIZE) {
        return;
    }
    let targets = FURNITURE.lock().map(|f| f.clone()).unwrap_or_default();
    if targets.is_empty() {
        return;
    }
    // SAFETY: a live TESFurnitureEvent (readable): two NiPointer<TESObjectREFR> and a type.
    let (actor, furniture, kind): (*mut u8, *mut u8, i32) = unsafe {
        (
            mem::read(event, RE::TESFurnitureEvent::actor),
            mem::read(event, RE::TESFurnitureEvent::targetFurniture),
            mem::read(event, RE::TESFurnitureEvent::r#type),
        )
    };
    let (Some(actor_form), furniture_form) =
        (f4x_engine::Form::checked(actor), f4x_engine::Form::checked(furniture))
    else {
        return;
    };
    let getting_up = kind == 1; // FurnitureEventType::kExit
    // Forms become script objects when the VM reads the arguments.
    let actor_value = PValue::Form(actor_form.as_ptr());
    let furniture_value = furniture_form.map_or(PValue::None, |f| PValue::Form(f.as_ptr()));
    let sent = dispatch(&targets, "OnFurnitureEvent", || {
        vec![
            Arg::Typed("Actor".into(), actor_value.clone()),
            Arg::Typed("ObjectReference".into(), furniture_value.clone()),
            Arg::Bool(getting_up),
        ]
    });
    log::debug!(target: "f4x::papyrus", "furniture event ({}): {sent} event(s)", if getting_up { "exit" } else { "enter" });
}

fn install_furniture_hook() {
    let game = f4x_engine::game();
    let sink = game
        .rtti()
        .vtables_of(RE::PlayerControls::RTTI)
        .find(|v| v.offset as usize == RE::PlayerControls::bases::BSTEventSink__);
    let Some(vtable) = sink else {
        log::warn!(target: "f4x::papyrus", "PlayerControls' furniture sink not found; no OnFurnitureEvent");
        return;
    };
    let slot = (game.base() + vtable.vtable_rva as usize + 8) as *mut usize; // ProcessEvent (slot 1)
    // SAFETY: slot 1 of a BSTEventSink<TESFurnitureEvent> vtable in .rdata.
    match unsafe { f4x_agent::hooks::patch_pointer(slot, furniture_hook as ProcessEvent as usize) } {
        Ok(real) => REAL_PROCESS_EVENT.store(real, Relaxed),
        Err(error) => log::warn!(target: "f4x::papyrus", "furniture sink: {error}"),
    }
}

// ---- lifecycle ----

/// Watch input, the camera and furniture events (once, at plugin load).
pub(crate) fn install() {
    f4x_agent::input::add_observer(scan_input);
    f4x_agent::pump::add_ticker(poll_camera);
    install_furniture_hook();
}

/// Forget every registration (before a load).
pub(crate) fn revert() {
    if let Ok(mut k) = KEYS.lock() {
        k.clear();
    }
    if let Ok(mut c) = CAMERA.lock() {
        c.clear();
    }
    if let Ok(mut f) = FURNITURE.lock() {
        f.clear();
    }
    ANY_KEYS.store(false, Relaxed);
    ANY_CAMERA.store(false, Relaxed);
    LAST_STATE.store(-1, Relaxed);
}

/// Write the `KEYR`, `CAMR` and `FRNR` sections into F4SE's host block.
pub(crate) fn save(host: &mut Vec<Chunk>) {
    regs::replace_section(host, regs::KEYS, key_entries());
    let camera = CAMERA.lock().map(|c| c.clone()).unwrap_or_default();
    regs::replace_chunk(host, regs::CAMERA, regs::write_registrants(&camera));
    let furniture = FURNITURE.lock().map(|f| f.clone()).unwrap_or_default();
    regs::replace_chunk(host, regs::FURNITURE, regs::write_furniture(&furniture));
}

/// Restore registrations from a co-save's host block.
pub(crate) fn load(host: &[Chunk]) {
    let keys = load_keys(host);
    if let Ok(mut k) = KEYS.lock() {
        ANY_KEYS.store(!keys.is_empty(), Relaxed);
        *k = keys;
    }
    let mut camera = Vec::new();
    if let Some((list, _)) = regs::chunk(host, regs::CAMERA).and_then(|d| regs::parse_registrants(d, 0)) {
        for r in list.into_iter().filter_map(remap) {
            insert(&mut camera, r);
        }
    }
    if let Ok(mut c) = CAMERA.lock() {
        ANY_CAMERA.store(!camera.is_empty(), Relaxed);
        LAST_STATE.store(-1, Relaxed); // the state after the load is the starting point
        *c = camera;
    }
    let mut furniture = Vec::new();
    for r in regs::chunk(host, regs::FURNITURE)
        .and_then(regs::parse_furniture)
        .unwrap_or_default()
        .into_iter()
        .filter_map(remap)
    {
        insert(&mut furniture, r);
    }
    if let Ok(mut f) = FURNITURE.lock() {
        *f = furniture;
    }
}
