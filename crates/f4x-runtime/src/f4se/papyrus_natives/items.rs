//! Actor, ActorBase, Armor, ArmorAddon, HeadPart, MatSwap, EquipSlot, Perk, WaterType, MiscObject, Component, ConstructibleObject.
//!
//! Behaviour reference: CommonLibF4 `F4SEStub/runtime/src/Papyrus` (MIT), checked against
//! real F4SE with `tools/f4se_natives_check.py` and targeted black-box calls:
//!
//! - Empty arrays come back as None (F4SE never returns an empty array).
//! - Getters with a bad `self` or argument return the type's default; setters do nothing.
//! - `Actor.IsProtected` (not in F4SEStub) is the actor's `kProtected` flag: it follows
//!   `ActorBase.SetProtected` for the loaded player, not for unloaded actors (black-box).
//! - `Actor.GetWornItem` returns the (empty) struct for an actor without a biped; only an
//!   out-of-range slot gives None. `GetWornItemMods` reads the inventory stack of the
//!   slot's own item (F4SEStub takes the first equipped stack of any item).
//! - Natives run on VM threads: `Actor.QueueUpdate` (`Actor::Reset3D`) and the
//!   `AddChange` of `ActorBase.SetBodyWeight` are posted to the main thread.
//! - `MiscObject.SetMiscComponents` replaces the list (F4SEStub appends) and keeps None
//!   components, which `GetMiscComponents` returns as None objects.
//! - `WaterType.SetConsumeSpell`/`SetContactSpell` accept None (F4SEStub refuses it);
//!   the other form setters ignore None, as F4SEStub does.
//!
//! Layouts are the generated CommonLibF4 tables (`f4x_engine::layout`); the few that are
//! template or bit-field internals cite their header in `provenance/natives-items.jsonl`.

use std::ffi::c_void;
use std::mem::{size_of, transmute, transmute_copy};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};

use f4x_engine::address::resolve;
use f4x_engine::form::Form;
use f4x_engine::layout::RE;
use f4x_engine::mem;
use f4x_engine::papyrus::{self, FixedString, NativeCall};
use f4x_engine::papyrus_value::PValue;
use f4x_engine::FormId;
use f4x_engine_sys::addresses as addr;

use super::Entry;

// ---------------------------------------------------------------- memory helpers

/// A `T` at `base + offset`, if readable.
fn rd<T: Copy>(base: *const u8, offset: usize) -> Option<T> {
    let p = (base as usize).checked_add(offset)?;
    // SAFETY: checked readable just before.
    mem::readable(p, size_of::<T>()).then(|| unsafe { mem::read(base, offset) })
}

fn rd_ptr(base: *const u8, offset: usize) -> *mut u8 {
    rd::<*mut u8>(base, offset).unwrap_or(null_mut())
}

/// The form a pointer field holds (checked by RTTI).
fn form_at(base: *const u8, offset: usize) -> Option<*mut u8> {
    Form::checked(rd_ptr(base, offset)).map(Form::as_ptr)
}

/// # Safety
/// `base + offset` must be a writable `T` of a live engine object.
unsafe fn wr<T: Copy>(base: *mut u8, offset: usize, value: T) {
    // SAFETY: caller contract.
    unsafe { base.add(offset).cast::<T>().write_unaligned(value) }
}

/// A virtual function of `this` by slot.
///
/// # Safety
/// `this` must be a live polymorphic object whose vtable has `slot`.
unsafe fn vfn<F: Copy>(this: *const u8, slot: usize) -> F {
    // SAFETY: caller contract.
    unsafe {
        let vtable: *const usize = mem::read(this, 0);
        transmute_copy(&*vtable.add(slot))
    }
}

fn is(p: *mut u8, rtti: &'static str) -> bool {
    f4x_engine::game().is_instance_of(p as usize, rtti)
}

fn pform(p: Option<*mut u8>) -> PValue {
    p.map_or(PValue::None, PValue::Form)
}

/// `(data, len)` of a `BSTArray` at `array` (data at +0, size at +0x10), its `len`
/// elements of `elem` bytes readable.
fn array_view(array: *const u8, elem: usize) -> Option<(*mut u8, usize)> {
    let data: *mut u8 = rd(array, 0)?;
    let len = rd::<u32>(array, 0x10)? as usize;
    if data.is_null() || len == 0 {
        return Some((data, 0));
    }
    (len <= 1 << 20 && mem::readable(data as usize, len * elem)).then_some((data, len))
}

/// The forms in a `BSTArray<T*>` at `base + offset`, None for null/stale entries.
fn form_array(base: *const u8, offset: usize) -> Vec<PValue> {
    let Some((data, len)) = array_view(base.wrapping_add(offset), 8) else { return Vec::new() };
    // SAFETY: `len` readable pointers.
    (0..len).map(|i| pform(Form::checked(unsafe { mem::read(data, i * 8) }).map(Form::as_ptr))).collect()
}

/// Game-heap allocation (`MemoryManager::Allocate(size, 0, false)`, what `RE::malloc`
/// and the engine's heap containers use), zeroed.
fn heap_alloc(size: usize) -> Option<*mut u8> {
    let singleton = resolve(&addr::MEMORY_MANAGER_SINGLETON)?;
    let allocate = resolve(&addr::MEMORY_MANAGER_ALLOCATE)?;
    // SAFETY: MemoryManager::GetSingleton() / Allocate(this, size, alignment, alignmentRequired).
    let p = unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut c_void, usize, u32, bool) -> *mut u8>(allocate)(manager, size, 0, false)
    };
    if p.is_null() {
        return None;
    }
    // SAFETY: a fresh allocation of `size` bytes.
    unsafe { p.write_bytes(0, size) };
    Some(p)
}

/// Free a game-heap block (`MemoryManager::Deallocate(p, false)`, `RE::free`). Leaks it
/// if the function is not found.
fn heap_free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let (Some(singleton), Some(deallocate)) = (resolve(&addr::MEMORY_MANAGER_SINGLETON), resolve(&addr::MEMORY_MANAGER_DEALLOCATE)) else {
        return;
    };
    // SAFETY: MemoryManager::Deallocate(this, void*, alignmentRequired) on a block it allocated.
    unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut c_void, *mut u8, bool)>(deallocate)(manager, p, false);
    }
}

/// CRC-32 (reflected, polynomial 0xEDB88320, initial value 0, no final xor): the
/// engine's `BSCRC32` (CommonLibF4 `CRC.h` `GenerateCRC32`).
fn bs_crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    data.iter().fold(0u32, |hash, &b| (hash >> 8) ^ TABLE[((hash ^ u32::from(b)) & 0xFF) as usize])
}

/// `TESForm::GetFormByID`: a hashed lookup in the global form map
/// (`BSTHashMap<u32, TESForm*>`: 0x18-byte entries {key, form, next}, chains end at the
/// sentinel; same on NG 1.11.240, re: GetFormByID 0x311b80). Falls back to a scan of the
/// map if the chain does not have it.
fn form_by_id(id: u32) -> Option<*mut u8> {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let hashed = || -> Option<Option<*mut u8>> {
        let global = resolve(&addr::ALL_FORMS)?;
        let map = rd_ptr(global as *const u8, 0);
        let capacity = rd::<u32>(map, table::CAPACITY)? as usize;
        let entries = rd_ptr(map, table::ENTRIES);
        let sentinel = rd_ptr(map, table::SENTINEL);
        if entries.is_null() || !capacity.is_power_of_two() {
            return None;
        }
        let mut e = entries.wrapping_add((bs_crc32(&id.to_le_bytes()) as usize & (capacity - 1)) * 0x18);
        for _ in 0..capacity {
            let next = rd_ptr(e, 0x10);
            if next.is_null() {
                return Some(None);
            }
            if rd::<u32>(e, 0)? == id {
                return Some(form_at(e, 8));
            }
            if next == sentinel {
                return Some(None);
            }
            e = next;
        }
        None
    };
    if let Some(Some(form)) = hashed() {
        return Some(form);
    }
    let scanned = f4x_engine::data::form_by_id(FormId(id)).map(Form::as_ptr);
    if scanned.is_some() && !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(target: "f4x::papyrus", "form {id:08X}: hashed allForms lookup missed, found by scan");
    }
    scanned
}

// ---------------------------------------------------------------- call helpers

/// `self` as an engine object of class `rtti`.
fn self_as(c: &NativeCall, rtti: &'static str) -> Result<*mut u8, String> {
    match c.self_value() {
        PValue::Form(p) if is(p, rtti) => Ok(p),
        _ => Err(format!("self is not a {rtti}")),
    }
}

/// Argument `i` as a form of class `rtti`.
fn arg_as(c: &NativeCall, i: usize, rtti: &'static str) -> Option<*mut u8> {
    match c.value(i) {
        PValue::Form(p) if is(p, rtti) => Some(p),
        _ => None,
    }
}

fn ret_form(c: &mut NativeCall, form: Option<*mut u8>) -> Result<(), String> {
    match form {
        Some(p) => c.ret_value(&PValue::Form(p)),
        None => Ok(()),
    }
}

/// An array result: None when empty, as F4SE returns.
fn ret_array(c: &mut NativeCall, items: Vec<PValue>) -> Result<(), String> {
    if items.is_empty() { Ok(()) } else { c.ret_value(&PValue::Array(items)) }
}

/// A struct field by name (field names compare case-insensitively, like the VM's).
fn field<'a>(fields: &'a [(String, PValue)], name: &str) -> Option<&'a PValue> {
    fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

fn field_f32(fields: &[(String, PValue)], name: &str) -> Option<f32> {
    match field(fields, name)? {
        PValue::Float(f) => Some(*f),
        PValue::Int(i) => Some(*i as f32),
        _ => None,
    }
}

fn field_i32(fields: &[(String, PValue)], name: &str) -> Option<i32> {
    match field(fields, name)? {
        PValue::Int(i) => Some(*i),
        _ => None,
    }
}

fn field_str(fields: &[(String, PValue)], name: &str) -> Option<String> {
    match field(fields, name)? {
        PValue::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn field_form(fields: &[(String, PValue)], name: &str) -> Option<*mut u8> {
    match field(fields, name)? {
        PValue::Form(p) => Form::checked(*p).map(Form::as_ptr),
        _ => None,
    }
}

/// The elements of an array argument as struct field lists (None elements: no fields).
fn struct_array(c: &NativeCall, i: usize) -> Vec<Vec<(String, PValue)>> {
    match c.value(i) {
        PValue::Array(items) => items
            .into_iter()
            .map(|v| match v {
                PValue::Struct(fields) => fields,
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Set a form pointer field from argument 0, which must be a form of class `rtti`
/// (F4SEStub: "<arg> cannot be a none form").
fn set_form_field(c: &mut NativeCall, self_rtti: &'static str, offset: usize, arg_rtti: &'static str) -> Result<(), String> {
    let this = self_as(c, self_rtti)?;
    let value = arg_as(c, 0, arg_rtti).ok_or("the argument cannot be a none form")?;
    // SAFETY: a live form of the class; a pointer field.
    unsafe { wr(this, offset, value) };
    Ok(())
}

fn get_form_field(c: &mut NativeCall, self_rtti: &'static str, offset: usize) -> Result<(), String> {
    let this = self_as(c, self_rtti)?;
    ret_form(c, form_at(this, offset))
}

// ---------------------------------------------------------------- engine calls

/// `BSPointerHandleManagerInterface<TESObjectREFR>::GetSmartPointer`: the reference
/// behind a handle (the smart pointer's reference is released again).
fn refr_from_handle(handle: u32) -> Option<*mut u8> {
    if handle == 0 {
        return None;
    }
    let get = resolve(&addr::REFR_HANDLE_GET_SMART_POINTER)?;
    let mut out: *mut u8 = null_mut();
    // SAFETY: static bool GetSmartPointer(const ObjectRefHandle&, NiPointer<TESObjectREFR>&).
    let ok = unsafe { transmute::<usize, unsafe extern "C" fn(*const u32, *mut *mut u8) -> bool>(get)(&handle, &mut out) };
    if out.is_null() {
        return None;
    }
    release_refr(out);
    if ok { Form::checked(out).map(Form::as_ptr) } else { None }
}

/// `NiPointer<TESObjectREFR>` release: `BSHandleRefObject::DecRefCount` (count in the low
/// 10 bits; `DeleteThis` at zero).
fn release_refr(refr: *mut u8) {
    let object = refr.wrapping_add(RE::TESObjectREFR::bases::BSHandleRefObject);
    // SAFETY: a live reference: NiRefObject::refCount is a u32, updated atomically.
    let count = unsafe { &*(object.add(RE::NiRefObject::refCount) as *const AtomicU32) };
    if count.fetch_sub(1, Ordering::AcqRel).wrapping_sub(1) & 0x3FF == 0 {
        // SAFETY: NiRefObject::DeleteThis on the object whose last reference we held.
        unsafe { vfn::<unsafe extern "C" fn(*mut u8)>(object, RE::NiRefObject::vtbl::DeleteThis)(object) };
    }
}

/// Run `f` holding a `BSReadWriteLock` for reading (lock_read; unlock_read is an atomic
/// decrement of `_lock` at +4). Without the engine function, runs unlocked.
fn with_read_lock<R>(lock: *mut u8, f: impl FnOnce() -> R) -> R {
    let Some(lock_read) = resolve(&addr::RW_LOCK_LOCK_READ) else { return f() };
    // SAFETY: BSReadWriteLock::lock_read(this).
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8)>(lock_read)(lock) };
    let r = f();
    // SAFETY: BSReadWriteLock::_lock (u32 at +4), held for reading by us.
    unsafe { (*(lock.add(RE::BSReadWriteLock::_lock) as *const AtomicU32)).fetch_sub(1, Ordering::AcqRel) };
    r
}

// ---------------------------------------------------------------- Actor
//
// The values below are the same on NG 1.11.240 (clf4-ng `BIPED_OBJECT.h`:56,
// `ActorState.h`:50, `Actor.h`:116, `BGSInventoryItem.h`:28, `EXTRA_DATA_TYPE.h`; re: the
// protected/essential setter 0xc5b2b0 sets bit 19 of Actor+0x43C).

const BIPED_TOTAL: i32 = 44;
/// `ActorState` bit-field word at +0x0C: `interactingState` is bits 18..19.
const ACTOR_STATE_WORD: usize = 0x0C;
const INTERACTING_SHIFT: u32 = 18;
/// `Actor::BOOL_FLAGS::kProtected` (CommonLibF4 community `Actor.h`; `boolFlags` is the
/// u32 the generated table calls `flags`).
const ACTOR_PROTECTED: u32 = 1 << 19;
/// `BGSInventoryItem::Stack::Flag::kSlotMask`.
const STACK_EQUIPPED: u16 = 0b111;
/// `EXTRA_DATA_TYPE::kObjectInstance`.
const EXTRA_OBJECT_INSTANCE: u8 = 53;

fn actor(c: &NativeCall) -> Result<*mut u8, String> {
    self_as(c, RE::Actor::RTTI)
}

fn get_furniture_reference(c: &mut NativeCall) -> Result<(), String> {
    let actor = actor(c)?;
    let process = rd_ptr(actor, RE::Actor::currentProcess);
    let middle_high = if process.is_null() { null_mut() } else { rd_ptr(process, RE::AIProcess::middleHigh) };
    if middle_high.is_null() {
        return Ok(());
    }
    let state: u32 = rd(actor, RE::Actor::bases::ActorState + ACTOR_STATE_WORD).unwrap_or(0);
    let interacting = (state >> INTERACTING_SHIFT) & 0b11 != 0;
    let field = if interacting { RE::MiddleHighProcessData::currentFurniture } else { RE::MiddleHighProcessData::occupiedFurniture };
    let handle: u32 = rd(middle_high, field).unwrap_or(0);
    ret_form(c, refr_from_handle(handle))
}

/// The `BipedAnim` to read: the player's first-person one if asked, else the actor's.
fn biped(actor: *mut u8, first_person: bool) -> *mut u8 {
    if first_person && Form::checked(actor).is_some_and(|f| f.id() == FormId(0x14)) && is(actor, RE::PlayerCharacter::RTTI) {
        rd_ptr(actor, RE::PlayerCharacter::firstPersonBipedAnim)
    } else {
        rd_ptr(actor, RE::Actor::biped)
    }
}

/// `BipedAnim::object[slot]`, if the biped is readable.
fn biped_object(biped: *mut u8, slot: i32) -> Option<*mut u8> {
    if biped.is_null() || !(0..BIPED_TOTAL).contains(&slot) {
        return None;
    }
    let object = biped.wrapping_add(RE::BipedAnim::object + slot as usize * RE::BIPOBJECT::SIZE);
    mem::readable(object as usize, RE::BIPOBJECT::SIZE).then_some(object)
}

fn get_worn_item(c: &mut NativeCall) -> Result<(), String> {
    let actor = actor(c)?;
    let (slot, first_person) = (c.int(0), c.bool(1));
    if !(0..BIPED_TOTAL).contains(&slot) {
        return Err("slotIndex out of range".into());
    }
    // Without a biped (actor not loaded) F4SE still returns the struct, all fields empty.
    let object = biped_object(biped(actor, first_person), slot);
    let field = |offset: usize| object.and_then(|o| form_at(o, offset));
    let item = field(RE::BIPOBJECT::parent + RE::BGSObjectInstance::object);
    let model = field(RE::BIPOBJECT::armorAddon);
    let texture = field(RE::BIPOBJECT::skinTexture);
    let part = object.map_or(null_mut(), |o| rd_ptr(o, RE::BIPOBJECT::part));
    let (mut model_name, mut swap) = (String::new(), None);
    if !part.is_null() && is(part, RE::TESModel::RTTI) {
        // SAFETY: a live TESModel: GetModel() -> const char*, GetAsModelMaterialSwap().
        unsafe {
            model_name = mem::c_str(vfn::<unsafe extern "C" fn(*mut u8) -> *const u8>(part, RE::TESModel::vtbl::GetModel)(part));
            let material = vfn::<unsafe extern "C" fn(*mut u8) -> *mut u8>(part, RE::TESModel::vtbl::GetAsModelMaterialSwap)(part);
            if !material.is_null() {
                swap = form_at(material, RE::BGSModelMaterialSwap::swapForm);
            }
        }
    }
    c.ret_value(&PValue::Struct(vec![
        ("item".into(), pform(item)),
        ("model".into(), pform(model)),
        ("modelName".into(), PValue::String(model_name)),
        ("materialSwap".into(), pform(swap)),
        ("texture".into(), pform(texture)),
    ]))
}

/// The `BGSObjectInstanceExtra` in an `ExtraDataList` (`BaseExtraList` at +8: head at +0,
/// type-presence bits at +0x10; each `BSExtraData` has next at +8 and type at +0x12).
fn object_instance_extra(extra_list: *mut u8) -> Option<*mut u8> {
    let base = extra_list.wrapping_add(RE::ExtraDataList::extraData);
    let flags = rd_ptr(base, RE::BaseExtraList::_flags);
    let bit: u8 = rd(flags, usize::from(EXTRA_OBJECT_INSTANCE / 8))?;
    if bit & (1 << (EXTRA_OBJECT_INSTANCE % 8)) == 0 {
        return None;
    }
    with_read_lock(extra_list.wrapping_add(RE::ExtraDataList::extraRWLock), || {
        let mut node = rd_ptr(base, RE::BaseExtraList::_head);
        for _ in 0..512 {
            if node.is_null() {
                return None;
            }
            if rd::<u8>(node, RE::BSExtraData::r#type)? == EXTRA_OBJECT_INSTANCE {
                return Some(node);
            }
            node = rd_ptr(node, RE::BSExtraData::next);
        }
        None
    })
}

/// The mods of an object instance (`BSTDataBuffer<1>` block 0 of `ObjectIndexData`,
/// 8 bytes each: form id at +0).
fn instance_mods(instance: *mut u8) -> Vec<PValue> {
    let values = rd_ptr(instance, RE::BGSObjectInstanceExtra::values);
    let buffer = rd_ptr(values, 0);
    let Some(size) = rd::<u32>(values, 8) else { return Vec::new() };
    if buffer.is_null() || size > 1 << 16 || !mem::readable(buffer as usize, size as usize + 4) {
        return Vec::new();
    }
    // The block table follows the data: {size: 24 bits, id: 8 bits}, 0xFF id ends it.
    // SAFETY: readable (checked).
    let block: u32 = unsafe { mem::read(buffer, size as usize) };
    if block >> 24 != 0 {
        return Vec::new();
    }
    let bytes = (block & 0x00FF_FFFF).min(size) as usize;
    (0..bytes / 8)
        // SAFETY: within the readable buffer.
        .filter_map(|i| form_by_id(unsafe { mem::read::<u32>(buffer, i * 8) }))
        .filter(|&f| is(f, RE::BGSMod::Attachment::Mod::RTTI))
        .map(PValue::Form)
        .collect()
}

fn get_worn_item_mods(c: &mut NativeCall) -> Result<(), String> {
    let actor = actor(c)?;
    let slot = c.int(0);
    if !(0..BIPED_TOTAL).contains(&slot) {
        return Err("slotIndex out of range".into());
    }
    let object = biped_object(rd_ptr(actor, RE::Actor::biped), slot)
        .and_then(|o| form_at(o, RE::BIPOBJECT::parent + RE::BGSObjectInstance::object));
    let inventory = rd_ptr(actor, RE::TESObjectREFR::inventoryList);
    let (Some(object), false) = (object, inventory.is_null()) else { return Ok(()) };
    let mods = with_read_lock(inventory.wrapping_add(RE::BGSInventoryList::rwLock), || {
        let (data, len) = array_view(inventory.wrapping_add(RE::BGSInventoryList::data), RE::BGSInventoryItem::SIZE)?;
        for i in 0..len {
            let item = data.wrapping_add(i * RE::BGSInventoryItem::SIZE);
            if rd_ptr(item, RE::BGSInventoryItem::object) != object {
                continue;
            }
            let mut stack = rd_ptr(item, RE::BGSInventoryItem::stackData);
            for _ in 0..4096 {
                if stack.is_null() {
                    break;
                }
                let flags: u16 = rd(stack, RE::BGSInventoryItem::Stack::flags).unwrap_or(0);
                let extra = rd_ptr(stack, RE::BGSInventoryItem::Stack::extra);
                if flags & STACK_EQUIPPED != 0 && !extra.is_null() {
                    return Some(object_instance_extra(extra).map(instance_mods).unwrap_or_default());
                }
                stack = rd_ptr(stack, RE::BGSInventoryItem::Stack::nextStack);
            }
        }
        None
    });
    ret_array(c, mods.unwrap_or_default())
}

fn queue_update(c: &mut NativeCall) -> Result<(), String> {
    let actor = actor(c)?;
    let id = Form::checked(actor).ok_or("not a form")?.id();
    let (do_equipment, flags) = (c.bool(0), c.int(1) as u32);
    // Actor::Reset3D(reloadAll, additionalFlags, queueReset, excludeFlags), as F4SEStub
    // calls it, on the main thread (the actor is looked up again there).
    f4x_agent::pump::post(move || {
        let Some(actor) = form_by_id(id.0).filter(|&p| is(p, RE::Actor::RTTI)) else { return };
        if let Some(reset) = resolve(&addr::ACTOR_RESET_3D) {
            // SAFETY: Actor::Reset3D(this, bool, u32, bool, u32) on a live actor.
            unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, bool, u32, bool, u32)>(reset)(actor, do_equipment, 0, true, flags) };
        }
    });
    Ok(())
}

fn is_protected(c: &mut NativeCall) -> Result<(), String> {
    c.ret_bool(false);
    let actor = actor(c)?;
    let flags: u32 = rd(actor, RE::Actor::flags).unwrap_or(0);
    c.ret_bool(flags & ACTOR_PROTECTED != 0);
    Ok(())
}

// ---------------------------------------------------------------- ActorBase

/// `ActorBase:BodyWeight` fields in `TESNPC::morphWeight` order (x, y, z).
const BODY_WEIGHT: [&str; 3] = ["thin", "muscular", "large"];
/// `CHANGE_TYPE::kNPCBodyScales` (same on NG: clf4-ng `CHANGE_TYPES.h`:91).
const CHANGE_NPC_BODY_SCALES: u32 = 1 << 14;

fn npc(c: &NativeCall) -> Result<*mut u8, String> {
    self_as(c, RE::TESNPC::RTTI)
}

fn form_race(npc: *mut u8) -> *mut u8 {
    rd_ptr(npc, RE::TESNPC::bases::TESRaceForm + RE::TESRaceForm::formRace)
}

/// The engine's alternate head-part lists (`BSTHashMap<const TESNPC*,
/// BSTArray<BGSHeadPart*>>`): entries at +0x28, capacity at +0x0C; each entry is the key,
/// the array (0x18) and `next` (non-null when used), 0x28 bytes. Same on NG.
fn alternate_head_parts(npc: *mut u8) -> Option<*mut u8> {
    let map = resolve(&addr::NPC_ALTERNATE_HEAD_PARTS)?.checked_sub(8)? as *const u8;
    let capacity = rd::<u32>(map, 0x0C)? as usize;
    let entries = rd_ptr(map, 0x28);
    if entries.is_null() || capacity > 1 << 20 || !mem::readable(entries as usize, capacity * 0x28) {
        return None;
    }
    (0..capacity).map(|i| entries.wrapping_add(i * 0x28)).find(|&e| rd_ptr(e, 0x20) as usize != 0 && rd_ptr(e, 0) == npc).map(|e| e.wrapping_add(8))
}

/// `TESNPC::UsingAlternateHeadPartList` (CommonLibF4 `TESBoundAnimObjects.cpp`).
fn using_alternate_head_parts(npc: *mut u8) -> bool {
    let is_player_base = Form::checked(npc).is_some_and(|f| f.id() == FormId(7));
    match f4x_engine::player() {
        Some(player) if is_player_base => {
            let chargen = rd_ptr(player.form().as_ptr(), RE::PlayerCharacter::charGenRace);
            let player_npc = player.base_object().map_or(null_mut(), Form::as_ptr);
            !chargen.is_null() && chargen != form_race(npc) && alternate_head_parts(player_npc).is_some()
        }
        _ => {
            let original = rd_ptr(npc, RE::TESNPC::originalRace);
            !original.is_null() && original != form_race(npc)
        }
    }
}

fn has_head_part_overlays(c: &mut NativeCall) -> Result<(), String> {
    c.ret_bool(false);
    let npc = npc(c)?;
    c.ret_bool(using_alternate_head_parts(npc));
    Ok(())
}

fn get_template(c: &mut NativeCall) -> Result<(), String> {
    let npc = npc(c)?;
    let top_most = c.bool(0);
    let mut template = form_at(npc, RE::TESNPC::faceNPC);
    if top_most {
        for _ in 0..64 {
            match template.and_then(|t| form_at(t, RE::TESNPC::faceNPC)) {
                Some(next) => template = Some(next),
                None => break,
            }
        }
    }
    ret_form(c, template)
}

fn get_body_weight(c: &mut NativeCall) -> Result<(), String> {
    let npc = npc(c)?;
    let fields = BODY_WEIGHT
        .iter()
        .enumerate()
        .map(|(i, name)| ((*name).to_owned(), PValue::Float(rd::<f32>(npc, RE::TESNPC::morphWeight + 4 * i).unwrap_or(0.0))))
        .collect();
    c.ret_value(&PValue::Struct(fields))
}

fn set_body_weight(c: &mut NativeCall) -> Result<(), String> {
    let npc = npc(c)?;
    let fields = match c.value(0) {
        PValue::Struct(fields) => fields,
        _ => Vec::new(),
    };
    for (i, name) in BODY_WEIGHT.iter().enumerate() {
        if let Some(v) = field_f32(&fields, name) {
            // SAFETY: a live TESNPC: morphWeight is a NiPoint3 (three floats).
            unsafe { wr(npc, RE::TESNPC::morphWeight + 4 * i, v) };
        }
    }
    // The save-change flag goes through the engine's change map: set it on the main
    // thread (natives run on VM threads), the form looked up again there.
    let id = Form::checked(npc).ok_or("not a form")?.id();
    f4x_agent::pump::post(move || {
        let Some(npc) = form_by_id(id.0).filter(|&p| is(p, RE::TESNPC::RTTI)) else { return };
        // SAFETY: TESForm::AddChange(u32 changeFlags) virtual on a live form.
        unsafe { vfn::<unsafe extern "C" fn(*mut u8, u32) -> bool>(npc, RE::TESForm::vtbl::AddChange)(npc, CHANGE_NPC_BODY_SCALES) };
    });
    Ok(())
}

fn get_outfit(c: &mut NativeCall) -> Result<(), String> {
    let npc = npc(c)?;
    let field = if c.bool(0) { RE::TESNPC::sleepOutfit } else { RE::TESNPC::defOutfit };
    ret_form(c, form_at(npc, field))
}

fn get_head_parts(c: &mut NativeCall) -> Result<(), String> {
    let npc = npc(c)?;
    let overlays = c.bool(0);
    let parts = if overlays && using_alternate_head_parts(npc) {
        alternate_head_parts(npc).map(|array| form_array(array, 0)).unwrap_or_default()
    } else {
        let list = rd_ptr(npc, RE::TESNPC::headParts);
        let count = rd::<i8>(npc, RE::TESNPC::numHeadParts).unwrap_or(0).max(0) as usize;
        if list.is_null() || !mem::readable(list as usize, count * 8) {
            Vec::new()
        } else {
            // SAFETY: `count` readable pointers.
            (0..count).map(|i| pform(Form::checked(unsafe { mem::read(list, i * 8) }).map(Form::as_ptr))).collect()
        }
    };
    ret_array(c, parts)
}

// ---------------------------------------------------------------- Armor, ArmorAddon, EquipSlot

fn get_armor_addons(c: &mut NativeCall) -> Result<(), String> {
    let armor = self_as(c, RE::TESObjectARMO::RTTI)?;
    use RE::TESObjectARMO::ArmorAddon as AA;
    let Some((data, len)) = array_view(armor.wrapping_add(RE::TESObjectARMO::modelArray), AA::SIZE) else { return Ok(()) };
    let addons = (0..len).map(|i| pform(form_at(data, i * AA::SIZE + AA::armorAddon))).collect();
    ret_array(c, addons)
}

fn get_additional_races(c: &mut NativeCall) -> Result<(), String> {
    let addon = self_as(c, RE::TESObjectARMA::RTTI)?;
    ret_array(c, form_array(addon, RE::TESObjectARMA::additionalRaces))
}

fn get_parents(c: &mut NativeCall) -> Result<(), String> {
    let slot = self_as(c, RE::BGSEquipSlot::RTTI)?;
    ret_array(c, form_array(slot, RE::BGSEquipSlot::parentSlots))
}

// ---------------------------------------------------------------- HeadPart

/// `BGSHeadPart::Flag::kExtraPart` (same on NG: clf4-ng `BGSHeadPart.h`:29).
const HEAD_PART_EXTRA: u8 = 1 << 3;

fn head_part(c: &NativeCall) -> Result<*mut u8, String> {
    self_as(c, RE::BGSHeadPart::RTTI)
}

fn set_valid_races(c: &mut NativeCall) -> Result<(), String> {
    let part = head_part(c)?;
    if let Some(list) = arg_as(c, 0, RE::BGSListForm::RTTI) {
        // SAFETY: a live BGSHeadPart; validRaces is a BGSListForm*.
        unsafe { wr(part, RE::BGSHeadPart::validRaces, list) };
    }
    Ok(())
}

fn has_extra_part(c: &mut NativeCall) -> Result<(), String> {
    c.ret_bool(false);
    let part = head_part(c)?;
    let Some(other) = arg_as(c, 0, RE::BGSHeadPart::RTTI) else { return Ok(()) };
    let found = form_array(part, RE::BGSHeadPart::extraParts).iter().any(|v| *v == PValue::Form(other));
    c.ret_bool(found);
    Ok(())
}

fn get_type(c: &mut NativeCall) -> Result<(), String> {
    c.ret_int(0);
    let part = head_part(c)?;
    c.ret_int(rd::<i32>(part, RE::BGSHeadPart::r#type).unwrap_or(0));
    Ok(())
}

fn is_extra_part(c: &mut NativeCall) -> Result<(), String> {
    c.ret_bool(false);
    let part = head_part(c)?;
    c.ret_bool(rd::<u8>(part, RE::BGSHeadPart::flags).unwrap_or(0) & HEAD_PART_EXTRA != 0);
    Ok(())
}

fn get_extra_parts(c: &mut NativeCall) -> Result<(), String> {
    let part = head_part(c)?;
    ret_array(c, form_array(part, RE::BGSHeadPart::extraParts))
}

// ---------------------------------------------------------------- MatSwap

/// `BSTScatterTable` fields (CommonLibF4 `BSTHashMap.h`): capacity, free, good (u32 at
/// +0x0C/+0x10/+0x14), sentinel (+0x18), heap allocator entries (+0x28). Same on NG
/// 1.11.240 (clf4-ng `BSTHashMap.h`:575-581, 647-648).
mod table {
    pub const CAPACITY: usize = 0x0C;
    pub const FREE: usize = 0x10;
    pub const GOOD: usize = 0x14;
    pub const SENTINEL: usize = 0x18;
    pub const ENTRIES: usize = 0x28;
}
/// A `BGSMaterialSwap::swapMap` entry: key `BSFixedString`, `Entry` (swapMaterial,
/// colorRemappingIndex), next.
const SWAP_ENTRY: usize = 0x20;
const SWAP_KEY: usize = 0;
const SWAP_VALUE: usize = 8;
const SWAP_NEXT: usize = 0x18;

fn swap_hash(key: *mut u8) -> u32 {
    bs_crc32(&(key as usize).to_le_bytes())
}

fn get_remap_data(c: &mut NativeCall) -> Result<(), String> {
    let swap = self_as(c, RE::BGSMaterialSwap::RTTI)?;
    let map = swap.wrapping_add(RE::BGSMaterialSwap::swapMap);
    let capacity = rd::<u32>(map, table::CAPACITY).unwrap_or(0) as usize;
    let entries = rd_ptr(map, table::ENTRIES);
    if entries.is_null() || capacity > 1 << 20 || !mem::readable(entries as usize, capacity * SWAP_ENTRY) {
        return Ok(());
    }
    let text = |p: *mut u8| {
        // SAFETY: a BSFixedString field of a used entry.
        unsafe { mem::fixed_string(mem::read(p, 0)) }.unwrap_or_default()
    };
    let mut out = Vec::new();
    for i in 0..capacity {
        let e = entries.wrapping_add(i * SWAP_ENTRY);
        if rd_ptr(e, SWAP_NEXT).is_null() {
            continue;
        }
        out.push(PValue::Struct(vec![
            ("source".into(), PValue::String(text(e.wrapping_add(SWAP_KEY)))),
            ("target".into(), PValue::String(text(e.wrapping_add(SWAP_VALUE + RE::BGSMaterialSwap::Entry::swapMaterial)))),
            ("colorIndex".into(), PValue::Float(rd(e, SWAP_VALUE + RE::BGSMaterialSwap::Entry::colorRemappingIndex).unwrap_or(0.0))),
        ]));
    }
    ret_array(c, out)
}

/// Insert into a fresh scatter table being built (CommonLibF4 `BSTScatterTable::do_insert`;
/// keys are unique).
///
/// # Safety
/// `entries` must hold `capacity` (a power of two) entries with at least one free.
unsafe fn swap_insert(entries: *mut u8, capacity: usize, sentinel: *mut u8, good: &mut usize, key: *mut u8, target: *mut u8, color: f32) {
    let slot = |k: *mut u8| entries.wrapping_add((swap_hash(k) as usize & (capacity - 1)) * SWAP_ENTRY);
    let next = |e: *mut u8| -> *mut u8 { unsafe { mem::read(e, SWAP_NEXT) } };
    let put = |e: *mut u8, n: *mut u8| unsafe {
        wr(e, SWAP_KEY, key);
        wr(e, SWAP_VALUE + RE::BGSMaterialSwap::Entry::swapMaterial, target);
        wr(e, SWAP_VALUE + RE::BGSMaterialSwap::Entry::colorRemappingIndex, color);
        wr(e, SWAP_NEXT, n);
    };
    let entry = slot(key);
    if next(entry).is_null() {
        put(entry, sentinel);
        return;
    }
    while !next(entries.wrapping_add(*good * SWAP_ENTRY)).is_null() {
        *good = (*good + 1) & (capacity - 1);
    }
    let free = entries.wrapping_add(*good * SWAP_ENTRY);
    // SAFETY: the occupant's key.
    let wouldve = slot(unsafe { mem::read(entry, SWAP_KEY) });
    if wouldve == entry {
        // Same chain: the new value goes to the free entry, linked after the head.
        put(free, next(entry));
        // SAFETY: within the table.
        unsafe { wr(entry, SWAP_NEXT, free) };
    } else {
        // The occupant belongs to another chain: move it out, take its place.
        let mut prev = wouldve;
        while next(prev) != entry {
            prev = next(prev);
        }
        // SAFETY: whole entries within the table.
        unsafe {
            std::ptr::copy_nonoverlapping(entry, free, SWAP_ENTRY);
            wr(prev, SWAP_NEXT, free);
        }
        put(entry, sentinel);
    }
}

fn set_remap_data(c: &mut NativeCall) -> Result<(), String> {
    let swap = self_as(c, RE::BGSMaterialSwap::RTTI)?;
    let items = struct_array(c, 0);
    if items.is_empty() {
        return Err("data cannot be empty".into());
    }
    let map = swap.wrapping_add(RE::BGSMaterialSwap::swapMap);
    let sentinel = rd_ptr(map, table::SENTINEL);
    if sentinel.is_null() {
        return Err("swap map has no sentinel".into());
    }
    let mut pairs: Vec<(FixedString, FixedString, f32)> = Vec::new();
    for fields in &items {
        let key = FixedString::new(&field_str(fields, "source").unwrap_or_default()).ok_or("string pool unavailable")?;
        if pairs.iter().any(|(k, _, _)| k.as_raw() == key.as_raw()) {
            continue; // emplace keeps the first
        }
        let target = FixedString::new(&field_str(fields, "target").unwrap_or_default()).ok_or("string pool unavailable")?;
        pairs.push((key, target, field_f32(fields, "colorIndex").unwrap_or(0.0)));
    }
    // reserve(size / 0.7): a power of two, at least 8.
    let capacity = (((pairs.len() as f64) / 0.7) as usize).next_power_of_two().max(8);
    let entries = heap_alloc(capacity * SWAP_ENTRY).ok_or("allocation failed")?;
    let mut good = 0usize;
    let count = pairs.len();
    for (key, target, color) in pairs {
        // SAFETY: a fresh table with `capacity` > `count` entries.
        unsafe { swap_insert(entries, capacity, sentinel, &mut good, key.into_raw(), target.into_raw(), color) };
    }
    let old_capacity = rd::<u32>(map, table::CAPACITY).unwrap_or(0) as usize;
    let old_entries = rd_ptr(map, table::ENTRIES);
    // SAFETY: the form's scatter table; a reader never sees a capacity larger than the
    // entries it pairs with.
    unsafe {
        if capacity >= old_capacity {
            wr(map, table::ENTRIES, entries);
            wr(map, table::CAPACITY, capacity as u32);
        } else {
            wr(map, table::CAPACITY, capacity as u32);
            wr(map, table::ENTRIES, entries);
        }
        wr(map, table::FREE, (capacity - count) as u32);
        wr(map, table::GOOD, good as u32);
    }
    // Destroy the old table: release its strings, free its entries.
    if !old_entries.is_null() && old_capacity <= 1 << 20 && mem::readable(old_entries as usize, old_capacity * SWAP_ENTRY) {
        for i in 0..old_capacity {
            let e = old_entries.wrapping_add(i * SWAP_ENTRY);
            if !rd_ptr(e, SWAP_NEXT).is_null() {
                // SAFETY: a used entry owns one reference per string field.
                unsafe {
                    papyrus::release_entry(e.add(SWAP_KEY).cast());
                    papyrus::release_entry(e.add(SWAP_VALUE + RE::BGSMaterialSwap::Entry::swapMaterial).cast());
                }
            }
        }
        heap_free(old_entries);
    }
    Ok(())
}

// ---------------------------------------------------------------- Perk

fn perk(c: &NativeCall) -> Result<*mut u8, String> {
    self_as(c, RE::BGSPerk::RTTI)
}

fn perk_data<T: Copy + Default>(c: &mut NativeCall, field: usize) -> Result<T, String> {
    let perk = perk(c)?;
    Ok(rd::<T>(perk, RE::BGSPerk::data + field).unwrap_or_default())
}

fn is_eligible(c: &mut NativeCall) -> Result<(), String> {
    c.ret_bool(false);
    let perk = perk(c)?;
    let actor = arg_as(c, 0, RE::Actor::RTTI).ok_or("akActor cannot be a none form")?;
    let conditions = perk.wrapping_add(RE::BGSPerk::perkConditions);
    if rd_ptr(conditions, 0).is_null() {
        c.ret_bool(true);
        return Ok(());
    }
    let is_true = resolve(&addr::CONDITION_IS_TRUE).ok_or("TESCondition::IsTrue not found")?;
    // SAFETY: TESCondition::IsTrue(this, actionRef, targetRef) on the perk's conditions.
    let ok = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> bool>(is_true)(conditions, actor, actor) };
    c.ret_bool(ok);
    Ok(())
}

// ---------------------------------------------------------------- WaterType

/// F4SE sets the spell even to None (F4SEStub refuses None).
fn set_water_spell(c: &mut NativeCall, offset: usize) -> Result<(), String> {
    let water = self_as(c, RE::TESWaterForm::RTTI)?;
    let spell = match c.value(0) {
        PValue::None => null_mut(),
        PValue::Form(p) if is(p, RE::SpellItem::RTTI) => p,
        _ => return Err("sp is not a spell".into()),
    };
    // SAFETY: a live TESWaterForm; a SpellItem* field.
    unsafe { wr(water, offset, spell) };
    Ok(())
}

// ---------------------------------------------------------------- MiscObject, ConstructibleObject

/// `BSTArray<BSTTuple<TESForm*, BGSTypedFormValuePair::SharedVal>>` elements: form at +0,
/// the u32 value at +8.
const PAIR: usize = 0x10;

/// The (form, count) pairs of a `BSTArray<BSTTuple<TESForm*, SharedVal>>*` field.
fn form_pairs(owner: *mut u8, offset: usize) -> Vec<(Option<*mut u8>, u32)> {
    let array = rd_ptr(owner, offset);
    if array.is_null() {
        return Vec::new();
    }
    let Some((data, len)) = array_view(array, PAIR) else { return Vec::new() };
    (0..len).map(|i| (form_at(data, i * PAIR), rd::<u32>(data, i * PAIR + 8).unwrap_or(0))).collect()
}

/// Replace the (form, count) pairs of the array field (created on the game heap if
/// missing; F4SEStub `clear` + `reserve` + `emplace_back`).
fn set_form_pairs(owner: *mut u8, offset: usize, pairs: &[(*mut u8, u32)]) -> Result<(), String> {
    let mut array = rd_ptr(owner, offset);
    if array.is_null() {
        // A new, empty BSTArray (F4SEStub: `new BSTArray`).
        array = heap_alloc(0x18).ok_or("allocation failed")?;
        // SAFETY: a pointer field of a live form.
        unsafe { wr(owner, offset, array) };
    }
    let (mut data, _) = array_view(array, PAIR).ok_or("unreadable array")?;
    let need = pairs.len();
    let capacity = rd::<u32>(array, 8).unwrap_or(0) as usize;
    if need > capacity || (data.is_null() && need > 0) {
        let new = heap_alloc(need * PAIR).ok_or("allocation failed")?;
        // SAFETY: BSTArray data at +0, capacity u32 at +8, size at +0x10 (emptied first
        // so no reader pairs the old size with the new buffer).
        unsafe {
            wr(array, 0x10, 0u32);
            wr(array, 0, new);
            wr(array, 8, need as u32);
        }
        heap_free(data);
        data = new;
    }
    for (i, &(form, count)) in pairs.iter().enumerate() {
        let e = data.wrapping_add(i * PAIR);
        // SAFETY: within the buffer's capacity.
        unsafe {
            wr(e, 0, form);
            wr(e, 8, count);
            wr(e, 12, 0u32);
        }
    }
    // SAFETY: BSTArray size u32 at +0x10.
    unsafe { wr(array, 0x10, need as u32) };
    Ok(())
}

fn get_misc_components(c: &mut NativeCall) -> Result<(), String> {
    let misc = self_as(c, RE::TESObjectMISC::RTTI)?;
    // F4SE keeps entries without a component (object None), unlike F4SEStub.
    let out = form_pairs(misc, RE::TESObjectMISC::componentData)
        .into_iter()
        .map(|(form, count)| {
            let component = form.filter(|&f| is(f, RE::BGSComponent::RTTI));
            PValue::Struct(vec![("object".into(), pform(component)), ("count".into(), PValue::Int(count as i32))])
        })
        .collect();
    ret_array(c, out)
}

/// Replaces the list (F4SE; F4SEStub appends), None objects included.
fn set_misc_components(c: &mut NativeCall) -> Result<(), String> {
    let misc = self_as(c, RE::TESObjectMISC::RTTI)?;
    let pairs: Vec<(*mut u8, u32)> = struct_array(c, 0)
        .iter()
        .map(|f| (field_form(f, "object").filter(|&p| is(p, RE::BGSComponent::RTTI)).unwrap_or(null_mut()), field_i32(f, "count").unwrap_or(0) as u32))
        .collect();
    set_form_pairs(misc, RE::TESObjectMISC::componentData, &pairs)
}

fn cobj(c: &NativeCall) -> Result<*mut u8, String> {
    self_as(c, RE::BGSConstructibleObject::RTTI)
}

use RE::BGSConstructibleObject::ConstructibleObjectData as COD;

fn get_constructible_components(c: &mut NativeCall) -> Result<(), String> {
    let cobj = cobj(c)?;
    let out = form_pairs(cobj, RE::BGSConstructibleObject::requiredItems)
        .into_iter()
        .map(|(form, count)| PValue::Struct(vec![("object".into(), pform(form)), ("count".into(), PValue::Int(count as i32))]))
        .collect();
    ret_array(c, out)
}

fn set_constructible_components(c: &mut NativeCall) -> Result<(), String> {
    let cobj = cobj(c)?;
    let pairs: Vec<(*mut u8, u32)> =
        struct_array(c, 0).iter().map(|f| (field_form(f, "object").unwrap_or(null_mut()), field_i32(f, "count").unwrap_or(0) as u32)).collect();
    set_form_pairs(cobj, RE::BGSConstructibleObject::requiredItems, &pairs)
}

fn cobj_data(c: &mut NativeCall, field: usize) -> Result<(), String> {
    c.ret_int(0);
    let cobj = cobj(c)?;
    c.ret_int(i32::from(rd::<u16>(cobj, RE::BGSConstructibleObject::data + field).unwrap_or(0)));
    Ok(())
}

fn set_cobj_data(c: &mut NativeCall, field: usize) -> Result<(), String> {
    let cobj = cobj(c)?;
    let value = c.int(0) as u16;
    // SAFETY: a live BGSConstructibleObject: ConstructibleObjectData u16 fields.
    unsafe { wr(cobj, RE::BGSConstructibleObject::data + field, value) };
    Ok(())
}

// ---------------------------------------------------------------- table

pub(crate) const NATIVES: &[Entry] = &[
    // Actor
    ("Actor", "GetFurnitureReference", get_furniture_reference),
    ("Actor", "GetWornItem", get_worn_item),
    ("Actor", "GetWornItemMods", get_worn_item_mods),
    ("Actor", "QueueUpdate", queue_update),
    ("Actor", "IsProtected", is_protected),
    // ActorBase
    ("ActorBase", "HasHeadPartOverlays", has_head_part_overlays),
    ("ActorBase", "GetTemplate", get_template),
    ("ActorBase", "GetBodyWeight", get_body_weight),
    ("ActorBase", "SetBodyWeight", set_body_weight),
    ("ActorBase", "GetOutfit", get_outfit),
    ("ActorBase", "GetHeadParts", get_head_parts),
    // Armor, ArmorAddon, EquipSlot
    ("Armor", "GetArmorAddons", get_armor_addons),
    ("ArmorAddon", "GetAdditionalRaces", get_additional_races),
    ("EquipSlot", "GetParents", get_parents),
    // HeadPart
    ("HeadPart", "SetValidRaces", set_valid_races),
    ("HeadPart", "HasExtraPart", has_extra_part),
    ("HeadPart", "GetType", get_type),
    ("HeadPart", "GetValidRaces", |c| get_form_field(c, RE::BGSHeadPart::RTTI, RE::BGSHeadPart::validRaces)),
    ("HeadPart", "IsExtraPart", is_extra_part),
    ("HeadPart", "GetExtraParts", get_extra_parts),
    // MatSwap
    ("MatSwap", "GetRemapData", get_remap_data),
    ("MatSwap", "SetRemapData", set_remap_data),
    // Perk
    ("Perk", "IsHidden", |c| {
        c.ret_bool(false);
        let v: u8 = perk_data(c, RE::PerkData::hidden)?;
        c.ret_bool(v != 0);
        Ok(())
    }),
    ("Perk", "IsPlayable", |c| {
        c.ret_bool(false);
        let v: u8 = perk_data(c, RE::PerkData::playable)?;
        c.ret_bool(v != 0);
        Ok(())
    }),
    ("Perk", "IsEligible", is_eligible),
    ("Perk", "GetNumRanks", |c| {
        c.ret_int(0);
        let v: i8 = perk_data(c, RE::PerkData::numRanks)?;
        c.ret_int(i32::from(v));
        Ok(())
    }),
    ("Perk", "GetSWFPath", |c| {
        c.ret_string("");
        let perk = perk(c)?;
        // SAFETY: BGSPerk::swfFile is a BSFixedStringCS (pool entry or null).
        let path = rd::<*const u8>(perk, RE::BGSPerk::swfFile).and_then(|e| unsafe { mem::fixed_string(e) }).unwrap_or_default();
        c.ret_string(&path);
        Ok(())
    }),
    ("Perk", "GetNextPerk", |c| get_form_field(c, RE::BGSPerk::RTTI, RE::BGSPerk::nextPerk)),
    ("Perk", "GetLevel", |c| {
        c.ret_int(0);
        let v: i8 = perk_data(c, RE::PerkData::level)?;
        c.ret_int(i32::from(v));
        Ok(())
    }),
    // WaterType
    ("WaterType", "SetConsumeSpell", |c| set_water_spell(c, RE::TESWaterForm::consumeSpell)),
    ("WaterType", "GetConsumeSpell", |c| get_form_field(c, RE::TESWaterForm::RTTI, RE::TESWaterForm::consumeSpell)),
    ("WaterType", "SetContactSpell", |c| set_water_spell(c, RE::TESWaterForm::contactSpell)),
    ("WaterType", "GetContactSpell", |c| get_form_field(c, RE::TESWaterForm::RTTI, RE::TESWaterForm::contactSpell)),
    // MiscObject
    ("MiscObject", "GetMiscComponents", get_misc_components),
    ("MiscObject", "SetMiscComponents", set_misc_components),
    // Component
    ("Component", "GetScrapScalar", |c| get_form_field(c, RE::BGSComponent::RTTI, RE::BGSComponent::modScrapScalar)),
    ("Component", "GetScrapItem", |c| get_form_field(c, RE::BGSComponent::RTTI, RE::BGSComponent::scrapItem)),
    ("Component", "SetScrapItem", |c| set_form_field(c, RE::BGSComponent::RTTI, RE::BGSComponent::scrapItem, RE::TESObjectMISC::RTTI)),
    ("Component", "SetScrapScalar", |c| set_form_field(c, RE::BGSComponent::RTTI, RE::BGSComponent::modScrapScalar, RE::TESGlobal::RTTI)),
    // ConstructibleObject
    ("ConstructibleObject", "GetCreatedObject", |c| get_form_field(c, RE::BGSConstructibleObject::RTTI, RE::BGSConstructibleObject::createdItem)),
    ("ConstructibleObject", "SetCreatedObject", |c| {
        set_form_field(c, RE::BGSConstructibleObject::RTTI, RE::BGSConstructibleObject::createdItem, RE::TESForm::RTTI)
    }),
    ("ConstructibleObject", "SetConstructibleComponents", set_constructible_components),
    ("ConstructibleObject", "SetCreatedCount", |c| set_cobj_data(c, COD::numConstructed)),
    ("ConstructibleObject", "GetCreatedCount", |c| cobj_data(c, COD::numConstructed)),
    ("ConstructibleObject", "GetPriority", |c| cobj_data(c, COD::workshopPriority)),
    ("ConstructibleObject", "GetConstructibleComponents", get_constructible_components),
    ("ConstructibleObject", "SetPriority", |c| set_cobj_data(c, COD::workshopPriority)),
    ("ConstructibleObject", "SetWorkbenchKeyword", |c| {
        set_form_field(c, RE::BGSConstructibleObject::RTTI, RE::BGSConstructibleObject::benchKeyword, RE::BGSKeyword::RTTI)
    }),
    ("ConstructibleObject", "GetWorkbenchKeyword", |c| get_form_field(c, RE::BGSConstructibleObject::RTTI, RE::BGSConstructibleObject::benchKeyword)),
];
