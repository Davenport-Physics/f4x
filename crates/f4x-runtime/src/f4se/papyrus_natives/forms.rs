//! Form, Game, Input, DefaultObject, Cell, Location, EncounterZone (the rest), and the
//! scripted `F4SE.GetScriptVersionRelease`.
//!
//! Behaviour reference: CommonLibF4 `F4SEStub/runtime/src/Papyrus` (MIT), checked against
//! real F4SE with `tools/f4se_natives_check.py` and targeted black-box calls. Where F4SE
//! behaves differently from the stub, F4SE wins; each such place says so.
//!
//! Form components (`TESFullName`, `TESModel`, ...) are found with the engine's RTTI (a
//! `dynamic_cast` to the component class), which is what CommonLibF4's `TESForm::As<T>()`
//! resolves to per form type. Layouts come from the generated CommonLibF4 tables; the few
//! that the tables lack are spelled out below with their header lines.

use std::ffi::{CString, c_char};
use std::mem::transmute;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};

use f4x_engine::address::resolve;
use f4x_engine::form::Form;
use f4x_engine::layout::RE;
use f4x_engine::mem::{self, read};
use f4x_engine::papyrus::{self, FixedString, NativeCall};
use f4x_engine::papyrus_value::PValue;
use f4x_engine_sys::addresses as addr;

use super::Entry;

type R = Result<(), String>;

// ---------------------------------------------------------------- helpers

/// `self` as a form (any form-backed script).
fn self_form(c: &NativeCall) -> Option<Form> {
    match c.self_value() {
        PValue::Form(p) => Form::checked(p),
        _ => None,
    }
}

/// `self` as a form with record signature `sig`.
fn self_of(c: &NativeCall, sig: &str) -> Result<Form, String> {
    let form = self_form(c).ok_or("self is not a form")?;
    if form.form_type().signature() == sig { Ok(form) } else { Err(format!("self is not a {sig}")) }
}

/// Argument `i` as a form (None for a None argument).
fn arg_form(c: &NativeCall, i: usize) -> Option<Form> {
    match c.value(i) {
        PValue::Form(p) => Form::checked(p),
        _ => None,
    }
}

/// Return a form pointer read from an engine field (checked; None when null or stale).
fn ret_form(c: &mut NativeCall, p: *mut u8) -> R {
    let value = Form::checked(p).map_or(PValue::None, |f| PValue::Form(f.as_ptr()));
    c.ret_value(&value)
}

/// `form` as one of its base components (RTTI `dynamic_cast`; `class` is mangled).
fn component(form: Form, class: &'static str) -> Option<*mut u8> {
    // SAFETY: a live polymorphic form.
    unsafe { f4x_engine::game().cast(form.as_ptr(), class) }.map(<*const u8>::cast_mut)
}

/// Virtual function `slot` of a live polymorphic object.
///
/// # Safety
/// `object` must be a live object whose vtable has more than `slot` entries, and `F` must
/// be that slot's signature.
unsafe fn vfunc<F: Copy>(object: *const u8, slot: usize) -> F {
    // SAFETY: caller contract.
    unsafe {
        let vtable: *const usize = read(object, 0);
        std::mem::transmute_copy(&*vtable.add(slot))
    }
}

/// A new reference to the pool entry for `text` (`BSStringPool::GetEntry<char>`, in the
/// case-sensitive pool when `cs`). Null for "" (as `BSFixedString(std::string_view)`).
fn pool_entry(text: &str, cs: bool) -> Option<*mut u8> {
    if text.is_empty() {
        return Some(null_mut());
    }
    let get = resolve(&addr::STRING_POOL_GET_ENTRY)?;
    let text = CString::new(text.replace('\0', "")).ok()?;
    let mut entry = null_mut();
    // SAFETY: GetEntry<char>(Entry*& out, const char*, bool caseSensitive).
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut *mut u8, *const c_char, bool)>(get)(&mut entry, text.as_ptr(), cs) };
    Some(entry)
}

/// Store `text` in the `BSFixedString` at `field`, releasing the reference it held.
///
/// # Safety
/// `field` must be a live `BSFixedString` field.
unsafe fn set_fixed(field: *mut u8, text: &str, cs: bool) {
    let Some(new) = pool_entry(text, cs) else { return };
    // SAFETY: caller contract; the old reference is released after the swap.
    unsafe {
        let mut old: *mut u8 = read(field, 0);
        field.cast::<*mut u8>().write_unaligned(new);
        papyrus::release_entry(&mut old);
    }
}

/// The text of the `BSFixedString` at `field`.
///
/// # Safety
/// `field` must be a live `BSFixedString` field.
unsafe fn fixed_at(field: *const u8) -> String {
    // SAFETY: caller contract.
    unsafe { mem::fixed_string(read(field, 0)) }.unwrap_or_default()
}

// `BSStringT<char>` (CommonLibF4 `BSStringT.h`: `_data` pointer, then u16 `_size` and
// u16 `_capacity`; the allocator policy is empty). Same on NG (clf4-ng `BSStringT.h`:157).
const BSSTRING_SIZE: usize = 0x10;

/// The text of a `BSStringT<char>`.
///
/// # Safety
/// `s` must point to a live `BSStringT<char>`.
unsafe fn bs_string(s: *const u8) -> String {
    // SAFETY: caller contract.
    let data: *const u8 = unsafe { read(s, 0) };
    if data.is_null() || !mem::readable(data as usize, 1) {
        return String::new();
    }
    // SAFETY: the engine keeps these strings NUL-terminated.
    unsafe { mem::c_str(data) }
}

/// Free a block from the game heap (`MemoryManager::Deallocate(p, false)`, what
/// `BSStringT`'s allocator does).
fn game_free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let (Some(singleton), Some(free)) = (resolve(&addr::MEMORY_MANAGER_SINGLETON), resolve(&addr::MEMORY_MANAGER_DEALLOCATE)) else { return };
    // SAFETY: MemoryManager::GetSingleton() / Deallocate(this, void*, bool alignmentRequired).
    unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut u8>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8, bool)>(free)(manager, p, false);
    }
}

/// A NUL-terminated copy of `text` on the game heap (never freed by us).
fn game_strdup(text: &str) -> Option<*mut u8> {
    let singleton = resolve(&addr::MEMORY_MANAGER_SINGLETON)?;
    let allocate = resolve(&addr::MEMORY_MANAGER_ALLOCATE)?;
    let bytes = text.replace('\0', "");
    // SAFETY: MemoryManager::GetSingleton() / Allocate(this, size, alignment, alignmentRequired).
    let p = unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut u8>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut u8, usize, u32, bool) -> *mut u8>(allocate)(manager, bytes.len() + 1, 0, false)
    };
    if p.is_null() {
        return None;
    }
    // SAFETY: a fresh block of len + 1 bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        p.add(bytes.len()).write(0);
    }
    Some(p)
}

/// F4SE returns a None array where the array would be empty (black-box:
/// `Game.GetInstalledLightPlugins` with no light plugins, unknown plugin dependencies).
fn array_or_none(items: Vec<PValue>) -> PValue {
    if items.is_empty() { PValue::None } else { PValue::Array(items) }
}

// ---------------------------------------------------------------- Form

const BIPED_SLOTS: usize = RE::BGSBipedObjectForm::bipedModelData + RE::BIPED_MODEL::bipedObjectSlots;
// BGSMessageIcon::icon is a TESIcon member (not a base), whose texture name is a BSFixedString.
const MESSAGE_ICON_NAME: usize = RE::BGSMessageIcon::icon + RE::TESTexture::textureName;

fn biped_slots(c: &NativeCall) -> Option<*mut u32> {
    let biped = component(self_form(c)?, RE::BGSBipedObjectForm::RTTI)?;
    // SAFETY: BIPED_MODEL::bipedObjectSlots inside a live BGSBipedObjectForm.
    Some(unsafe { biped.add(BIPED_SLOTS) }.cast())
}

fn slot_mask_op(c: &mut NativeCall, op: fn(u32, u32) -> u32) -> R {
    let mask = c.int(0) as u32;
    let result = biped_slots(c).map_or(0, |p| {
        // SAFETY: a live u32 field.
        unsafe {
            let v = op(p.read_unaligned(), mask);
            p.write_unaligned(v);
            v
        }
    });
    c.ret_int(result as i32);
    Ok(())
}

/// F4SE's `GetMaskForSlot` (black-box): `1 << (slot - 30)` with the shift count taken
/// modulo 32 for slots 29..=61, else 0 (slot 29 gives bit 31, as slot 61 does).
fn mask_for_slot(slot: i32) -> u32 {
    if (29..=61).contains(&slot) { 1u32 << ((slot - 30) as u32 & 31) } else { 0 }
}

/// `TESDescription` text through the engine. CommonLibF4 declares the function (ID
/// 523613) on `BGSLocalizedStringDL`, but its `this` is the `TESDescription` component
/// (our analysis of Fallout4.exe: it reads `fileOffset`/`chunkOffset` at +8/+0xC, passes
/// `this + 0x10` on, and with a null owner form dynamic-casts `this` from
/// `TESDescription` to `TESForm`). The owner form picks the file the text is read from.
fn description(form: Form) -> String {
    let Some(desc) = component(form, RE::TESDescription::RTTI) else { return String::new() };
    let Some(get) = resolve(&addr::LOCALIZED_STRING_DL_GET_DESCRIPTION) else { return String::new() };
    let mut buf = [0u8; BSSTRING_SIZE];
    // SAFETY: GetDescription(const TESDescription* this, BSStringT<char>& out, const TESForm* owner).
    unsafe {
        transmute::<usize, unsafe extern "C" fn(*const u8, *mut u8, *const u8)>(get)(desc, buf.as_mut_ptr(), form.as_ptr());
    }
    // SAFETY: the BSStringT the engine filled.
    let text = unsafe { bs_string(buf.as_ptr()) };
    // SAFETY: as above.
    game_free(unsafe { read::<*mut u8>(buf.as_ptr(), 0) });
    text
}

/// `TESFullName::fullName = name` with `BGSLocalizedString`'s assignment rule: a value
/// starting with "<ID=" keeps its 13-character prefix (CommonLibF4 `BSFixedString.h`).
fn set_full_name(form: Form, name: &str) {
    let Some(full) = component(form, RE::TESFullName::RTTI) else { return };
    // SAFETY: TESFullName::fullName, a BGSLocalizedString (case-sensitive BSFixedString).
    unsafe {
        let field = full.add(RE::TESFullName::fullName);
        let current = fixed_at(field);
        let value = match current.get(..13) {
            Some(prefix) if prefix.starts_with("<ID=") => format!("{prefix}{name}"),
            _ => name.to_owned(),
        };
        set_fixed(field, &value, true);
    }
}

fn form_natives_get_name(c: &mut NativeCall) -> R {
    let name = self_form(c).and_then(|f| component(f, RE::TESFullName::RTTI)).map_or_else(String::new, |full| {
        // SAFETY: TESFullName::GetFullName() const -> const char*.
        unsafe { mem::c_str(vfunc::<unsafe extern "C" fn(*const u8) -> *const u8>(full, RE::TESFullName::vtbl::GetFullName)(full)) }
    });
    c.ret_string(&name);
    Ok(())
}

fn model_of(c: &NativeCall) -> Option<*mut u8> {
    component(self_form(c)?, RE::TESModel::RTTI)
}

fn texture_field(c: &NativeCall, class: &'static str, offset: usize) -> Option<*mut u8> {
    // SAFETY: a field inside a live component.
    component(self_form(c)?, class).map(|p| unsafe { p.add(offset) })
}

fn get_texture(c: &mut NativeCall, class: &'static str, offset: usize) -> R {
    // SAFETY: a BSFixedString field.
    let text = texture_field(c, class, offset).map_or_else(String::new, |f| unsafe { fixed_at(f) });
    c.ret_string(&text);
    Ok(())
}

fn set_texture(c: &mut NativeCall, class: &'static str, offset: usize) -> R {
    let text = c.string(0);
    if let Some(field) = texture_field(c, class, offset) {
        // SAFETY: a BSFixedString field.
        unsafe { set_fixed(field, &text, false) };
    }
    Ok(())
}

fn enchantable(c: &NativeCall) -> Option<*mut u8> {
    component(self_form(c)?, RE::TESEnchantableForm::RTTI)
}

/// A pointer-sized field of a component of `self`.
fn component_field(c: &NativeCall, class: &'static str, offset: usize) -> Option<*mut *mut u8> {
    // SAFETY: a field inside a live component.
    component(self_form(c)?, class).map(|p| unsafe { p.add(offset) }.cast())
}

fn get_component_form(c: &mut NativeCall, class: &'static str, offset: usize) -> R {
    // SAFETY: a pointer field (validated by ret_form).
    let p = component_field(c, class, offset).map_or(null_mut(), |f| unsafe { f.read_unaligned() });
    ret_form(c, p)
}

fn set_component_form(c: &mut NativeCall, class: &'static str, offset: usize) -> R {
    let value = arg_form(c, 0).map_or(null_mut(), Form::as_ptr);
    if let Some(field) = component_field(c, class, offset) {
        // SAFETY: a pointer field.
        unsafe { field.write_unaligned(value) };
    }
    Ok(())
}

const FORM: &[Entry] = &[
    ("Form", "GetMaskForSlot", |c| {
        let slot = c.int(0);
        c.ret_int(mask_for_slot(slot) as i32);
        Ok(())
    }),
    ("Form", "AddSlotToMask", |c| slot_mask_op(c, |a, b| a | b)),
    ("Form", "RemoveSlotFromMask", |c| slot_mask_op(c, |a, b| a & !b)),
    ("Form", "GetSlotMask", |c| {
        // SAFETY: a live u32 field.
        let v = biped_slots(c).map_or(0, |p| unsafe { p.read_unaligned() });
        c.ret_int(v as i32);
        Ok(())
    }),
    ("Form", "SetSlotMask", |c| {
        let mask = c.int(0) as u32;
        if let Some(p) = biped_slots(c) {
            // SAFETY: a live u32 field.
            unsafe { p.write_unaligned(mask) };
        }
        Ok(())
    }),
    ("Form", "GetDescription", |c| {
        let text = self_form(c).map_or_else(String::new, description);
        c.ret_string(&text);
        Ok(())
    }),
    ("Form", "GetEnchantment", |c| get_component_form(c, RE::TESEnchantableForm::RTTI, RE::TESEnchantableForm::formEnchanting)),
    ("Form", "SetEnchantment", |c| set_component_form(c, RE::TESEnchantableForm::RTTI, RE::TESEnchantableForm::formEnchanting)),
    ("Form", "GetEnchantmentValue", |c| {
        // SAFETY: TESEnchantableForm fields.
        let v = enchantable(c).map_or(0, |e| unsafe {
            if read::<usize>(e, RE::TESEnchantableForm::formEnchanting) != 0 { i32::from(read::<u16>(e, RE::TESEnchantableForm::amountOfEnchantment)) } else { 0 }
        });
        c.ret_int(v);
        Ok(())
    }),
    ("Form", "SetEnchantmentValue", |c| {
        // F4SE (black-box) only changes the charge of a form that has an enchantment, and
        // crashes on forms that are not enchantable; f4x ignores those.
        let value = c.int(0) as u16;
        if let Some(e) = enchantable(c) {
            // SAFETY: TESEnchantableForm fields.
            unsafe {
                if read::<usize>(e, RE::TESEnchantableForm::formEnchanting) != 0 {
                    e.add(RE::TESEnchantableForm::amountOfEnchantment).cast::<u16>().write_unaligned(value);
                }
            }
        }
        Ok(())
    }),
    // The equip slot is read from the field: F4SE calls `GetEquipSlot(nullptr)`, which
    // crashes the game for weapons (black-box, `Form.GetEquipType` on a WEAP).
    ("Form", "GetEquipType", |c| get_component_form(c, RE::BGSEquipType::RTTI, RE::BGSEquipType::equipSlot)),
    ("Form", "SetEquipType", |c| {
        // F4SE (black-box) keeps the equip slot when given None.
        let Some(value) = arg_form(c, 0).map(Form::as_ptr) else { return Ok(()) };
        if let Some(equip) = self_form(c).and_then(|f| component(f, RE::BGSEquipType::RTTI)) {
            // SAFETY: BGSEquipType::SetEquipSlot(BGSEquipSlot*).
            unsafe { vfunc::<unsafe extern "C" fn(*mut u8, *mut u8)>(equip, RE::BGSEquipType::vtbl::SetEquipSlot)(equip, value) };
        }
        Ok(())
    }),
    ("Form", "GetIconPath", |c| get_texture(c, RE::TESIcon::RTTI, RE::TESTexture::textureName)),
    ("Form", "SetIconPath", |c| set_texture(c, RE::TESIcon::RTTI, RE::TESTexture::textureName)),
    ("Form", "GetMessageIconPath", |c| get_texture(c, RE::BGSMessageIcon::RTTI, MESSAGE_ICON_NAME)),
    ("Form", "SetMessageIconPath", |c| set_texture(c, RE::BGSMessageIcon::RTTI, MESSAGE_ICON_NAME)),
    ("Form", "GetKeywords", |c| {
        let mut items = Vec::new();
        if let Some(kw) = self_form(c).and_then(|f| component(f, RE::BGSKeywordForm::RTTI)) {
            // SAFETY: BGSKeywordForm::keywords (BGSKeyword**) and numKeywords.
            unsafe {
                let list: *const *mut u8 = read(kw, RE::BGSKeywordForm::keywords);
                let count = read::<u32>(kw, RE::BGSKeywordForm::numKeywords) as usize;
                if !list.is_null() && count < 1 << 16 && mem::readable(list as usize, count * 8) {
                    items.extend((0..count).filter_map(|i| Form::checked(*list.add(i))).map(|f| PValue::Form(f.as_ptr())));
                }
            }
        }
        c.ret_value(&array_or_none(items))
    }),
    ("Form", "GetName", form_natives_get_name),
    // F4SE 0.7.0 (whatsnew: "Form.GetEditorID"; signature from 0.7.9's .pex). The form's
    // `GetFormEditorID` virtual: release builds keep editor IDs for some types only, the
    // rest give "" (black-box: F4SE 0.7.9 returns "" for MISC forms 0xA and 0xF).
    ("Form", "GetEditorID", |c| {
        let id = self_form(c).map(Form::editor_id).unwrap_or_default();
        c.ret_string(&id);
        Ok(())
    }),
    ("Form", "SetName", |c| {
        // No AddChange: the stub's change flag is marked as divergent from F4SE.
        let name = c.string(0);
        if let Some(form) = self_form(c) {
            set_full_name(form, &name);
        }
        Ok(())
    }),
    ("Form", "GetRaceForm", |c| get_component_form(c, RE::TESRaceForm::RTTI, RE::TESRaceForm::formRace)),
    ("Form", "SetRaceForm", |c| {
        // F4SE (black-box) keeps the race when given None.
        if arg_form(c, 0).is_none() {
            return Ok(());
        }
        set_component_form(c, RE::TESRaceForm::RTTI, RE::TESRaceForm::formRace)
    }),
    ("Form", "GetWeight", |c| {
        // SAFETY: TESWeightForm::weight.
        let w = self_form(c).and_then(|f| component(f, RE::TESWeightForm::RTTI)).map_or(0.0, |p| unsafe { read::<f32>(p, RE::TESWeightForm::weight) });
        c.ret_float(w);
        Ok(())
    }),
    ("Form", "SetWeight", |c| {
        let w = c.float(0);
        if let Some(p) = self_form(c).and_then(|f| component(f, RE::TESWeightForm::RTTI)) {
            // SAFETY: TESWeightForm::weight.
            unsafe { p.add(RE::TESWeightForm::weight).cast::<f32>().write_unaligned(w) };
        }
        Ok(())
    }),
    ("Form", "SetGoldValue", |c| {
        // No AddChange: the stub's change flag is marked as divergent from F4SE.
        let v = c.int(0);
        if let Some(p) = self_form(c).and_then(|f| component(f, RE::TESValueForm::RTTI)) {
            // SAFETY: TESValueForm::value.
            unsafe { p.add(RE::TESValueForm::value).cast::<i32>().write_unaligned(v) };
        }
        Ok(())
    }),
    ("Form", "HasWorldModel", |c| {
        let has = model_of(c).is_some();
        c.ret_bool(has);
        Ok(())
    }),
    ("Form", "GetWorldModelPath", |c| {
        let path = model_of(c).map_or_else(String::new, |m| {
            // SAFETY: TESModel::GetModel() const -> const char*.
            unsafe { mem::c_str(vfunc::<unsafe extern "C" fn(*const u8) -> *const u8>(m, RE::TESModel::vtbl::GetModel)(m)) }
        });
        c.ret_string(&path);
        Ok(())
    }),
    ("Form", "SetWorldModelPath", |c| {
        let path = CString::new(c.string(0).replace('\0', "")).unwrap_or_default();
        if let Some(m) = model_of(c) {
            // SAFETY: TESModel::SetModel(const char*) (interns the string).
            unsafe { vfunc::<unsafe extern "C" fn(*mut u8, *const c_char)>(m, RE::TESModel::vtbl::SetModel)(m, path.as_ptr()) };
        }
        Ok(())
    }),
];

// ---------------------------------------------------------------- Game

/// `GameSettingCollection` (`SettingCollectionMap<Setting>`): `settings` is a `BSTBTree`
/// at +0x118 (CommonLibF4 `Settings.h:274`, after `SettingCollection`'s vptr,
/// `settingFile[260]` and `handle` at +0x110). `BSTBTree` (`BSTBTree.h:41-43, 239`): root
/// node at +0x10; a node holds 4 `BSTTuple<BSFixedString, Setting*>` entries (0x10 each),
/// then 5 child pointers at +0x40 and `usedEntries` (u32) at +0x68. Same on NG 1.11.240
/// (clf4-ng `Setting.h`:293-294, 368, `BSTBTree.h`:57-59, 261-265; re: the collection's
/// vtable functions use +0x110 and +0x128 in both builds).
const SETTINGS_TREE: usize = 0x118;
const TREE_ROOT: usize = 0x10;
const NODE_CHILDREN: usize = 0x40;
const NODE_USED: usize = 0x68;
const NODE_SIZE: usize = 0x6C;

/// The game setting called `name` (`settings.find(BSFixedString(name))`).
fn game_setting(name: &str) -> Option<*mut u8> {
    if name.is_empty() {
        return None;
    }
    let collection = f4x_engine::game().singleton(RE::GameSettingCollection::RTTI)?;
    let key = FixedString::new(name)?;
    let key = key.as_raw();
    // SAFETY: the tree's root pointer inside the live collection.
    let mut stack: Vec<*const u8> = vec![unsafe { read(collection, SETTINGS_TREE + TREE_ROOT) }];
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 1 << 16 {
            break;
        }
        if node.is_null() || !mem::readable(node as usize, NODE_SIZE) {
            continue;
        }
        // SAFETY: a readable tree node (checked).
        unsafe {
            let used = read::<u32>(node, NODE_USED) as usize;
            if used > 4 {
                continue;
            }
            for i in 0..used {
                let entry_key: *mut u8 = read(node, i * 0x10);
                if entry_key == key || (!entry_key.is_null() && mem::fixed_string(entry_key).is_some_and(|k| k.eq_ignore_ascii_case(name))) {
                    let setting: *mut u8 = read(node, i * 0x10 + 8);
                    return (!setting.is_null() && mem::readable(setting as usize, RE::Setting::SIZE)).then_some(setting);
                }
            }
            for i in 0..=used {
                stack.push(read(node, NODE_CHILDREN + i * 8));
            }
        }
    }
    None
}

/// A setting's type letter (`Setting::GetType`: the first character of its key).
fn setting_type(setting: *const u8) -> u8 {
    // SAFETY: Setting::_key (const char*).
    let key: *const u8 = unsafe { read(setting, RE::Setting::_key) };
    if key.is_null() || !mem::readable(key as usize, 1) {
        return 0;
    }
    // SAFETY: readable (checked).
    unsafe { key.read() }
}

/// Write a game setting's value if its type is one of `types`.
fn set_setting<T: Copy>(name: &str, types: &[u8], value: T) {
    if let Some(s) = game_setting(name)
        && types.contains(&setting_type(s))
    {
        // SAFETY: Setting::_value (a SETTING_VALUE union, 8 bytes).
        unsafe { s.add(RE::Setting::_value).cast::<T>().write_unaligned(value) };
    }
}

/// The `PlayerCamera` singleton (`PlayerCamera::GetSingleton`, a pointer global; the
/// object is static, so an RTTI scan for a heap pointer does not find it).
fn player_camera() -> Option<*mut u8> {
    let global = resolve(&addr::PLAYER_CAMERA_SINGLETON)?;
    // SAFETY: the PlayerCamera* global.
    let camera: *mut u8 = unsafe { read(global as *const u8, 0) };
    f4x_engine::game().is_instance_of(camera as usize, RE::TESCamera::RTTI).then_some(camera)
}

/// `TESCameraState::id` of the player camera's current state, or -1.
fn camera_state() -> i32 {
    let Some(camera) = player_camera() else { return -1 };
    // SAFETY: TESCamera::currentState (BSTSmartPointer<TESCameraState>).
    let state: *const u8 = unsafe { read(camera, RE::TESCamera::currentState) };
    if !f4x_engine::game().is_instance_of(state as usize, RE::TESCameraState::RTTI) {
        return -1;
    }
    // SAFETY: a live TESCameraState.
    unsafe { read::<u32>(state, RE::TESCameraState::id) as i32 }
}

/// `CameraStates::k3rdPerson` (CommonLibF4 `TESCamera.h`; same on NG, clf4-ng
/// `CameraStates.h`:18).
const THIRD_PERSON: usize = 8;
/// `WEAPON_STATE::kDrawn` (CommonLibF4 `Actor.h`); `ActorState::weaponState` is bits 1..=3
/// of the u32 at +0x0C (same on NG: clf4-ng `WEAPON_STATE.h`:10, `ActorState.h`:41).
const WEAPON_DRAWN: u32 = 3;

/// `ThirdPersonState::ProcessWeaponDrawnChange(player->GetWeaponMagicDrawn())`. Main thread.
fn update_third_person() {
    let Some(player) = f4x_engine::refr::player() else { return };
    let Some(camera) = player_camera() else { return };
    // SAFETY: PlayerCamera::cameraStates[k3rdPerson].
    let third: *mut u8 = unsafe { read(camera, RE::PlayerCamera::cameraStates + 8 * THIRD_PERSON) };
    if !f4x_engine::game().is_instance_of(third as usize, RE::ThirdPersonState::RTTI) {
        return;
    }
    // SAFETY: the player's ActorState bitfield word.
    let bits: u32 = unsafe { read(player.form().as_ptr(), RE::Actor::bases::ActorState + RE::ActorState::weaponState) };
    let drawn = (bits >> 1) & 7 >= WEAPON_DRAWN;
    // SAFETY: ThirdPersonState::ProcessWeaponDrawnChange(bool) on a live state.
    unsafe { vfunc::<unsafe extern "C" fn(*mut u8, bool)>(third, RE::ThirdPersonState::vtbl::ProcessWeaponDrawnChange)(third, drawn) };
}

/// The console's selected reference (`Console::GetPickRef()`), through
/// `BSPointerHandleManagerInterface<TESObjectREFR>::GetSmartPointer`. The reference taken
/// is dropped after `use_ref` (as `NiPointer` does).
fn with_console_ref(use_ref: impl FnOnce(*mut u8) -> R) -> R {
    let (Some(global), Some(get)) = (resolve(&addr::CONSOLE_PICK_REF), resolve(&addr::REFR_HANDLE_GET_SMART_POINTER)) else {
        return use_ref(null_mut());
    };
    // SAFETY: the ObjectRefHandle global (u32).
    let handle: u32 = unsafe { read(global as *const u8, 0) };
    if handle == 0 {
        return use_ref(null_mut());
    }
    let mut refr: *mut u8 = null_mut();
    // SAFETY: GetSmartPointer(const ObjectRefHandle&, NiPointer<TESObjectREFR>&) -> bool.
    let ok = unsafe { transmute::<usize, unsafe extern "C" fn(*const u32, *mut *mut u8) -> bool>(get)(&handle, &mut refr) };
    if !ok || refr.is_null() {
        return use_ref(null_mut());
    }
    let result = use_ref(refr);
    release_refr(refr);
    result
}

/// `NiPointer<TESObjectREFR>` release: `BSHandleRefObject::DecRefCount` (CommonLibF4
/// `TESObjectREFRs.h:117`: the low 10 bits of `refCount` count references; `DeleteThis`
/// at zero).
fn release_refr(refr: *mut u8) {
    // SAFETY: the BSHandleRefObject base of a live reference we hold a reference to.
    unsafe {
        let handle_object = refr.add(RE::TESObjectREFR::bases::BSHandleRefObject);
        let count = &*handle_object.add(RE::NiRefObject::refCount).cast::<AtomicU32>();
        let left = count.fetch_sub(1, Ordering::AcqRel).wrapping_sub(1) & 0x3FF;
        if left == 0 {
            vfunc::<unsafe extern "C" fn(*mut u8)>(handle_object, RE::NiRefObject::vtbl::DeleteThis)(handle_object);
        }
    }
}

/// `TESFile` fields as a `Game#PluginInfo`.
///
/// # Safety
/// `file` must be a live `TESFile`.
unsafe fn plugin_info(file: *const u8) -> PValue {
    // SAFETY: caller contract; TESFile fields (generated layout).
    unsafe {
        PValue::Struct(vec![
            ("index".into(), PValue::Int(i32::from(read::<u8>(file, RE::TESFile::compileIndex)))),
            ("name".into(), PValue::String(mem::c_str(file.add(RE::TESFile::filename)))),
            ("author".into(), PValue::String(bs_string(file.add(RE::TESFile::createdBy)))),
            ("description".into(), PValue::String(bs_string(file.add(RE::TESFile::summary)))),
        ])
    }
}

/// `compiledFileCollection.files` (or `.smallFiles`) as `PluginInfo`s.
fn installed(light: bool) -> PValue {
    let Some(data) = f4x_engine::DataHandler::get() else { return PValue::None };
    let offset = RE::TESDataHandler::compiledFileCollection + if light { RE::TESFileCollection::smallFiles } else { RE::TESFileCollection::files };
    // SAFETY: BSTArray<TESFile*> inside the data handler.
    let files = unsafe { mem::BSTArray::<*const u8>::at(data.as_ptr().add(offset)) };
    // SAFETY: live TESFiles.
    array_or_none(files.iter().filter(|f| !f.is_null()).map(|f| unsafe { plugin_info(f) }).collect())
}

/// Masters of the loaded file `name` (`TESDataHandler::files`, a `BSSimpleList<TESFile*>`
/// with an inline head node `{value, next}`), if it has a compile index.
fn plugin_dependencies(name: &str) -> Vec<PValue> {
    let mut out = Vec::new();
    let Some(data) = f4x_engine::DataHandler::get() else { return out };
    // SAFETY: the list's inline head node.
    let mut node = unsafe { data.as_ptr().add(RE::TESDataHandler::files) }.cast_const();
    for _ in 0..4096 {
        // SAFETY: list nodes: TESFile* at 0, next node at 8.
        let (file, next): (*const u8, *const u8) = unsafe { (read(node, 0), read(node, 8)) };
        // SAFETY: a live TESFile.
        if !file.is_null() && unsafe { mem::c_str(file.add(RE::TESFile::filename)) }.eq_ignore_ascii_case(name) {
            // SAFETY: TESFile::compileIndex, masterCount and masterPtrs (TESFile**).
            unsafe {
                if read::<u8>(file, RE::TESFile::compileIndex) != 0xFF {
                    let count = read::<u32>(file, RE::TESFile::masterCount) as usize;
                    let masters: *const *const u8 = read(file, RE::TESFile::masterPtrs);
                    if !masters.is_null() && count <= 0x100 && mem::readable(masters as usize, count * 8) {
                        for i in 0..count {
                            let master = *masters.add(i);
                            if !master.is_null() && mem::readable(master as usize, RE::TESFile::SIZE) {
                                out.push(PValue::String(mem::c_str(master.add(RE::TESFile::filename))));
                            }
                        }
                    }
                }
            }
            break;
        }
        if next.is_null() {
            break;
        }
        node = next;
    }
    out
}

const GAME: &[Entry] = &[
    ("Game", "GetCameraState", |c| {
        c.ret_int(camera_state());
        Ok(())
    }),
    ("Game", "GetCurrentConsoleRef", |c| with_console_ref(|refr| ret_form(c, refr))),
    ("Game", "GetInstalledPlugins", |c| c.ret_value(&installed(false))),
    ("Game", "GetInstalledLightPlugins", |c| c.ret_value(&installed(true))),
    ("Game", "GetPluginDependencies", |c| {
        let name = c.string(0);
        c.ret_value(&array_or_none(plugin_dependencies(&name)))
    }),
    ("Game", "SetGameSettingBool", |c| {
        let (name, v) = (c.string(0), c.bool(1));
        set_setting(&name, b"b", v);
        Ok(())
    }),
    ("Game", "SetGameSettingFloat", |c| {
        // F4SE (black-box) also sets Int settings, truncated (3.5 -> 3); the other typed
        // setters leave settings of other types alone.
        let (name, v) = (c.string(0), c.float(1));
        set_setting(&name, b"f", v);
        set_setting(&name, b"i", v as i32);
        Ok(())
    }),
    ("Game", "SetGameSettingInt", |c| {
        let (name, v) = (c.string(0), c.int(1));
        set_setting(&name, b"i", v);
        Ok(())
    }),
    ("Game", "SetGameSettingString", |c| {
        let (name, v) = (c.string(0), c.string(1));
        if let Some(s) = game_setting(&name)
            && matches!(setting_type(s), b's' | b'S')
            && let Some(copy) = game_strdup(&v)
        {
            // SAFETY: Setting::_value.s (char*). The old string belongs to the engine and
            // is left alone; the copy lives as long as the setting may use it.
            unsafe { s.add(RE::Setting::_value).cast::<*mut u8>().write_unaligned(copy) };
        }
        Ok(())
    }),
    ("Game", "UpdateThirdPerson", |_| {
        f4x_agent::pump::post(update_third_person);
        Ok(())
    }),
];

// ---------------------------------------------------------------- Input

// F4SE key codes: DirectInput scan codes 0..256, mouse buttons 256..266, gamepad buttons
// 266..282 (CommonLibF4 `F4SEStub/runtime/src/Papyrus/Input.h`).
const KEYBOARD_END: i32 = 256;
const MOUSE_BEGIN: i32 = 256;
const MOUSE_END: i32 = 266;
const GAMEPAD_BEGIN: i32 = 266;
const GAMEPAD_END: i32 = 282;
/// Gamepad buttons in key-code order (Up, Down, Left, Right, Start, Back, LS, RS, LB, RB,
/// A, B, X, Y, LT, RT) as the `ControlMap` stores them (XInput masks; the triggers as
/// Up|Right and Down|Right).
const GAMEPAD_MASKS: [i32; 16] = [0x1, 0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x1000, 0x2000, 0x4000, 0x8000, 0x9, 0xA];

const KEYBOARD: usize = 0;
const MOUSE: usize = 1;
const GAMEPAD: usize = 2;
/// `ControlMap::UserEventMapping` size; `eventID` at 0, `inputKey` (i32) at 8.
const MAPPING_SIZE: usize = RE::ControlMap::UserEventMapping::SIZE;

/// `inputKey` of a mapping with no key bound: the `ControlMap` lists every event for
/// every device, unbound ones with 0xFF (F4SE skips them, black-box: `GetMappedKey
/// ("Forward", 1)` is -1 and `GetMappedControl(255)` is "").
const UNBOUND: i32 = 0xFF;

/// The main-gameplay context's bound mappings for one device: (eventID entry, inputKey).
fn mappings(device: usize) -> Vec<(*mut u8, i32)> {
    let mut out = Vec::new();
    let Some(map) = f4x_engine::controls::control_map() else { return out };
    // SAFETY: ControlMap::controlMaps[kMainGameplay] (InputContext*).
    let context: *const u8 = unsafe { read(map, RE::ControlMap::controlMaps) };
    if context.is_null() || !mem::readable(context as usize, RE::ControlMap::InputContext::SIZE) {
        return out;
    }
    // SAFETY: InputContext::deviceMappings[device] (BSTArray<UserEventMapping>).
    let (data, len): (*const u8, u32) = unsafe {
        let array = context.add(RE::ControlMap::InputContext::deviceMappings + device * 0x18);
        (read(array, 0), read(array, 0x10))
    };
    let len = len as usize;
    if data.is_null() || len > 4096 || !mem::readable(data as usize, len * MAPPING_SIZE) {
        return out;
    }
    for i in 0..len {
        // SAFETY: within the readable array.
        unsafe {
            let m = data.add(i * MAPPING_SIZE);
            let key: i32 = read(m, RE::ControlMap::UserEventMapping::inputKey);
            if key != UNBOUND {
                out.push((read(m, RE::ControlMap::UserEventMapping::eventID), key));
            }
        }
    }
    out
}

fn gamepad_connected() -> bool {
    let Some(global) = resolve(&addr::INPUT_DEVICE_MANAGER_SINGLETON) else { return false };
    // SAFETY: the BSInputDeviceManager* global.
    let manager: *const u8 = unsafe { read(global as *const u8, 0) };
    if manager.is_null() || !mem::readable(manager as usize, RE::BSInputDeviceManager::SIZE) {
        return false;
    }
    // SAFETY: devices[kGamepad].
    let pad: *const u8 = unsafe { read(manager, RE::BSInputDeviceManager::devices + 8 * GAMEPAD) };
    f4x_engine::game().is_instance_of(pad as usize, RE::BSInputDevice::RTTI)
        // SAFETY: BSInputDevice::IsConnected() const on a live device.
        && unsafe { vfunc::<unsafe extern "C" fn(*const u8) -> bool>(pad, RE::BSInputDevice::vtbl::IsConnected)(pad) }
}

/// The F4SE key code of `control` on `device` (first mapping of the event).
fn mapped_key(control: *mut u8, device: usize) -> Option<i32> {
    let key = mappings(device).into_iter().find(|(event, _)| *event == control)?.1;
    match device {
        KEYBOARD => Some(key),
        MOUSE => Some(key + MOUSE_BEGIN),
        _ => GAMEPAD_MASKS.iter().position(|&m| m == key).map(|i| GAMEPAD_BEGIN + i as i32),
    }
}

fn get_mapped_key(c: &mut NativeCall) -> R {
    let (control, device) = (c.string(0), c.int(1));
    let key = FixedString::new(&control).and_then(|control| {
        let control = control.as_raw();
        match device {
            0..=2 => mapped_key(control, device as usize),
            // Any other device type (F4SE, black-box: -1, 3, 4, 0xFF, 0x100 alike): the
            // gamepad when one is connected, else keyboard then mouse.
            _ if gamepad_connected() => mapped_key(control, GAMEPAD),
            _ => mapped_key(control, KEYBOARD).or_else(|| mapped_key(control, MOUSE)),
        }
    });
    c.ret_int(key.unwrap_or(-1));
    Ok(())
}

fn get_mapped_control(c: &mut NativeCall) -> R {
    let code = c.int(0);
    let (device, key) = match code {
        0..KEYBOARD_END => (KEYBOARD, Some(code)),
        MOUSE_BEGIN..MOUSE_END => (MOUSE, Some(code - MOUSE_BEGIN)),
        GAMEPAD_BEGIN..GAMEPAD_END => (GAMEPAD, Some(GAMEPAD_MASKS[(code - GAMEPAD_BEGIN) as usize])),
        _ => (KEYBOARD, None),
    };
    let name = key
        .and_then(|key| mappings(device).into_iter().find(|(_, k)| *k == key))
        // SAFETY: a mapping's eventID (pool entry or null).
        .map(|(event, _)| unsafe { mem::fixed_string(event) }.unwrap_or_default())
        .unwrap_or_default();
    c.ret_string(&name);
    Ok(())
}

const INPUT: &[Entry] = &[("Input", "GetMappedKey", get_mapped_key), ("Input", "GetMappedControl", get_mapped_control)];

// ---------------------------------------------------------------- DefaultObject, Cell

/// Every form by editor ID (`TESForm::GetAllFormsByEditorID`, a
/// `BSTHashMap<BSFixedString, TESForm*>`; layout as in `f4x_engine::data::form_by_id`).
fn form_by_editor_id(editor_id: &str) -> Option<Form> {
    if editor_id.is_empty() {
        return None;
    }
    let global = resolve(&addr::ALL_FORMS_BY_EDITOR_ID)?;
    let key = FixedString::new(editor_id)?;
    // SAFETY: the map pointer global and the map's own fields (capacity at +0x0C, entries
    // at +0x28, 24-byte entries {key, form, next}, in use when `next` is set; same on NG).
    unsafe {
        let map: *const u8 = read(global as *const u8, 0);
        if map.is_null() || !mem::readable(map as usize, 0x30) {
            return None;
        }
        let capacity = read::<u32>(map, 0x0C) as usize;
        let entries: *const u8 = read(map, 0x28);
        if entries.is_null() || !mem::readable(entries as usize, capacity * 24) {
            return None;
        }
        (0..capacity)
            .map(|i| entries.add(i * 24))
            .find(|&e| read::<usize>(e, 16) != 0 && read::<*mut u8>(e, 0) == key.as_raw())
            .and_then(|e| Form::checked(read(e, 8)))
    }
}

/// The cell's `ExtraCellWaterType` water (CommonLibF4 `TESObjectCELL::extraList`, a
/// `BSTSmartPointer<ExtraDataList>` whose `BaseExtraList` is a singly linked list).
fn extra_water(cell: Form) -> *mut u8 {
    // SAFETY: TESObjectCELL::extraList.
    let list: *const u8 = unsafe { read(cell.as_ptr(), RE::TESObjectCELL::extraList) };
    if list.is_null() || !mem::readable(list as usize, RE::ExtraDataList::SIZE) {
        return null_mut();
    }
    // SAFETY: BaseExtraList::_head.
    let mut extra: *const u8 = unsafe { read(list, RE::ExtraDataList::extraData + RE::BaseExtraList::_head) };
    for _ in 0..256 {
        if extra.is_null() || !mem::readable(extra as usize, RE::ExtraCellWaterType::SIZE) {
            break;
        }
        // SAFETY: a readable BSExtraData (checked).
        unsafe {
            if read::<u8>(extra, RE::BSExtraData::r#type) == EXTRA_WATER_TYPE {
                return read(extra, RE::ExtraCellWaterType::water);
            }
            extra = read(extra, RE::BSExtraData::next);
        }
    }
    null_mut()
}

/// `EXTRA_DATA_TYPE::kWaterType` (CommonLibF4 `BSExtraData.h:30`).
const EXTRA_WATER_TYPE: u8 = 3;
/// `TESObjectCELL::Flag::kInterior`.
const CELL_INTERIOR: u16 = 1;
/// The cell's water as F4SE reports it (black-box, every cell of the session compared):
/// its `ExtraCellWaterType`, else the default water (form 0x18) for exteriors and None for
/// interiors. The engine's own `TESObjectCELL::GetWaterType` (our analysis of
/// Fallout4.exe, RVA 0x3b53d0) would give exteriors their world space's water and
/// interiors the default water instead.
fn cell_water(cell: Form) -> *mut u8 {
    let water = extra_water(cell);
    if !water.is_null() {
        return water;
    }
    // SAFETY: TESObjectCELL::cellFlags.
    let flags: u16 = unsafe { read(cell.as_ptr(), RE::TESObjectCELL::cellFlags) };
    if flags & CELL_INTERIOR != 0 {
        return null_mut();
    }
    let Some(global) = resolve(&addr::DEFAULT_WATER) else { return null_mut() };
    // SAFETY: the TESWaterForm* global.
    unsafe { read(global as *const u8, 0) }
}

const DEFAULT_OBJECT_CELL: &[Entry] = &[
    ("DefaultObject", "GetDefaultObject", |c| {
        let editor_id = c.string(0);
        let form = form_by_editor_id(&editor_id).map_or(null_mut(), Form::as_ptr);
        ret_form(c, form)
    }),
    ("DefaultObject", "get", |c| {
        let dfob = self_of(c, "DFOB")?;
        // SAFETY: BGSDefaultObject::form.
        let form: *mut u8 = unsafe { read(dfob.as_ptr(), RE::BGSDefaultObject::form) };
        ret_form(c, form)
    }),
    ("DefaultObject", "set", |c| {
        // F4SE (black-box) clears the form when given None, but left it unchanged for
        // every form tried: of the default object's own type, of another type, and the
        // original form itself (TRNS and SPEL default objects).
        let dfob = self_of(c, "DFOB")?;
        if arg_form(c, 0).is_none() {
            // SAFETY: BGSDefaultObject::form.
            unsafe { dfob.as_ptr().add(RE::BGSDefaultObject::form).cast::<*mut u8>().write_unaligned(null_mut()) };
        }
        Ok(())
    }),
    ("Cell", "GetWaterType", |c| {
        let cell = self_of(c, "CELL")?;
        ret_form(c, cell_water(cell))
    }),
];

// ---------------------------------------------------------------- Location, EncounterZone

fn location_field(c: &mut NativeCall, offset: usize) -> R {
    let location = self_of(c, "LCTN")?;
    // SAFETY: a pointer field of a live BGSLocation.
    let p: *mut u8 = unsafe { read(location.as_ptr(), offset) };
    ret_form(c, p)
}

fn set_location_field(c: &mut NativeCall, offset: usize) -> R {
    let location = self_of(c, "LCTN")?;
    let value = arg_form(c, 0).map_or(null_mut(), Form::as_ptr);
    // SAFETY: a pointer field of a live BGSLocation.
    unsafe { location.as_ptr().add(offset).cast::<*mut u8>().write_unaligned(value) };
    Ok(())
}

const ZONE_DATA: usize = RE::BGSEncounterZone::data;

/// A one-byte field of `ENCOUNTER_ZONE_DATA`, read signed (the rank) or unsigned (the
/// levels): F4SE returns 200 after `SetMinLevel(200)` and -128 after `SetRank(128)`
/// (black-box), though CommonLibF4 declares all three `std::int8_t`.
fn zone_byte(c: &mut NativeCall, field: usize, signed: bool) -> R {
    let zone = self_of(c, "ECZN")?;
    // SAFETY: a one-byte field of ENCOUNTER_ZONE_DATA.
    let v: u8 = unsafe { read(zone.as_ptr(), ZONE_DATA + field) };
    c.ret_int(if signed { i32::from(v as i8) } else { i32::from(v) });
    Ok(())
}

fn set_zone_byte(c: &mut NativeCall, field: usize) -> R {
    let zone = self_of(c, "ECZN")?;
    let v = c.int(0) as u8;
    // SAFETY: a one-byte field of ENCOUNTER_ZONE_DATA.
    unsafe { zone.as_ptr().add(ZONE_DATA + field).write(v) };
    Ok(())
}

const LOCATION_ZONE: &[Entry] = &[
    ("Location", "GetParent", |c| location_field(c, RE::BGSLocation::parentLoc)),
    ("Location", "SetParent", |c| set_location_field(c, RE::BGSLocation::parentLoc)),
    ("Location", "SetEncounterZone", |c| set_location_field(c, RE::BGSLocation::zone)),
    ("EncounterZone", "GetLocation", |c| {
        let zone = self_of(c, "ECZN")?;
        // SAFETY: ENCOUNTER_ZONE_DATA::location.
        let p: *mut u8 = unsafe { read(zone.as_ptr(), ZONE_DATA + RE::ENCOUNTER_ZONE_DATA::location) };
        ret_form(c, p)
    }),
    ("EncounterZone", "SetLocation", |c| {
        let zone = self_of(c, "ECZN")?;
        let value = arg_form(c, 0).map_or(null_mut(), Form::as_ptr);
        // SAFETY: ENCOUNTER_ZONE_DATA::location.
        unsafe { zone.as_ptr().add(ZONE_DATA + RE::ENCOUNTER_ZONE_DATA::location).cast::<*mut u8>().write_unaligned(value) };
        Ok(())
    }),
    ("EncounterZone", "GetMaxLevel", |c| zone_byte(c, RE::ENCOUNTER_ZONE_DATA::maxLevel, false)),
    ("EncounterZone", "SetMaxLevel", |c| set_zone_byte(c, RE::ENCOUNTER_ZONE_DATA::maxLevel)),
    ("EncounterZone", "GetMinLevel", |c| zone_byte(c, RE::ENCOUNTER_ZONE_DATA::minLevel, false)),
    ("EncounterZone", "SetMinLevel", |c| set_zone_byte(c, RE::ENCOUNTER_ZONE_DATA::minLevel)),
    ("EncounterZone", "GetRank", |c| zone_byte(c, RE::ENCOUNTER_ZONE_DATA::ownerRank, true)),
    ("EncounterZone", "SetRank", |c| set_zone_byte(c, RE::ENCOUNTER_ZONE_DATA::ownerRank)),
];

// ---------------------------------------------------------------- F4SE

/// `F4SE.GetScriptVersionRelease` (scripted in F4SE's `F4SE.psc`) returns what
/// `F4SE.GetVersionRelease` returns.
fn script_version_release(c: &mut NativeCall) -> R {
    match super::f4se::NATIVES.iter().find(|(s, n, _)| *s == "F4SE" && *n == "GetVersionRelease") {
        Some((_, _, handler)) => handler(c),
        None => Err("F4SE.GetVersionRelease is not implemented".into()),
    }
}

const F4SE: &[Entry] = &[("F4SE", "GetScriptVersionRelease", script_version_release)];

/// Every native of this family.
pub(crate) const NATIVES: &[Entry] = &concat_entries();

const fn concat_entries() -> [Entry; FORM.len() + GAME.len() + INPUT.len() + DEFAULT_OBJECT_CELL.len() + LOCATION_ZONE.len() + F4SE.len()] {
    let parts: [&[Entry]; 6] = [FORM, GAME, INPUT, DEFAULT_OBJECT_CELL, LOCATION_ZONE, F4SE];
    let mut out = [FORM[0]; FORM.len() + GAME.len() + INPUT.len() + DEFAULT_OBJECT_CELL.len() + LOCATION_ZONE.len() + F4SE.len()];
    let (mut i, mut p) = (0, 0);
    while p < parts.len() {
        let mut j = 0;
        while j < parts[p].len() {
            out[i] = parts[p][j];
            i += 1;
            j += 1;
        }
        p += 1;
    }
    out
}
