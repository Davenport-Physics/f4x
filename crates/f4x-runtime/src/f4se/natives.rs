//! F4SE Papyrus natives, implemented on engine internals (`f4x_engine::papyrus`).
//!
//! The functions are declared native by F4SE's script files (signatures from F4SE's
//! distributed `.pex`). Behaviour follows CommonLibF4's MIT `F4SEStub/runtime` where it
//! exists: no `AddChange` on the zone setters, which the stub marks as divergent from
//! F4SE. Control registrations are our own design: they are saved in F4SE's co-save
//! format (`CTLR` + `REGS` + `REGE`, black-box) and dispatched from the engine's input
//! queue.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use f4x_cosave::regs::{self, Registrant};
use f4x_cosave::Chunk;
use f4x_engine::form_type::FormType;
use f4x_engine::layout::RE;
use f4x_engine::mem;
use f4x_engine::papyrus::{self, Arg, NativeCall, NativeSpec, Type, Vm};

fn form_type(signature: &str) -> FormType {
    FormType::from_signature(signature).unwrap_or(FormType(0))
}

fn zone_type() -> FormType {
    form_type("ECZN")
}

fn location_type() -> FormType {
    form_type("LCTN")
}

const ZONE_FLAGS: usize = RE::BGSEncounterZone::data + RE::ENCOUNTER_ZONE_DATA::flags;
const NEVER_RESET: u8 = 1 << 0;
const WORKSHOP: u8 = 1 << 3;

fn zone_flag(call: &mut NativeCall, bit: u8) -> Result<(), String> {
    let zone = call.self_form(zone_type()).ok_or("self is not an EncounterZone")?;
    // SAFETY: a live BGSEncounterZone.
    let flags: u8 = unsafe { mem::read(zone, ZONE_FLAGS) };
    call.ret_bool(flags & bit != 0);
    Ok(())
}

fn set_zone_flag(call: &mut NativeCall, bit: u8) -> Result<(), String> {
    let zone = call.self_form(zone_type()).ok_or("self is not an EncounterZone")?;
    let on = call.bool(0);
    // SAFETY: a live BGSEncounterZone; a one-byte flags field.
    unsafe {
        let p = zone.add(ZONE_FLAGS);
        *p = if on { *p | bit } else { *p & !bit };
    }
    Ok(())
}

fn get_encounter_zone(call: &mut NativeCall) -> Result<(), String> {
    let location = call.self_form(location_type()).ok_or("self is not a Location")?;
    let recursive = call.bool(0);
    let mut current = location;
    let mut zone: *mut u8 = std::ptr::null_mut();
    for _ in 0..64 {
        // SAFETY: a live BGSLocation: zone and parentLoc pointers.
        unsafe {
            zone = mem::read(current, RE::BGSLocation::zone);
            if !zone.is_null() || !recursive {
                break;
            }
            current = mem::read(current, RE::BGSLocation::parentLoc);
        }
        if current.is_null() {
            break;
        }
    }
    call.ret_form((!zone.is_null()).then_some(zone), zone_type());
    Ok(())
}

// ---- controls ----

/// Control name → registered scripts, in registration order.
static CONTROLS: Mutex<Vec<(String, Vec<Registrant>)>> = Mutex::new(Vec::new());
static ANY_CONTROLS: AtomicBool = AtomicBool::new(false);

fn registrant(call: &NativeCall) -> Result<Registrant, String> {
    let object = call.self_object().ok_or("no script object")?;
    let handle = papyrus::object_handle(object).ok_or("no handle")?;
    let script = papyrus::object_script_name(object).ok_or("no script name")?;
    Ok(Registrant { handle, script })
}

fn register_for_control(call: &mut NativeCall) -> Result<(), String> {
    let control = call.string(0);
    pool_order(&control);
    let who = registrant(call)?;
    let mut controls = CONTROLS.lock().map_err(|_| "poisoned")?;
    let entry = match controls.iter().position(|(name, _)| name.eq_ignore_ascii_case(&control)) {
        Some(i) => &mut controls[i].1,
        None => {
            controls.push((control.clone(), Vec::new()));
            &mut controls.last_mut().unwrap().1
        }
    };
    if !entry.contains(&who) {
        log::info!(target: "f4x::papyrus", "{} ({:#x}) registered for control {control:?}", who.script, who.handle);
        entry.push(who);
    }
    ANY_CONTROLS.store(true, Relaxed);
    Ok(())
}

fn unregister_for_control(call: &mut NativeCall) -> Result<(), String> {
    let control = call.string(0);
    let who = registrant(call)?;
    let mut controls = CONTROLS.lock().map_err(|_| "poisoned")?;
    for (name, list) in controls.iter_mut() {
        if name.eq_ignore_ascii_case(&control) {
            list.retain(|r| r != &who);
        }
    }
    controls.retain(|(_, list)| !list.is_empty());
    ANY_CONTROLS.store(!controls.is_empty(), Relaxed);
    Ok(())
}

/// Registration names, each pinned in the string pool: F4SE keeps a pool reference per
/// registered name and orders entries by the entry's address (black-box). Pinning keeps
/// the address stable, as it does for F4SE.
static PINNED: Mutex<Vec<(String, papyrus::FixedString)>> = Mutex::new(Vec::new());

fn pool_order(name: &str) -> usize {
    let Ok(mut pinned) = PINNED.lock() else { return usize::MAX };
    if let Some((_, s)) = pinned.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return s.as_raw() as usize;
    }
    match papyrus::FixedString::new(name) {
        Some(s) => {
            let address = s.as_raw() as usize;
            pinned.push((name.to_owned(), s));
            address
        }
        None => usize::MAX,
    }
}

/// The current control registrations as `REGS` entries, in F4SE's order: entries by the
/// name's string-pool address, registrants by handle.
pub(crate) fn control_entries() -> Vec<Vec<u8>> {
    let mut controls = CONTROLS.lock().map(|c| c.clone()).unwrap_or_default();
    controls.sort_by_key(|(name, _)| pool_order(name));
    controls
        .into_iter()
        .map(|(name, mut list)| {
            list.sort_by(|a, b| a.handle.cmp(&b.handle).then_with(|| a.script.cmp(&b.script)));
            regs::write_named(&name, &list)
        })
        .collect()
}

/// Forget all registrations (before a load or a new game).
pub(crate) fn revert() {
    if let Ok(mut c) = CONTROLS.lock() {
        c.clear();
    }
    if let Ok(mut e) = EXTERNAL.lock() {
        e.clear();
    }
    ANY_CONTROLS.store(false, Relaxed);
}

/// Restore control registrations from a co-save's host block. Handles carry a form id
/// in their low 32 bits, which is mapped to the current load order.
pub(crate) fn load_controls(host: &[Chunk]) {
    let mut restored = Vec::new();
    for chunk in regs::section(host, regs::CONTROLS) {
        let Some((name, list)) = regs::parse_named(&chunk.data) else { continue };
        pool_order(&name);
        let list: Vec<Registrant> = list
            .into_iter()
            .filter_map(|r| {
                let id = super::serialization::remap_form_id(r.handle as u32)?;
                Some(Registrant { handle: (r.handle & !0xFFFF_FFFF) | u64::from(id), script: r.script })
            })
            .collect();
        if !list.is_empty() {
            restored.push((name, list));
        }
    }
    let count: usize = restored.iter().map(|(_, l)| l.len()).sum();
    if count > 0 {
        log::info!(target: "f4x::papyrus", "restored {count} control registration(s)");
    }
    if let Ok(mut c) = CONTROLS.lock() {
        ANY_CONTROLS.store(!restored.is_empty(), Relaxed);
        *c = restored;
    }
}

/// Send `OnControlDown(control)` / `OnControlUp(control, held)` to every script
/// registered for `control`.
pub(crate) fn dispatch_control(control: &str, down: bool, held: f32) -> usize {
    let Some(vm) = f4x_agent::observe::virtual_machine() else { return 0 };
    let targets: Vec<(String, Registrant)> = CONTROLS
        .lock()
        .map(|c| {
            c.iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(control))
                .flat_map(|(name, list)| list.iter().map(move |r| (name.clone(), r.clone())))
                .collect()
        })
        .unwrap_or_default();
    let vm = Vm(vm as *mut _);
    let mut sent = 0;
    for (name, r) in targets {
        let (event, args) = if down {
            ("OnControlDown", vec![Arg::String(name.clone())])
        } else {
            ("OnControlUp", vec![Arg::String(name.clone()), Arg::Float(held)])
        };
        let ok = vm.dispatch_on_bound(r.handle, &r.script, event, args);
        log::info!(target: "f4x::papyrus", "{event}({name}) -> {} ({:#x}): {}", r.script, r.handle, if ok { "queued" } else { "refused" });
        if ok {
            sent += 1;
        }
    }
    sent
}

// ---- external events (ScriptObject.RegisterForExternalEvent, root.f4se.SendExternalEvent) ----
//
// Black-box (F4SE 0.6.23, docs/formats/cosave.md):
// - one registration per (event, handle, script). Registering again keeps the first
//   callback; Unregister removes it;
// - SendExternalEvent calls the callback on every registrant, in handle order;
// - arguments: every number becomes a Float, strings a String, booleans a Bool. Calls
//   whose types do not match fail silently;
// - saved in the EXEV section, entries by the event name's pool address, registrants by
//   handle.

static EXTERNAL: Mutex<Vec<(String, Vec<regs::Callback>)>> = Mutex::new(Vec::new());

fn register_for_external_event(call: &mut NativeCall) -> Result<(), String> {
    let (event, callback) = (call.string(0), call.string(1));
    pool_order(&event);
    let who = registrant(call)?;
    let mut all = EXTERNAL.lock().map_err(|_| "poisoned")?;
    let entry = match all.iter().position(|(name, _)| name.eq_ignore_ascii_case(&event)) {
        Some(i) => &mut all[i].1,
        None => {
            all.push((event.clone(), Vec::new()));
            &mut all.last_mut().unwrap().1
        }
    };
    if !entry.iter().any(|r| r.handle == who.handle && r.script == who.script) {
        log::info!(target: "f4x::papyrus", "{} ({:#x}) registered for external event {event:?} -> {callback}", who.script, who.handle);
        entry.push(regs::Callback { handle: who.handle, script: who.script, callback });
    }
    Ok(())
}

fn unregister_for_external_event(call: &mut NativeCall) -> Result<(), String> {
    let event = call.string(0);
    let who = registrant(call)?;
    let mut all = EXTERNAL.lock().map_err(|_| "poisoned")?;
    for (name, list) in all.iter_mut() {
        if name.eq_ignore_ascii_case(&event) {
            list.retain(|r| !(r.handle == who.handle && r.script == who.script));
        }
    }
    all.retain(|(_, list)| !list.is_empty());
    Ok(())
}

/// The registrants of `event`, in dispatch order.
pub(crate) fn external_registrants(event: &str) -> Vec<regs::Callback> {
    let mut out: Vec<regs::Callback> = EXTERNAL
        .lock()
        .map(|all| all.iter().filter(|(name, _)| name.eq_ignore_ascii_case(event)).flat_map(|(_, l)| l.clone()).collect())
        .unwrap_or_default();
    out.sort_by(|a, b| a.handle.cmp(&b.handle).then_with(|| a.script.cmp(&b.script)));
    out
}

/// Call `event`'s registered callbacks with `args` (already converted as F4SE does).
pub(crate) fn send_external_event(event: &str, args: &[Arg]) -> usize {
    let Some(vm) = f4x_agent::observe::virtual_machine() else { return 0 };
    let vm = Vm(vm as *mut _);
    let mut sent = 0;
    for r in external_registrants(event) {
        let call_args = args.iter().map(clone_arg).collect();
        if vm.dispatch_on_bound(r.handle, &r.script, &r.callback, call_args) {
            sent += 1;
        }
    }
    log::info!(target: "f4x::papyrus", "external event {event:?}: {sent} call(s)");
    sent
}

fn clone_arg(a: &Arg) -> Arg {
    match a {
        Arg::None | Arg::Object(_) => Arg::None,
        Arg::Bool(b) => Arg::Bool(*b),
        Arg::Int(i) => Arg::Int(*i),
        Arg::Float(f) => Arg::Float(*f),
        Arg::String(s) => Arg::String(s.clone()),
    }
}

pub(crate) fn external_entries() -> Vec<Vec<u8>> {
    let mut all = EXTERNAL.lock().map(|e| e.clone()).unwrap_or_default();
    all.sort_by_key(|(name, _)| pool_order(name));
    all.into_iter()
        .map(|(name, mut list)| {
            list.sort_by(|a, b| a.handle.cmp(&b.handle).then_with(|| a.script.cmp(&b.script)));
            regs::write_callbacks(&name, &list)
        })
        .collect()
}

pub(crate) fn load_external(host: &[Chunk]) {
    let mut restored = Vec::new();
    for chunk in regs::section(host, regs::EXTERNAL_EVENTS) {
        let Some((name, list)) = regs::parse_callbacks(&chunk.data) else { continue };
        pool_order(&name);
        let list: Vec<regs::Callback> = list
            .into_iter()
            .filter_map(|r| {
                let id = super::serialization::remap_form_id(r.handle as u32)?;
                Some(regs::Callback { handle: (r.handle & !0xFFFF_FFFF) | u64::from(id), ..r })
            })
            .collect();
        if !list.is_empty() {
            restored.push((name, list));
        }
    }
    if let Ok(mut e) = EXTERNAL.lock() {
        *e = restored;
    }
}

// ---- input ----

const BUTTON_EVENT: i32 = 0; // INPUT_EVENT_TYPE::kButton

/// Watch each frame's input list (through the agent's input hooks) for registered
/// controls.
pub(crate) fn install_input_hook() {
    f4x_agent::input::add_observer(scan_input);
}

fn scan_input(head: *const u8) {
    if !ANY_CONTROLS.load(Relaxed) {
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
            // SAFETY: a ButtonEvent: IDEvent::strUserEvent, value, heldDownSecs.
            let (user_event, value, held): (*const u8, f32, f32) =
                unsafe { (mem::read(event, RE::IDEvent::strUserEvent), mem::read(event, RE::ButtonEvent::value), mem::read(event, RE::ButtonEvent::heldDownSecs)) };
            let pressed = value != 0.0 && held == 0.0;
            let released = value == 0.0 && held != 0.0;
            if (pressed || released) && !user_event.is_null() {
                // SAFETY: a pool entry.
                if let Some(name) = unsafe { mem::fixed_string(user_event) }
                    && !name.is_empty()
                    && is_registered(&name)
                {
                    dispatch_control(&name, pressed, held);
                }
            }
        }
        // SAFETY: as above.
        event = unsafe { mem::read(event, RE::InputEvent::next) };
    }
}

fn is_registered(control: &str) -> bool {
    CONTROLS.lock().is_ok_and(|c| c.iter().any(|(name, _)| name.eq_ignore_ascii_case(control)))
}

// ---- binding ----

static NATIVES: [NativeSpec; 9] = [
    NativeSpec { script: "EncounterZone", name: "IsWorkshop", params: &[], ret: Type::Bool, is_static: false, handler: |c| zone_flag(c, WORKSHOP) },
    NativeSpec {
        script: "EncounterZone",
        name: "SetWorkshop",
        params: &[("ws", Type::Bool)],
        ret: Type::None,
        is_static: false,
        handler: |c| set_zone_flag(c, WORKSHOP),
    },
    NativeSpec { script: "EncounterZone", name: "IsNeverResetable", params: &[], ret: Type::Bool, is_static: false, handler: |c| zone_flag(c, NEVER_RESET) },
    NativeSpec {
        script: "EncounterZone",
        name: "SetNeverResetable",
        params: &[("resetable", Type::Bool)],
        ret: Type::None,
        is_static: false,
        handler: |c| set_zone_flag(c, NEVER_RESET),
    },
    NativeSpec {
        script: "Location",
        name: "GetEncounterZone",
        params: &[("recursive", Type::Bool)],
        ret: Type::Object("EncounterZone"),
        is_static: false,
        handler: get_encounter_zone,
    },
    NativeSpec {
        script: "ScriptObject",
        name: "RegisterForControl",
        params: &[("control", Type::String)],
        ret: Type::None,
        is_static: false,
        handler: register_for_control,
    },
    NativeSpec {
        script: "ScriptObject",
        name: "UnregisterForControl",
        params: &[("control", Type::String)],
        ret: Type::None,
        is_static: false,
        handler: unregister_for_control,
    },
    NativeSpec {
        script: "ScriptObject",
        name: "RegisterForExternalEvent",
        params: &[("eventName", Type::String), ("callback", Type::String)],
        ret: Type::None,
        is_static: false,
        handler: register_for_external_event,
    },
    NativeSpec {
        script: "ScriptObject",
        name: "UnregisterForExternalEvent",
        params: &[("eventName", Type::String)],
        ret: Type::None,
        is_static: false,
        handler: unregister_for_external_event,
    },
];

/// Bind f4x's F4SE natives. Scripts that do not declare them (no F4SE script files
/// installed) simply refuse the binding.
pub(crate) fn bind_all(vm: usize) {
    let vm = Vm(vm as *mut _);
    let mut bound = Vec::new();
    for spec in &NATIVES {
        match papyrus::bind(vm, spec) {
            Ok(()) => bound.push(papyrus::describe(spec)),
            Err(error) => log::info!(target: "f4x::papyrus", "{} not bound: {error}", papyrus::describe(spec)),
        }
    }
    log::info!(target: "f4x::papyrus", "bound {} F4SE native(s): {}", bound.len(), bound.join(", "));
}
