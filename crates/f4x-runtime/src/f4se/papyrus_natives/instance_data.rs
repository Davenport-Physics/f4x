//! InstanceData, Weapon, ObjectMod, and the scripted `Weapon.GetInstanceOwner` /
//! `Actor.GetInstanceOwner` (declared native by f4x).
//!
//! Behaviour reference: CommonLibF4 `F4SEStub/runtime/src/Papyrus` (MIT), checked against
//! real F4SE with `tools/f4se_natives_check.py` and targeted black-box calls. F4SEStub
//! leaves every `InstanceData` native unimplemented; their behaviour here is black-box:
//!
//! - An `InstanceData:Owner` names a form and a slot. A weapon or armor base form means
//!   its own data (`TESObjectWEAP::weaponData`, `TESObjectARMO::data`). An actor means
//!   the item in biped slot `slotIndex` (its `BGSObjectInstance` instance data, else the
//!   item's base data). Another reference means its `ExtraInstanceData`, else its base
//!   object's data (our choice: not yet observed under F4SE).
//! - Getters on anything else return 0 / 0.0 / false / None; setters do nothing.
//! - Empty arrays come back as None (F4SE never returns an empty array).
//! - Integer details (8/16-bit fields unsigned and clamped, `GetFlag` = all bits set) and
//!   `GetPropertyModifiers`' target encoding are black-box; see the functions.
//!
//! Layouts: CommonLibF4 `TESBoundObjects.h` (`TESObjectWEAP::InstanceData`, `RangedData`,
//! `TESObjectARMO::InstanceData`), `TESObjectREFRs.h` (`BipedAnim`, `BIPOBJECT`,
//! `BGSObjectInstance`), `BSExtraData.h` (`ExtraDataList`, `ExtraInstanceData`),
//! `BGSMod.h` (`Attachment::Mod`, `Property::Mod`), through the generated tables.

use std::ffi::c_void;

use f4x_engine::address::resolve;
use f4x_engine::form::Form;
use f4x_engine::form_type::FormType;
use f4x_engine::layout::RE;
use f4x_engine::layout::RE::TESObjectARMO::InstanceData as AD;
use f4x_engine::layout::RE::TESObjectWEAP::InstanceData as WD;
use f4x_engine::layout::RE::TESObjectWEAP::RangedData as RD;
use f4x_engine::mem;
use f4x_engine::papyrus::NativeCall;
use f4x_engine::papyrus_value::PValue;
use f4x_engine_sys::addresses as addr;

use super::Entry;

fn sig(signature: &str) -> FormType {
    FormType::from_signature(signature).unwrap_or(FormType(0xFF))
}

// ---------------------------------------------------------------- instance data

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Weapon,
    Armor,
}

/// A live `TESObjectWEAP::InstanceData` or `TESObjectARMO::InstanceData`.
#[derive(Clone, Copy)]
struct Data {
    ptr: *mut u8,
    kind: Kind,
}

impl Data {
    fn checked(ptr: *mut u8, kind: Kind) -> Option<Data> {
        let rtti = match kind {
            Kind::Weapon => WD::RTTI,
            Kind::Armor => AD::RTTI,
        };
        f4x_engine::game().is_instance_of(ptr as usize, rtti).then_some(Data { ptr, kind })
    }

    fn weapon(self) -> Option<*mut u8> {
        (self.kind == Kind::Weapon).then_some(self.ptr)
    }

    fn armor(self) -> Option<*mut u8> {
        (self.kind == Kind::Armor).then_some(self.ptr)
    }

    /// `TESObjectWEAP::RangedData` (null for melee weapons).
    fn ranged(self) -> Option<*mut u8> {
        let weapon = self.weapon()?;
        // SAFETY: a checked weapon instance; rangedData is a pointer.
        let ranged: *mut u8 = unsafe { mem::read(weapon, WD::rangedData) };
        mem::readable(ranged as usize, RD::SIZE).then_some(ranged)
    }

    /// The `BGSKeywordForm*` (weapon +0x80, armor +0x28).
    fn keyword_form_slot(self) -> usize {
        match self.kind {
            Kind::Weapon => WD::keywords,
            Kind::Armor => AD::keywords,
        }
    }

    fn damage_types_slot(self) -> usize {
        match self.kind {
            Kind::Weapon => WD::damageTypes,
            Kind::Armor => AD::damageTypes,
        }
    }
}

fn kind_of(form: Form) -> Option<Kind> {
    let t = form.form_type();
    if t == sig("WEAP") {
        Some(Kind::Weapon)
    } else if t == sig("ARMO") {
        Some(Kind::Armor)
    } else {
        None
    }
}

/// A weapon's or armor's own data.
fn base_data(form: Form) -> Option<Data> {
    let kind = kind_of(form)?;
    let offset = match kind {
        Kind::Weapon => RE::TESObjectWEAP::weaponData,
        Kind::Armor => RE::TESObjectARMO::data,
    };
    // SAFETY: an embedded member of a live form.
    Data::checked(unsafe { form.as_ptr().add(offset) }, kind)
}

/// The instance data of `object` held in a `BSTSmartPointer<TBO_InstanceData>`, else the
/// object's own data.
fn instance_or_base(object: Form, instance: *mut u8) -> Option<Data> {
    let kind = kind_of(object)?;
    Data::checked(instance, kind).or_else(|| base_data(object))
}

/// Biped slot `slot` of an actor: (item, instance data pointer).
fn biped_slot(actor: Form, slot: i32) -> Option<(Form, *mut u8)> {
    const SLOTS: i32 = 44; // BIPED_OBJECT::kTotal
    if !(0..SLOTS).contains(&slot) {
        return None;
    }
    // SAFETY: Actor::biped is a BSTSmartPointer<BipedAnim> (one pointer).
    let biped: *mut u8 = unsafe { mem::read(actor.as_ptr(), RE::Actor::biped) };
    if !mem::readable(biped as usize, RE::BipedAnim::SIZE) {
        return None;
    }
    // SAFETY: BipedAnim::object[slot] (BIPOBJECT, 0x58 bytes); BGSObjectInstance at +0.
    unsafe {
        let entry = biped.add(RE::BipedAnim::object + slot as usize * RE::BIPOBJECT::SIZE + RE::BIPOBJECT::parent);
        let object = Form::checked(mem::read(entry, RE::BGSObjectInstance::object))?;
        Some((object, mem::read(entry, RE::BGSObjectInstance::instanceData)))
    }
}

/// The `ExtraInstanceData` on a reference: (base object, instance data pointer).
fn extra_instance(reference: Form) -> Option<*mut u8> {
    // SAFETY: TESObjectREFR::extraList is a BSTSmartPointer<ExtraDataList>.
    let list: *mut u8 = unsafe { mem::read(reference.as_ptr(), RE::TESObjectREFR::extraList) };
    if !mem::readable(list as usize, RE::ExtraDataList::SIZE) {
        return None;
    }
    // SAFETY: ExtraDataList::extraData is a BaseExtraList whose first member is the head.
    let mut extra: *mut u8 = unsafe { mem::read(list, RE::ExtraDataList::extraData) };
    let game = f4x_engine::game();
    for _ in 0..256 {
        if !game.is_instance_of(extra as usize, RE::BSExtraData::RTTI) {
            return None;
        }
        if game.is_instance_of(extra as usize, RE::ExtraInstanceData::RTTI) {
            // SAFETY: a live ExtraInstanceData; data is a BSTSmartPointer<TBO_InstanceData>.
            return Some(unsafe { mem::read(extra, RE::ExtraInstanceData::data) });
        }
        // SAFETY: a live BSExtraData.
        extra = unsafe { mem::read(extra, RE::BSExtraData::next) };
    }
    None
}

/// The `Owner` struct argument: (form, slotIndex).
fn owner_arg(c: &NativeCall) -> Option<(Form, i32)> {
    let PValue::Struct(fields) = c.value(0) else { return None };
    let mut form = None;
    let mut slot = 0;
    for (name, value) in fields {
        match value {
            PValue::Form(p) if name.eq_ignore_ascii_case("owner") => form = Form::checked(p),
            PValue::Int(i) if name.eq_ignore_ascii_case("slotIndex") => slot = i,
            _ => {}
        }
    }
    Some((form?, slot))
}

/// The instance data an `Owner` names.
fn resolve_owner(c: &NativeCall) -> Option<Data> {
    let (form, slot) = owner_arg(c)?;
    if form.as_reference().is_none() {
        return base_data(form);
    }
    // SAFETY: a live polymorphic form.
    if unsafe { f4x_engine::game().cast(form.as_ptr(), RE::Actor::RTTI) }.is_some() {
        let (object, instance) = biped_slot(form, slot)?;
        return instance_or_base(object, instance);
    }
    let base = form.as_reference()?.base_object()?;
    instance_or_base(base, extra_instance(form).unwrap_or(std::ptr::null_mut()))
}

// ---------------------------------------------------------------- field access

/// Where a scalar field lives: in the instance data or in its `RangedData`.
#[derive(Clone, Copy)]
enum At {
    /// Weapon instance data offset.
    W(usize),
    /// Armor instance data offset.
    A(usize),
    /// Weapon or armor offset.
    WA(usize, usize),
    /// Weapon `RangedData` offset.
    R(usize),
}

fn locate(data: Data, at: At) -> Option<*mut u8> {
    // SAFETY: offsets of the checked instance (generated layout).
    unsafe {
        match at {
            At::W(o) => data.weapon().map(|p| p.add(o)),
            At::A(o) => data.armor().map(|p| p.add(o)),
            At::WA(w, a) => Some(data.ptr.add(if data.kind == Kind::Weapon { w } else { a })),
            At::R(o) => data.ranged().map(|p| p.add(o)),
        }
    }
}

fn field(c: &NativeCall, at: At) -> Option<*mut u8> {
    resolve_owner(c).and_then(|d| locate(d, at))
}

/// Integer widths of the fields. F4SE reads the 8- and 16-bit fields unsigned and clamps
/// what it writes to them (black-box: `SetNumProjectiles(300)` reads back 255,
/// `SetAttackDamage(70000)` 65535, `SetAccuracyBonus(200)` 200); 32-bit fields are stored
/// as given (`SetGoldValue(-5)` reads back -5).
#[derive(Clone, Copy)]
enum Int {
    U8,
    U16,
    U32,
    I32,
}

fn read_int(p: *mut u8, w: Int) -> i32 {
    // SAFETY: a field of a checked instance.
    unsafe {
        match w {
            Int::U8 => i32::from(mem::read::<u8>(p, 0)),
            Int::U16 => i32::from(mem::read::<u16>(p, 0)),
            Int::U32 => mem::read::<u32>(p, 0) as i32,
            Int::I32 => mem::read::<i32>(p, 0),
        }
    }
}

fn write_int(p: *mut u8, w: Int, v: i32) {
    // SAFETY: a field of a checked instance.
    unsafe {
        match w {
            Int::U8 => p.write(v.clamp(0, 0xFF) as u8),
            Int::U16 => p.cast::<u16>().write_unaligned(v.clamp(0, 0xFFFF) as u16),
            Int::U32 => p.cast::<u32>().write_unaligned(v as u32),
            Int::I32 => p.cast::<i32>().write_unaligned(v),
        }
    }
}

fn get_float(c: &mut NativeCall, at: At) -> Result<(), String> {
    // SAFETY: a float field of a checked instance.
    let v = field(c, at).map_or(0.0, |p| unsafe { mem::read::<f32>(p, 0) });
    c.ret_float(v);
    Ok(())
}

fn set_float(c: &mut NativeCall, at: At) -> Result<(), String> {
    let v = c.float(1);
    if let Some(p) = field(c, at) {
        // SAFETY: a float field of a checked instance.
        unsafe { p.cast::<f32>().write_unaligned(v) };
    }
    Ok(())
}

fn get_int(c: &mut NativeCall, at: At, w: Int) -> Result<(), String> {
    let v = field(c, at).map_or(0, |p| read_int(p, w));
    c.ret_int(v);
    Ok(())
}

fn set_int(c: &mut NativeCall, at: At, w: Int) -> Result<(), String> {
    let v = c.int(1);
    if let Some(p) = field(c, at) {
        write_int(p, w, v);
    }
    Ok(())
}

/// A form pointer field, returned as the declared script type.
fn get_form(c: &mut NativeCall, at: At) -> Result<(), String> {
    // SAFETY: a pointer field of a checked instance (validated before use).
    let form = field(c, at).and_then(|p| Form::checked(unsafe { mem::read(p, 0) }));
    c.ret_value(&form.map_or(PValue::None, |f| PValue::Form(f.as_ptr())))
}

/// Set a form pointer field to argument 1 if it has the expected type (None clears it).
fn set_form(c: &mut NativeCall, at: At, signature: &str) -> Result<(), String> {
    let value = match c.value(1) {
        PValue::Form(p) => match Form::checked(p) {
            Some(f) if f.form_type() == sig(signature) => f.as_ptr(),
            _ => return Ok(()),
        },
        _ => std::ptr::null_mut(),
    };
    if let Some(p) = field(c, at) {
        // SAFETY: a pointer field of a checked instance.
        unsafe { p.cast::<*mut u8>().write_unaligned(value) };
    }
    Ok(())
}

// ---------------------------------------------------------------- flags

/// True when every bit of `flag` is set (black-box: F4SE answers true for flag 0 on a
/// weapon, false for -1).
fn get_flag(c: &mut NativeCall) -> Result<(), String> {
    let flag = c.int(1) as u32;
    let v = field(c, At::W(WD::flags)).is_some_and(|p| read_int(p, Int::U32) as u32 & flag == flag);
    c.ret_bool(v);
    Ok(())
}

fn set_flag(c: &mut NativeCall) -> Result<(), String> {
    let (flag, on) = (c.int(1) as u32, c.bool(2));
    if let Some(p) = field(c, At::W(WD::flags)) {
        let flags = read_int(p, Int::U32) as u32;
        write_int(p, Int::U32, (if on { flags | flag } else { flags & !flag }) as i32);
    }
    Ok(())
}

// ---------------------------------------------------------------- engine heap

fn heap() -> Option<(*mut c_void, usize)> {
    let singleton = resolve(&addr::MEMORY_MANAGER_SINGLETON)?;
    let allocate = resolve(&addr::MEMORY_MANAGER_ALLOCATE)?;
    // SAFETY: MemoryManager::GetSingleton() -> MemoryManager& (CommonLibF4 MemoryManager.h).
    let manager = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)() };
    (!manager.is_null()).then_some((manager, allocate))
}

/// `RE::malloc`: `MemoryManager::Allocate(size, 0, false)`, zeroed. The engine frees
/// these blocks itself when it destroys their owner.
fn engine_alloc(size: usize) -> Option<*mut u8> {
    let (manager, allocate) = heap()?;
    // SAFETY: MemoryManager::Allocate(this, size, alignment, alignmentRequired).
    let p = unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, usize, u32, bool) -> *mut u8>(allocate)(manager, size.max(8), 0, false)
    };
    if p.is_null() {
        return None;
    }
    // SAFETY: a fresh block of at least `size` bytes.
    unsafe { std::ptr::write_bytes(p, 0, size) };
    Some(p)
}

// ---------------------------------------------------------------- keywords

/// A `BGSKeywordForm`'s keywords (`keywords` array, `numKeywords`).
fn keyword_list(kf: *mut u8) -> Vec<*mut u8> {
    // SAFETY: a checked BGSKeywordForm.
    unsafe {
        let array: *const *mut u8 = mem::read(kf, RE::BGSKeywordForm::keywords);
        let count: u32 = mem::read(kf, RE::BGSKeywordForm::numKeywords);
        if array.is_null() || count > 4096 || !mem::readable(array as usize, count as usize * 8) {
            return Vec::new();
        }
        (0..count as usize).map(|i| array.add(i).read_unaligned()).collect()
    }
}

fn keyword_form(data: Data) -> Option<*mut u8> {
    // SAFETY: a pointer field of a checked instance.
    let kf: *mut u8 = unsafe { mem::read(data.ptr, data.keyword_form_slot()) };
    f4x_engine::game().is_instance_of(kf as usize, RE::BGSKeywordForm::RTTI).then_some(kf)
}

fn get_keywords(c: &mut NativeCall) -> Result<(), String> {
    let keywords: Vec<PValue> = resolve_owner(c)
        .and_then(keyword_form)
        .map(keyword_list)
        .unwrap_or_default()
        .into_iter()
        .filter_map(Form::checked)
        .map(|f| PValue::Form(f.as_ptr()))
        .collect();
    c.ret_value(&if keywords.is_empty() { PValue::None } else { PValue::Array(keywords) })
}

fn set_keywords(c: &mut NativeCall) -> Result<(), String> {
    let keyword = sig("KYWD");
    let new: Vec<*mut u8> = match c.value(1) {
        PValue::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                PValue::Form(p) => Form::checked(p).filter(|f| f.form_type() == keyword).map(Form::as_ptr),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let Some(data) = resolve_owner(c) else { return Ok(()) };
    let kf = match keyword_form(data) {
        Some(kf) => kf,
        None if new.is_empty() => return Ok(()),
        None => {
            // TBO_InstanceData::CreateKeywordData (vtable slot 3), then GetKeywordData (1).
            type Create = unsafe extern "C" fn(*mut u8);
            type Get = unsafe extern "C" fn(*mut u8) -> *mut u8;
            // SAFETY: virtuals of a checked TBO_InstanceData.
            unsafe {
                let vtable: *const usize = mem::read(data.ptr, 0);
                std::mem::transmute::<usize, Create>(*vtable.add(RE::TBO_InstanceData::vtbl::CreateKeywordData))(data.ptr);
                let kf = std::mem::transmute::<usize, Get>(*vtable.add(RE::TBO_InstanceData::vtbl::GetKeywordData))(data.ptr);
                if !f4x_engine::game().is_instance_of(kf as usize, RE::BGSKeywordForm::RTTI) {
                    return Ok(());
                }
                kf
            }
        }
    };
    // The old array is not freed: it may be shared or owned by another allocator.
    let array = if new.is_empty() {
        std::ptr::null_mut()
    } else {
        let Some(array) = engine_alloc(new.len() * 8) else { return Err("engine allocation failed".into()) };
        // SAFETY: a fresh block of `new.len()` pointers.
        unsafe { std::ptr::copy_nonoverlapping(new.as_ptr(), array.cast::<*mut u8>(), new.len()) };
        array
    };
    // SAFETY: a checked BGSKeywordForm; the count is written after the array so readers
    // never see more entries than the array holds.
    unsafe {
        kf.add(RE::BGSKeywordForm::numKeywords).cast::<u32>().write_unaligned(0);
        kf.add(RE::BGSKeywordForm::keywords).cast::<*mut u8>().write_unaligned(array);
        kf.add(RE::BGSKeywordForm::numKeywords).cast::<u32>().write_unaligned(new.len() as u32);
    }
    Ok(())
}

// ---------------------------------------------------------------- damage types

/// `BSTArray<BSTTuple<TESForm*, BGSTypedFormValuePair::SharedVal>>`: 0x10-byte elements
/// (form pointer, then a u32/float). Same on NG (clf4-ng `BGSTypedFormValuePair.h`:12).
const PAIR_SIZE: usize = 0x10;

fn damage_types_array(data: Data) -> Option<*mut u8> {
    // SAFETY: a pointer field of a checked instance.
    let array: *mut u8 = unsafe { mem::read(data.ptr, data.damage_types_slot()) };
    if !mem::readable(array as usize, 0x18) {
        return None;
    }
    // SAFETY: a readable BSTArray header {data, capacity u32, size u32 @0x10}.
    let (elements, size): (usize, u32) = unsafe { (mem::read(array, 0), mem::read(array, 0x10)) };
    let ok = size <= 4096 && (size == 0 || mem::readable(elements, size as usize * PAIR_SIZE));
    ok.then_some(array)
}

fn get_damage_types(c: &mut NativeCall) -> Result<(), String> {
    let mut out = Vec::new();
    if let Some(array) = resolve_owner(c).and_then(damage_types_array) {
        // SAFETY: a BSTArray of 0x10-byte tuples.
        let view = unsafe { mem::BSTArray::<[u8; PAIR_SIZE]>::at(array) };
        for pair in view.iter() {
            let form = Form::checked(usize::from_le_bytes(pair[0..8].try_into().unwrap_or_default()) as *mut u8);
            let damage = u32::from_le_bytes(pair[8..12].try_into().unwrap_or_default()) as i32;
            out.push(PValue::Struct(vec![
                ("type".into(), form.map_or(PValue::None, |f| PValue::Form(f.as_ptr()))),
                ("damage".into(), PValue::Int(damage)),
            ]));
        }
    }
    c.ret_value(&if out.is_empty() { PValue::None } else { PValue::Array(out) })
}

fn set_damage_types(c: &mut NativeCall) -> Result<(), String> {
    let damage_type = sig("DMGT");
    let mut new: Vec<(*mut u8, u32)> = Vec::new();
    if let PValue::Array(items) = c.value(1) {
        for item in items {
            let PValue::Struct(fields) = item else { continue };
            let mut form = None;
            let mut damage = 0u32;
            for (name, value) in fields {
                match value {
                    PValue::Form(p) if name.eq_ignore_ascii_case("type") => form = Form::checked(p).filter(|f| f.form_type() == damage_type),
                    PValue::Int(i) if name.eq_ignore_ascii_case("damage") => damage = i as u32,
                    _ => {}
                }
            }
            if let Some(form) = form {
                new.push((form.as_ptr(), damage));
            }
        }
    }
    let Some(data) = resolve_owner(c) else { return Ok(()) };
    let array = match damage_types_array(data) {
        Some(array) => array,
        None if new.is_empty() => return Ok(()),
        None => {
            let Some(array) = engine_alloc(0x18) else { return Err("engine allocation failed".into()) };
            // SAFETY: the pointer field of a checked instance; a fresh, zeroed BSTArray.
            unsafe { data.ptr.add(data.damage_types_slot()).cast::<*mut u8>().write_unaligned(array) };
            array
        }
    };
    let buffer = if new.is_empty() {
        std::ptr::null_mut()
    } else {
        let Some(buffer) = engine_alloc(new.len() * PAIR_SIZE) else { return Err("engine allocation failed".into()) };
        for (i, (form, damage)) in new.iter().enumerate() {
            // SAFETY: element `i` of the fresh buffer.
            unsafe {
                let e = buffer.add(i * PAIR_SIZE);
                e.cast::<*mut u8>().write_unaligned(*form);
                e.add(8).cast::<u32>().write_unaligned(*damage);
            }
        }
        buffer
    };
    // SAFETY: BSTArray {data @0, capacity u32 @8, size u32 @0x10}. The old buffer is not
    // freed (it may be referenced elsewhere); size is cleared first so readers never index
    // past the buffer.
    unsafe {
        array.add(0x10).cast::<u32>().write_unaligned(0);
        array.cast::<*mut u8>().write_unaligned(buffer);
        array.add(8).cast::<u32>().write_unaligned(new.len() as u32);
        array.add(0x10).cast::<u32>().write_unaligned(new.len() as u32);
    }
    Ok(())
}

// ---------------------------------------------------------------- owners

/// `Weapon.GetInstanceOwner()`: the weapon itself, slot 0 (as F4SE returns it).
fn weapon_instance_owner(c: &mut NativeCall) -> Result<(), String> {
    let weapon = c.self_form(sig("WEAP"));
    owner_struct(c, weapon, 0)
}

/// `Actor.GetInstanceOwner(slotIndex)`: the actor and the slot.
fn actor_instance_owner(c: &mut NativeCall) -> Result<(), String> {
    let actor = match c.self_value() {
        PValue::Form(p) => Form::checked(p).map(Form::as_ptr),
        _ => None,
    };
    let slot = c.int(0);
    owner_struct(c, actor, slot)
}

fn owner_struct(c: &mut NativeCall, owner: Option<*mut u8>, slot: i32) -> Result<(), String> {
    let Some(owner) = owner else { return c.ret_value(&PValue::None) };
    c.ret_value(&PValue::Struct(vec![("slotIndex".into(), PValue::Int(slot)), ("Owner".into(), PValue::Form(owner))]))
}

// ---------------------------------------------------------------- Weapon

fn get_embedded_mod(c: &mut NativeCall) -> Result<(), String> {
    let weapon = c.self_form(sig("WEAP")).ok_or("self is not a Weapon")?;
    // SAFETY: TESObjectWEAP::embeddedWeaponMod (validated before use).
    let m = Form::checked(unsafe { mem::read(weapon, RE::TESObjectWEAP::embeddedWeaponMod) });
    c.ret_value(&m.map_or(PValue::None, |f| PValue::Form(f.as_ptr())))
}

fn set_embedded_mod(c: &mut NativeCall) -> Result<(), String> {
    let weapon = c.self_form(sig("WEAP")).ok_or("self is not a Weapon")?;
    let m = c.form(0, sig("OMOD")).ok_or("mod cannot be a none form")?;
    // SAFETY: TESObjectWEAP::embeddedWeaponMod of a live weapon.
    unsafe { weapon.add(RE::TESObjectWEAP::embeddedWeaponMod).cast::<*mut u8>().write_unaligned(m) };
    Ok(())
}

// ---------------------------------------------------------------- ObjectMod

use f4x_engine::layout::RE::BGSMod::Attachment::Mod as OM;

fn object_mod(c: &NativeCall) -> Result<*mut u8, String> {
    c.self_form(sig("OMOD")).ok_or_else(|| "self is not an ObjectMod".into())
}

fn get_loose_mod(c: &mut NativeCall) -> Result<(), String> {
    let m = object_mod(c)?;
    let f = resolve(&addr::OMOD_GET_LOOSE_MOD).ok_or("BGSMod::Attachment::Mod::GetLooseMod not found")?;
    // SAFETY: Mod::GetLooseMod(this) -> TESObjectMISC* (CommonLibF4 community BGSMod.h).
    let misc = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*mut u8) -> *mut u8>(f)(m) };
    let misc = Form::checked(misc).filter(|f| f.form_type() == sig("MISC"));
    c.ret_value(&misc.map_or(PValue::None, |f| PValue::Form(f.as_ptr())))
}

/// `BGSMod::Property::Mod` (0x10 bytes): data @0, then a u32 of bitfields
/// `target:11 op:2 type:3` (CommonLibF4 `BGSMod.h`; same on NG, clf4-ng `BGSMod.h`:107-112).
struct PropertyMod {
    data: [u8; 8],
    target: u32,
    op: u32,
    kind: u32,
}

fn decode(p: *const u8) -> PropertyMod {
    // SAFETY: a readable 0x10-byte Property::Mod (checked by the callers).
    unsafe {
        let bits: u32 = mem::read(p, 8);
        PropertyMod { data: mem::read(p, 0), target: bits & 0x7FF, op: (bits >> 11) & 3, kind: (bits >> 13) & 7 }
    }
}

/// The attach-point index of each `Attachment::Instance` (0x10 bytes: mod @0, index u8
/// @8) of a Mod, through the engine's `Mod::GetData` (attachments @0x00,
/// attachmentCount u32 @0x10).
fn attachment_indices(m: *mut u8) -> Vec<u8> {
    let Some(data) = mod_data(m) else { return Vec::new() };
    let list = usize::from_le_bytes(data[0x00..0x08].try_into().unwrap_or_default());
    let count = u32::from_le_bytes(data[0x10..0x14].try_into().unwrap_or_default()) as usize;
    if count == 0 || count > 4096 || !mem::readable(list, count * 0x10) {
        return Vec::new();
    }
    // SAFETY: `count` readable 0x10-byte instances.
    (0..count).map(|k| unsafe { mem::read::<u8>((list + k * 0x10) as *const u8, 8) }).collect()
}

/// `Mod::GetData(this, Data&) const` (CommonLibF4 community `BGSMod.h`): a filled
/// `Mod::Data` (attachments @0x00, propertyMods @0x08, counts u32 @0x10 and @0x14; same on
/// NG, clf4-ng `BGSMod.h`:121-130 and `Instance` :225-234).
/// NG 1.11.240 inlines `Mod::GetData` everywhere; its `omod_get_data` recipe is the
/// `Container::GetData(const Container*, Container::Data*)` that it wraps (our
/// disassembly: 0x2525a0, called with the Mod's container at +0x88), which fills the
/// fields read here. So on next-gen the call takes the Mod's container.
fn mod_data(m: *mut u8) -> Option<[u8; OM::Data::SIZE]> {
    let get_data = resolve(&addr::OMOD_GET_DATA)?;
    let mut data = [0u8; OM::Data::SIZE];
    let this = m.wrapping_add(f4x_engine::pick!(0, OM::bases::Container));
    // SAFETY: the engine function fills the caller's Data.
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8)>(get_data)(this, data.as_mut_ptr()) };
    Some(data)
}

/// The Mod's property mods, through the engine's `Mod::GetData`, else by walking the
/// container.
fn property_mods(m: *mut u8) -> Vec<PropertyMod> {
    if let Some(data) = mod_data(m) {
        let mods = usize::from_le_bytes(data[0x08..0x10].try_into().unwrap_or_default());
        let count = u32::from_le_bytes(data[0x14..0x18].try_into().unwrap_or_default()) as usize;
        if count == 0 || count > 4096 || !mem::readable(mods, count * 0x10) {
            return Vec::new();
        }
        return (0..count).map(|k| decode((mods + k * 0x10) as *const u8)).collect();
    }
    // BGSMod::Container (a BSTDataBuffer<2>: buffer @0, size u32 @8) inside the Mod. The
    // buffer holds the blocks' data, then up to two 4-byte block headers
    // {size:24, id:8}; id 0xFF ends the list. Property mods are block id 1.
    let container = OM::bases::Container;
    let mut out = Vec::new();
    // SAFETY: the Mod's container; every read is bounds- and readability-checked.
    unsafe {
        let buffer: *const u8 = mem::read(m, container);
        let size: u32 = mem::read(m, container + 8);
        if buffer.is_null() || size > 1 << 20 || !mem::readable(buffer as usize, size as usize + 8) {
            return out;
        }
        let mut offset = 0usize;
        for i in 0..2usize {
            let header: u32 = mem::read(buffer, size as usize + i * 4);
            let (block_size, id) = ((header & 0x00FF_FFFF) as usize, header >> 24);
            if id == 0xFF {
                break;
            }
            if id == 1 {
                let count = block_size / 0x10;
                if offset + count * 0x10 > size as usize {
                    break;
                }
                out.extend((0..count).map(|k| decode(buffer.add(offset + k * 0x10))));
                break;
            }
            offset += block_size;
        }
    }
    out
}

fn get_property_modifiers(c: &mut NativeCall) -> Result<(), String> {
    let m = object_mod(c)?;
    // Property::TYPE: Int, Float, Bool, String, Form, Enum, Pair. Property::OP: Set 0,
    // Mul/And/Rem 1, Add/Or 2. Papyrus operators (F4SEStub ObjectMod::Operator): Set 0,
    // Add 1, MultAdd 2, And 3, Or 4, Rem 5.
    let mods = property_mods(m);
    // Targets come back as (record kind << 8) | index: armor mods' targets +0x100, actor
    // (NPC_) mods' +0x200, weapon mods' as they are (black-box, 400 vanilla OMODs).
    // SAFETY: Mod::targetFormType, a u8 ENUM_FORM_ID.
    let target_type = FormType(unsafe { mem::read::<u8>(m, OM::targetFormType) });
    let target_base = if target_type == sig("ARMO") {
        0x100
    } else if target_type == sig("NPC_") {
        0x200
    } else {
        0
    };
    let data_handler = f4x_engine::DataHandler::get();
    let out: Vec<PValue> = mods
        .iter()
        .map(|pm| {
            let lo = i32::from_le_bytes(pm.data[0..4].try_into().unwrap_or_default());
            let hi = i32::from_le_bytes(pm.data[4..8].try_into().unwrap_or_default());
            let (object, v1, v2) = match pm.kind {
                0 | 5 => (None, lo as f32, hi as f32),
                // F4SE reports float Set and Add mods as operator 0 with no values; only
                // MultAdd carries them (black-box).
                1 if pm.op != 1 => (None, 0.0, 0.0),
                1 => (None, f32::from_bits(lo as u32), f32::from_bits(hi as u32)),
                2 => (None, f32::from(u8::from(lo != 0)), f32::from(u8::from(hi != 0))),
                4 => (Form::checked(usize::from_le_bytes(pm.data) as *mut u8), 0.0, 0.0),
                6 => (data_handler.and_then(|d| d.lookup(f4x_engine::FormId(lo as u32))), f32::from_bits(hi as u32), 0.0),
                _ => (None, 0.0, 0.0),
            };
            let op = match (pm.kind, pm.op) {
                (0, 0) => 0,
                (0 | 1, 1) => 2,
                (0, 2) => 1,
                (2, 0) => 0,
                (2, 1) => 3,
                (2, 2) => 4,
                (5, 0) => 0,
                (4 | 6, 0) => 0,
                (4 | 6, 1) => 5,
                (4 | 6, 2) => 1,
                _ => 0,
            };
            PValue::Struct(vec![
                ("target".into(), PValue::Int(target_base | pm.target as i32)),
                ("operator".into(), PValue::Int(op)),
                ("object".into(), object.map_or(PValue::None, |f| PValue::Form(f.as_ptr()))),
                ("value1".into(), PValue::Float(v1)),
                ("value2".into(), PValue::Float(v2)),
            ])
        })
        .collect();
    // A Mod without property mods but with attachments reports one empty modifier per
    // attachment: target = its attach index, operator 0, no values (black-box: 9 of 9
    // vanilla OMODs like that; Mods without a data buffer report None).
    let out = if out.is_empty() {
        attachment_indices(m)
            .into_iter()
            .map(|index| {
                PValue::Struct(vec![
                    ("target".into(), PValue::Int(target_base | i32::from(index))),
                    ("operator".into(), PValue::Int(0)),
                    ("object".into(), PValue::None),
                    ("value1".into(), PValue::Float(0.0)),
                    ("value2".into(), PValue::Float(0.0)),
                ])
            })
            .collect()
    } else {
        out
    };
    c.ret_value(&if out.is_empty() { PValue::None } else { PValue::Array(out) })
}

fn mod_byte(c: &mut NativeCall, offset: usize, signed: bool) -> Result<(), String> {
    let m = object_mod(c)?;
    // SAFETY: a one-byte field of a live Mod.
    let b: u8 = unsafe { mem::read(m, offset) };
    c.ret_int(if signed { i32::from(b as i8) } else { i32::from(b) });
    Ok(())
}

fn set_mod_byte(c: &mut NativeCall, offset: usize) -> Result<(), String> {
    let m = object_mod(c)?;
    let v = c.int(0) as u8;
    // SAFETY: a one-byte field of a live Mod.
    unsafe { *m.add(offset) = v };
    Ok(())
}

// ---------------------------------------------------------------- table

pub(crate) const NATIVES: &[Entry] = &[
    // InstanceData: weapon floats.
    ("InstanceData", "GetAttackDelay", |c| get_float(c, At::W(WD::attackDelaySec))),
    ("InstanceData", "SetAttackDelay", |c| set_float(c, At::W(WD::attackDelaySec))),
    ("InstanceData", "GetReach", |c| get_float(c, At::W(WD::reach))),
    ("InstanceData", "SetReach", |c| set_float(c, At::W(WD::reach))),
    ("InstanceData", "GetMinRange", |c| get_float(c, At::W(WD::minRange))),
    ("InstanceData", "SetMinRange", |c| set_float(c, At::W(WD::minRange))),
    ("InstanceData", "GetMaxRange", |c| get_float(c, At::W(WD::maxRange))),
    ("InstanceData", "SetMaxRange", |c| set_float(c, At::W(WD::maxRange))),
    ("InstanceData", "GetSpeed", |c| get_float(c, At::W(WD::speed))),
    ("InstanceData", "SetSpeed", |c| set_float(c, At::W(WD::speed))),
    ("InstanceData", "GetReloadSpeed", |c| get_float(c, At::W(WD::reloadSpeed))),
    ("InstanceData", "SetReloadSpeed", |c| set_float(c, At::W(WD::reloadSpeed))),
    ("InstanceData", "GetOutOfRangeMultiplier", |c| get_float(c, At::W(WD::outOfRangeDamageMult))),
    ("InstanceData", "SetOutOfRangeMultiplier", |c| set_float(c, At::W(WD::outOfRangeDamageMult))),
    ("InstanceData", "GetCritChargeBonus", |c| get_float(c, At::W(WD::criticalChargeBonus))),
    ("InstanceData", "SetCritChargeBonus", |c| set_float(c, At::W(WD::criticalChargeBonus))),
    ("InstanceData", "GetCritMultiplier", |c| get_float(c, At::W(WD::criticalDamageMult))),
    ("InstanceData", "SetCritMultiplier", |c| set_float(c, At::W(WD::criticalDamageMult))),
    ("InstanceData", "GetActionPointCost", |c| get_float(c, At::W(WD::attackActionPointCost))),
    ("InstanceData", "SetActionPointCost", |c| set_float(c, At::W(WD::attackActionPointCost))),
    ("InstanceData", "GetSightedTransition", |c| get_float(c, At::R(RD::sightedTransitionSeconds))),
    ("InstanceData", "SetSightedTransition", |c| set_float(c, At::R(RD::sightedTransitionSeconds))),
    // Weapon ints.
    ("InstanceData", "GetAttackDamage", |c| get_int(c, At::W(WD::attackDamage), Int::U16)),
    ("InstanceData", "SetAttackDamage", |c| set_int(c, At::W(WD::attackDamage), Int::U16)),
    ("InstanceData", "GetAmmoCapacity", |c| get_int(c, At::W(WD::ammoCapacity), Int::U16)),
    ("InstanceData", "SetAmmoCapacity", |c| set_int(c, At::W(WD::ammoCapacity), Int::U16)),
    ("InstanceData", "GetAccuracyBonus", |c| get_int(c, At::W(WD::accuracyBonus), Int::U8)),
    ("InstanceData", "SetAccuracyBonus", |c| set_int(c, At::W(WD::accuracyBonus), Int::U8)),
    ("InstanceData", "GetNumProjectiles", |c| get_int(c, At::R(RD::numProjectiles), Int::U8)),
    ("InstanceData", "SetNumProjectiles", |c| set_int(c, At::R(RD::numProjectiles), Int::U8)),
    ("InstanceData", "GetStagger", |c| get_int(c, At::WA(WD::staggerValue, AD::staggerRating), Int::I32)),
    ("InstanceData", "SetStagger", |c| set_int(c, At::WA(WD::staggerValue, AD::staggerRating), Int::I32)),
    ("InstanceData", "GetFlag", get_flag),
    ("InstanceData", "SetFlag", set_flag),
    // Weapon forms.
    ("InstanceData", "GetAmmo", |c| get_form(c, At::W(WD::ammo))),
    ("InstanceData", "SetAmmo", |c| set_form(c, At::W(WD::ammo), "AMMO")),
    ("InstanceData", "GetAddAmmoList", |c| get_form(c, At::W(WD::npcAddAmmoList))),
    ("InstanceData", "SetAddAmmoList", |c| set_form(c, At::W(WD::npcAddAmmoList), "LVLI")),
    ("InstanceData", "GetProjectileOverride", |c| get_form(c, At::R(RD::overrideProjectile))),
    ("InstanceData", "SetProjectileOverride", |c| set_form(c, At::R(RD::overrideProjectile), "PROJ")),
    ("InstanceData", "GetSkill", |c| get_form(c, At::W(WD::skill))),
    ("InstanceData", "SetSkill", |c| set_form(c, At::W(WD::skill), "AVIF")),
    ("InstanceData", "GetResist", |c| get_form(c, At::W(WD::resistance))),
    ("InstanceData", "SetResist", |c| set_form(c, At::W(WD::resistance), "AVIF")),
    // Weapon or armor.
    ("InstanceData", "GetWeight", |c| get_float(c, At::WA(WD::weight, AD::weight))),
    ("InstanceData", "SetWeight", |c| set_float(c, At::WA(WD::weight, AD::weight))),
    ("InstanceData", "GetGoldValue", |c| get_int(c, At::WA(WD::value, AD::value), Int::U32)),
    ("InstanceData", "SetGoldValue", |c| set_int(c, At::WA(WD::value, AD::value), Int::U32)),
    ("InstanceData", "GetKeywords", get_keywords),
    ("InstanceData", "SetKeywords", set_keywords),
    ("InstanceData", "GetDamageTypes", get_damage_types),
    ("InstanceData", "SetDamageTypes", set_damage_types),
    // Armor.
    ("InstanceData", "GetArmorRating", |c| get_int(c, At::A(AD::rating), Int::U16)),
    ("InstanceData", "SetArmorRating", |c| set_int(c, At::A(AD::rating), Int::U16)),
    ("InstanceData", "GetArmorHealth", |c| get_int(c, At::A(AD::health), Int::U32)),
    ("InstanceData", "SetArmorHealth", |c| set_int(c, At::A(AD::health), Int::U32)),
    // Owners (scripted in F4SE).
    ("Weapon", "GetInstanceOwner", weapon_instance_owner),
    ("Actor", "GetInstanceOwner", actor_instance_owner),
    // Weapon.
    ("Weapon", "GetEmbeddedMod", get_embedded_mod),
    ("Weapon", "SetEmbeddedMod", set_embedded_mod),
    // ObjectMod.
    ("ObjectMod", "GetLooseMod", get_loose_mod),
    ("ObjectMod", "GetMaxRank", |c| mod_byte(c, OM::maxRank, true)),
    ("ObjectMod", "SetMaxRank", |c| set_mod_byte(c, OM::maxRank)),
    ("ObjectMod", "GetPriority", |c| mod_byte(c, OM::priority, true)),
    ("ObjectMod", "SetPriority", |c| set_mod_byte(c, OM::priority)),
    ("ObjectMod", "GetPropertyModifiers", get_property_modifiers),
];
