//! Placed-reference internals behind the `ObjectReference` and `FavoritesManager`
//! natives: extra data lists, inventory lists, inventory objects (items in a container
//! that scripts address as references), power links and the favorites manager.
//!
//! Layouts: CommonLibF4 (`BSExtraData.h`, `BGSInventoryItem.h`, `TESObjectREFRs.h`,
//! `FavoritesManager.h`, `BSTHashMap.h`, `BSLock.h`, `GameScript.h`) through the generated
//! tables. Engine functions: CommonLibF4 IDs, or our own analysis of `Fallout4.exe`
//! (`rva:` recipes; see `provenance/natives-references.jsonl`).

// These functions take engine pointers (refs, extra lists, inventory items) as the
// engine hands them out and check them (`mem::readable`, `Form::checked`) before use,
// like the rest of f4x-engine; they are safe to call with any pointer value.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::mem::transmute;
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};

use f4x_engine_sys::og_1_10_163_addresses as addr;

use crate::address::resolve;
use crate::form::Form;
use crate::form_id::FormId;
use crate::layout::RE;
use crate::mem::{self, BSTArray, read};
use crate::papyrus::{ScriptObject, Vm, object_handle, slot, vcall};
use crate::refr::Reference;

/// `EXTRA_DATA_TYPE` values (CommonLibF4 `BSExtraData.h`, enumerator positions; the
/// material swap value is also the constant the engine's own setter writes).
pub mod extra {
    pub const OBJECT_INSTANCE: u8 = 0x35;
    pub const TEXT_DISPLAY_DATA: u8 = 0x99;
    pub const UNIQUE_ID: u8 = 0x9F;
    pub const MATERIAL_SWAP: u8 = 0xB9;
    pub const WORKSHOP: u8 = 0xBF;
    pub const POWER_LINKS: u8 = 0xD0;
}

// ---------------------------------------------------------------- extra data lists

/// An `ExtraDataList` (`BSIntrusiveRefCounted` + `BaseExtraList` + `BSReadWriteLock`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtraList(NonNull<u8>);

// SAFETY: engine object with its own lock; only used through the engine's accessors.
unsafe impl Send for ExtraList {}

impl ExtraList {
    fn checked(p: *mut u8) -> Option<ExtraList> {
        (mem::readable(p as usize, RE::ExtraDataList::SIZE)).then(|| NonNull::new(p)).flatten().map(ExtraList)
    }

    /// The extra data list of a placed reference (`TESObjectREFR::extraList`).
    #[must_use]
    pub fn of(reference: Reference) -> Option<ExtraList> {
        // SAFETY: a live TESObjectREFR; the field is a BSTSmartPointer (one pointer).
        ExtraList::checked(unsafe { read(reference.form().as_ptr(), RE::TESObjectREFR::extraList) })
    }

    #[must_use]
    pub fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }

    /// The extra data of type `ty`, through the engine's `ExtraDataList::GetByType`
    /// (takes the list's read lock).
    #[must_use]
    pub fn get(self, ty: u8) -> Option<*mut u8> {
        let f = resolve(&addr::EXTRA_LIST_GET_BY_TYPE)?;
        // SAFETY: (ExtraDataList*, EXTRA_DATA_TYPE as u8) -> BSExtraData*.
        let p = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, u8) -> *mut u8>(f)(self.as_ptr(), ty) };
        (mem::readable(p as usize, RE::BSExtraData::SIZE)
            // SAFETY: readable; BSExtraData::type.
            && unsafe { read::<u8>(p, RE::BSExtraData::r#type) } == ty)
            .then_some(p)
    }

    /// Set or (with null) remove the material swap, through the engine's setter, which
    /// creates or deletes the `ExtraMaterialSwap` under the list's write lock.
    pub fn set_material_swap(self, swap: *mut u8) -> bool {
        let Some(f) = resolve(&addr::EXTRA_LIST_SET_MATERIAL_SWAP) else { return false };
        // SAFETY: (ExtraDataList*, BGSMaterialSwap*) -> void.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8)>(f)(self.as_ptr(), swap) };
        true
    }

    /// The material swap (`ExtraMaterialSwap::swap`).
    #[must_use]
    pub fn material_swap(self) -> Option<Form> {
        let x = self.get(extra::MATERIAL_SWAP)?;
        // SAFETY: an ExtraMaterialSwap.
        Form::checked(unsafe { read(x, RE::ExtraMaterialSwap::swap) })
    }

    /// The display name an `ExtraTextDisplayData` gives `base`, if the list has one
    /// (`ExtraTextDisplayData::GetDisplayName`).
    #[must_use]
    pub fn text_display_name(self, base: Option<Form>) -> Option<String> {
        let x = self.get(extra::TEXT_DISPLAY_DATA)?;
        let f = resolve(&addr::EXTRA_TEXT_DISPLAY_GET_NAME)?;
        // SAFETY: (ExtraTextDisplayData*, TESBoundObject*) -> const BSFixedStringCS&.
        let name = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8) -> *const *const u8>(f)(x, base.map_or(null_mut(), Form::as_ptr)) };
        if !mem::readable(name as usize, 8) {
            return None;
        }
        // SAFETY: a BSFixedStringCS (one pool entry pointer).
        unsafe { mem::fixed_string(*name) }
    }

    /// The object IDs of the mods in a `BGSObjectInstanceExtra` (CommonLibF4
    /// `GetIndexData`: block 0 of its `BSTDataBuffer<1>`, 8-byte `ObjectIndexData`).
    #[must_use]
    pub fn mod_ids(self) -> Option<Vec<u32>> {
        let x = self.get(extra::OBJECT_INSTANCE)?;
        // SAFETY: a BGSObjectInstanceExtra; `values` is a BSTDataBuffer<1>* (buffer, size).
        unsafe {
            let values: *const u8 = read(x, RE::BGSObjectInstanceExtra::values);
            if !mem::readable(values as usize, 0x10) {
                return Some(Vec::new());
            }
            let buffer: *const u8 = read(values, 0);
            let size: u32 = read(values, 8);
            if buffer.is_null() || !mem::readable(buffer as usize, size as usize + 4) {
                return Some(Vec::new());
            }
            // Blocks follow the data: {size:24, id:8}; N = 1, so only the first block counts.
            let block: u32 = read(buffer, size as usize);
            let (len, id) = (block & 0x00FF_FFFF, block >> 24);
            if id != 0 || len as usize > size as usize {
                return Some(Vec::new());
            }
            Some((0..len as usize / 8).map(|i| read::<u32>(buffer, i * 8)).collect())
        }
    }

    /// Form IDs of the power links (`ExtraPowerLinks::powerLinks`, 8-byte elements
    /// `{formID, linkType}`).
    #[must_use]
    pub fn power_links(self) -> Option<Vec<(u32, u32)>> {
        let x = self.get(extra::POWER_LINKS)?;
        // SAFETY: an ExtraPowerLinks; a BSTArray<Element>.
        let links = unsafe { BSTArray::<[u32; 2]>::at(x.add(RE::ExtraPowerLinks::powerLinks)) };
        Some(links.iter().map(|[id, kind]| (id, kind)).collect())
    }
}

// ---------------------------------------------------------------- locks

/// A read lock on a `BSReadWriteLock` (CommonLibF4 `BSLock.h`: `lock_read` through the
/// engine, `unlock_read` decrements the lock word at +4).
struct ReadGuard(*mut u8);

impl ReadGuard {
    fn new(lock: *mut u8) -> Option<ReadGuard> {
        let f = resolve(&addr::RW_LOCK_LOCK_READ)?;
        // SAFETY: BSReadWriteLock::lock_read(this).
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8)>(f)(lock) };
        Some(ReadGuard(lock))
    }
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        // SAFETY: the lock word of a live BSReadWriteLock (4-aligned, at +4).
        unsafe { AtomicU32::from_ptr(self.0.add(4).cast::<u32>()) }.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- forms by id

/// A form by id through the engine's `TESForm::GetFormByID` (hashed, under the global
/// form map's lock).
#[must_use]
pub fn lookup_form(id: u32) -> Option<Form> {
    if id == 0 {
        return None;
    }
    let f = resolve(&addr::FORM_LOOKUP_BY_ID)?;
    // SAFETY: (u32 formID) -> TESForm*.
    Form::checked(unsafe { transmute::<usize, unsafe extern "C" fn(u32) -> *mut u8>(f)(id) })
}

/// As [`lookup_form`], with a `FormId`.
#[must_use]
pub fn lookup(id: FormId) -> Option<Form> {
    lookup_form(id.0)
}

// ---------------------------------------------------------------- inventories

/// One `BGSInventoryItem`: the object and its first stack (`BGSInventoryItem::Stack*`).
#[derive(Clone, Copy, Debug)]
pub struct InventoryItem {
    pub object: *mut u8,
    pub stack: *mut u8,
}

/// The stacks of an item (`Stack::nextStack` chain).
pub fn stacks(item: InventoryItem) -> impl Iterator<Item = *mut u8> {
    let mut next = item.stack;
    let mut guard = 0;
    std::iter::from_fn(move || {
        if next.is_null() || guard > 4096 || !mem::readable(next as usize, RE::BGSInventoryItem::Stack::SIZE) {
            return None;
        }
        guard += 1;
        let current = next;
        // SAFETY: a live Stack (readable, checked).
        next = unsafe { read(current, RE::BGSInventoryItem::Stack::nextStack) };
        Some(current)
    })
}

/// A stack's extra data list.
#[must_use]
pub fn stack_extra(stack: *mut u8) -> Option<ExtraList> {
    // SAFETY: a live Stack; `extra` is a BSTSmartPointer<ExtraDataList>.
    ExtraList::checked(unsafe { read(stack, RE::BGSInventoryItem::Stack::extra) })
}

/// Run `f` over a reference's inventory items while holding its inventory list's read
/// lock. `None` if the reference has no inventory list.
pub fn with_inventory<R>(reference: Reference, f: impl FnOnce(&[InventoryItem]) -> R) -> Option<R> {
    // SAFETY: a live TESObjectREFR; BGSInventoryList* field.
    let list: *mut u8 = unsafe { read(reference.form().as_ptr(), RE::TESObjectREFR::inventoryList) };
    if !mem::readable(list as usize, RE::BGSInventoryList::SIZE) {
        return None;
    }
    // SAFETY: rwLock is embedded in the list.
    let _guard = ReadGuard::new(unsafe { list.add(RE::BGSInventoryList::rwLock) })?;
    // SAFETY: BSTArray<BGSInventoryItem> (16-byte items: object, stackData).
    let data = unsafe { BSTArray::<[usize; 2]>::at(list.add(RE::BGSInventoryList::data)) };
    let items: Vec<InventoryItem> = data.iter().map(|[o, s]| InventoryItem { object: o as *mut u8, stack: s as *mut u8 }).collect();
    Some(f(&items))
}

/// `TESObjectREFR::GetWeightInContainer`.
#[must_use]
pub fn weight_in_container(reference: Reference) -> Option<f32> {
    let f = resolve(&addr::REFR_GET_WEIGHT_IN_CONTAINER)?;
    // SAFETY: (TESObjectREFR*) -> float.
    Some(unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8) -> f32>(f)(reference.form().as_ptr()) })
}

// ---------------------------------------------------------------- inventory objects

/// What a script `ObjectReference` stands for: a placed reference, or an item in a
/// container addressed by its unique ID (CommonLibF4 `GameScript::RefrOrInventoryObj`).
#[derive(Clone, Copy, Debug, Default)]
pub struct RefOrInventory {
    pub reference: Option<Reference>,
    pub container: Option<Reference>,
    pub unique_id: u16,
}

fn as_ref(p: *mut u8) -> Option<Reference> {
    Form::checked(p)?.as_reference()
}

/// Resolve a script object's handle with the game's handle policy
/// (`HandlePolicy::GetInventoryObjFromHandle`).
#[must_use]
pub fn ref_or_inventory(vm: Vm, object: *mut ScriptObject) -> Option<RefOrInventory> {
    let handle = object_handle(object)?;
    if let Some(f) = resolve(&addr::HANDLE_POLICY_GET_INVENTORY_OBJ) {
        // SAFETY: IVirtualMachine::GetObjectHandlePolicy() -> the game's HandlePolicy.
        let policy = unsafe { vcall::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(vm.0, slot::GET_OBJECT_HANDLE_POLICY)(vm.0) };
        let (mut container, mut unique, mut reference): (*mut u8, u16, *mut u8) = (null_mut(), 0, null_mut());
        type F = unsafe extern "C" fn(*mut c_void, u64, *mut *mut u8, *mut u16, *mut *mut u8);
        // SAFETY: (this, handle, TESObjectREFR*& container, u16& uniqueID, TESObjectREFR*& ref).
        unsafe { transmute::<usize, F>(f)(policy, handle, &mut container, &mut unique, &mut reference) };
        let r = RefOrInventory { reference: as_ref(reference), container: as_ref(container), unique_id: unique };
        if r.reference.is_some() || r.container.is_some() {
            return Some(r);
        }
    }
    // A plain reference handle.
    let (_, p) = vm.game_object(object)?;
    Some(RefOrInventory { reference: as_ref(p), ..RefOrInventory::default() })
}

/// The base object and extra data list a `RefOrInventory` denotes (CommonLibF4
/// `Papyrus::GetReferenceData`): a placed reference's own, or the stack of the
/// container item whose `ExtraUniqueID` matches. `f` runs while the container's
/// inventory lock is held.
pub fn with_reference_data<R>(r: RefOrInventory, f: impl FnOnce(Option<Form>, Option<ExtraList>) -> R) -> Option<R> {
    if let Some(reference) = r.reference {
        return Some(f(reference.base_object(), ExtraList::of(reference)));
    }
    let (container, unique) = (r.container?, r.unique_id);
    if unique == 0 {
        return None;
    }
    with_inventory(container, |items| {
        for &item in items {
            for stack in stacks(item) {
                let Some(list) = stack_extra(stack) else { continue };
                let Some(x) = list.get(extra::UNIQUE_ID) else { continue };
                // SAFETY: an ExtraUniqueID (u16 uniqueID at +0x18).
                if unsafe { read::<u16>(x, 0x18) } == unique {
                    return Some(f(Form::checked(item.object), Some(list)));
                }
            }
        }
        None
    })
    .flatten()
}

// ---------------------------------------------------------------- 3D and connect points

/// A reference's loaded 3D root (`TESObjectREFR::Get3D() const`, vtable slot 0x8C).
#[must_use]
pub fn loaded_3d(reference: Reference) -> Option<*mut u8> {
    let p = reference.form().as_ptr();
    // SAFETY: a live TESObjectREFR (checked form); the slot from the generated layout.
    let root = unsafe { vcall::<unsafe extern "C" fn(*mut u8) -> *mut u8>(p.cast(), RE::TESObjectREFR::vtbl::Get3D_2)(p) };
    crate::game().is_instance_of(root as usize, ".?AVNiAVObject@@").then_some(root)
}

/// One `BSConnectPoint::Parents` entry (our analysis of `Parents::LoadBinary`: a
/// `BSTArray` of smart pointers at +0x18; each 0x38-byte point holds a refcount, the
/// parent node name at +0x08, its own name at +0x10, a rotation `NiQuaternion` (w, x,
/// y, z) at +0x18, a translation at +0x28 and a scale at +0x34).
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectPoint {
    pub parent: String,
    pub name: String,
    /// w, x, y, z.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
    pub scale: f32,
}

/// The connect points (`"CPA"` extra data) on a 3D root.
#[must_use]
pub fn connect_points(root: *mut u8) -> Vec<ConnectPoint> {
    let mut out = Vec::new();
    let (Some(f), Some(name)) = (resolve(&addr::NI_GET_EXTRA_DATA), crate::papyrus::FixedString::new("CPA")) else { return out };
    // SAFETY: NiObjectNET::GetExtraData(this, const BSFixedString&) -> NiExtraData*.
    let data = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *const crate::papyrus::FixedString) -> *mut u8>(f)(root, &name) };
    if !crate::game().is_instance_of(data as usize, ".?AVParents@BSConnectPoint@@") {
        return out;
    }
    // SAFETY: a BSConnectPoint::Parents; BSTArray<BSTSmartPointer<ConnectPoint>> at +0x18.
    let points = unsafe { BSTArray::<*mut u8>::at(data.add(0x18)) };
    for p in points.iter().take(1024) {
        if !mem::readable(p as usize, 0x38) {
            continue;
        }
        // SAFETY: a live ConnectPoint (readable, 0x38 bytes).
        unsafe {
            out.push(ConnectPoint {
                parent: mem::fixed_string(read(p, 0x08)).unwrap_or_default(),
                name: mem::fixed_string(read(p, 0x10)).unwrap_or_default(),
                rotation: read(p, 0x18),
                translation: read(p, 0x28),
                scale: read(p, 0x34),
            });
        }
    }
    out
}

// ---------------------------------------------------------------- workshops and power

/// `WorkshopItemKeyword` (Fallout4.esm `00054BA6`), checked by its editor ID
/// (`BGSKeyword::formEditorID`).
#[must_use]
pub fn workshop_item_keyword() -> Option<Form> {
    let kw = lookup_form(0x0005_4BA6)?;
    // SAFETY: a live form; checked to be a BGSKeyword before reading its editor ID.
    let edid = crate::game()
        .is_instance_of(kw.as_ptr() as usize, RE::BGSKeyword::RTTI)
        .then(|| unsafe { mem::fixed_string(read(kw.as_ptr(), RE::BGSKeyword::formEditorID)) })
        .flatten()?;
    edid.eq_ignore_ascii_case("WorkshopItemKeyword").then_some(kw)
}

/// `TESObjectREFR::GetLinkedRef(keyword)`.
#[must_use]
pub fn linked_ref(reference: Reference, keyword: Form) -> Option<Reference> {
    // The engine reads the reference's extra data list unchecked.
    ExtraList::of(reference)?;
    let f = resolve(&addr::REFR_GET_LINKED_REF)?;
    // SAFETY: (TESObjectREFR*, BGSKeyword*) -> TESObjectREFR*.
    let p = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8) -> *mut u8>(f)(reference.form().as_ptr(), keyword.as_ptr()) };
    Form::checked(p)?.as_reference()
}

/// `TESObjectREFR::SetLinkedRef(target, keyword)`.
pub fn set_linked_ref(reference: Reference, target: Reference, keyword: Form) -> bool {
    let Some(f) = resolve(&addr::REFR_SET_LINKED_REF) else { return false };
    if ExtraList::of(reference).is_none() {
        return false;
    }
    // SAFETY: (TESObjectREFR*, TESObjectREFR*, BGSKeyword*) -> void.
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8, *mut u8)>(f)(reference.form().as_ptr(), target.form().as_ptr(), keyword.as_ptr()) };
    true
}

/// The workshop an item belongs to (its `WorkshopItemKeyword` linked ref) and that
/// workshop's `Workshop::ExtraData`.
#[must_use]
pub fn workshop_of(item: Reference) -> Option<(Reference, *mut u8)> {
    let workshop = linked_ref(item, workshop_item_keyword()?)?;
    let data = ExtraList::of(workshop)?.get(extra::WORKSHOP)?;
    Some((workshop, data))
}

/// The engine's power update for an item on a workshop grid (what workshop placement
/// runs for both ends of a new wire): a power radiator connects to the items in range,
/// and the item is updated as a receiver.
pub fn transmit_power(item: Reference, workshop_data: *mut u8) -> bool {
    let Some(f) = resolve(&addr::WORKSHOP_TRANSMIT_POWER) else { return false };
    // SAFETY: (TESObjectREFR*, Workshop::ExtraData*) -> void.
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8)>(f)(item.form().as_ptr(), workshop_data) };
    true
}

/// The workshop whose area holds `item`, searched over the loaded cells: the engine's
/// own lookup (what it runs for the player when workshop mode starts). None when the
/// reference has no parent cell or no loaded workshop covers it.
#[must_use]
pub fn find_workshop(item: Reference) -> Option<Reference> {
    let f = resolve(&addr::WORKSHOP_FIND_FOR_REF)?;
    item.parent_cell()?;
    // SAFETY: (TESObjectREFR*) -> TESObjectREFR*; a live reference with a parent cell.
    let p = unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8) -> *mut u8>(f)(item.form().as_ptr()) };
    Form::checked(p)?.as_reference()
}

/// `TESObjectREFR::GetHandle` (MSVC: the handle is returned through a hidden pointer).
fn reference_handle(reference: Reference) -> Option<u32> {
    let f = resolve(&addr::REFR_GET_HANDLE)?;
    let mut handle: u32 = 0;
    // SAFETY: (TESObjectREFR*, ObjectRefHandle* out) -> ObjectRefHandle*.
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u32) -> *mut u32>(f)(reference.form().as_ptr(), &mut handle) };
    (handle != 0).then_some(handle)
}

/// Scrap `item` into `workshop` with the engine's workshop-mode scrap
/// (`Workshop::ScrapReference`), which credits the current workshop: the current-workshop
/// handle is pointed at `workshop` for the call and restored after it. The engine
/// disables the item (and deletes it when it comes from a plugin), moves its inventory and
/// adds its scrap components to the workshop's container, unlinks it from its workshop,
/// updates power and sends the workshop's item-destroyed event. Main thread only.
pub fn scrap(item: Reference, workshop: Reference) -> bool {
    let (Some(f), Some(global)) = (resolve(&addr::WORKSHOP_SCRAP_REFERENCE), resolve(&addr::WORKSHOP_CURRENT_HANDLE)) else { return false };
    // The engine reads both references' extra data lists unchecked.
    if ExtraList::of(item).is_none() || ExtraList::of(workshop).is_none() {
        return false;
    }
    let Some(handle) = reference_handle(workshop) else { return false };
    // Workshop::ContextData (CommonLibF4 `Workshop.h`, 0x30 bytes): look position and
    // direction, z angle, parent cell, worldspace.
    #[repr(C)]
    struct ContextData {
        look_pos: [f32; 3],
        look_dir: [f32; 3],
        z_angle: f32,
        parent_cell: *mut u8,
        world_space: *mut u8,
    }
    const _: () = assert!(std::mem::size_of::<ContextData>() == 0x30);
    let pos = item.position();
    let cell = item.parent_cell();
    let world = cell.map_or(null_mut(), |c| {
        // SAFETY: a live TESObjectCELL; cellFlags bit 0 = interior; the worldSpace union
        // member at +0xC8 (see `create_reference`).
        unsafe {
            let flags: u16 = read(c.as_ptr(), RE::TESObjectCELL::cellFlags);
            if flags & 1 != 0 { null_mut() } else { read(c.as_ptr(), RE::TESObjectCELL::spinLock + 8) }
        }
    });
    let context = ContextData { look_pos: [pos.x, pos.y, pos.z], look_dir: [0.0; 3], z_angle: 0.0, parent_cell: cell.map_or(null_mut(), Form::as_ptr), world_space: world };
    let p = item.form().as_ptr();
    // The smart pointer the engine takes (NiPointer<TESObjectREFR>); keep the item alive
    // for the call (NiRefObject count of the BSHandleRefObject base at +0x28).
    // SAFETY: the refcount word of a live reference (4-aligned).
    let refcount = || unsafe { AtomicU32::from_ptr(p.add(0x28).cast::<u32>()) };
    refcount().fetch_add(1, Ordering::SeqCst);
    let smart: *mut u8 = p;
    // SAFETY: the current-workshop handle global (a u32 inside the mapped image).
    let current = unsafe { AtomicU32::from_ptr(global as *mut u32) };
    let previous = current.swap(handle, Ordering::SeqCst);
    // SAFETY: (const ContextData&, NiPointer<TESObjectREFR>&, rewards array or null).
    unsafe { transmute::<usize, unsafe extern "C" fn(*const ContextData, *const *mut u8, *mut u8)>(f)(&context, &smart, null_mut()) };
    current.store(previous, Ordering::SeqCst);
    refcount().fetch_sub(1, Ordering::SeqCst);
    true
}

/// Wire connection kinds: the `linkType` of a power link is the kind of connect point
/// the wire uses on that end (our analysis of the engine's connect-point matcher at
/// 0x2001f0: `P-WS-Snap` 0, `P-WS-Input` 1, `P-WS-Output` 2).
#[must_use]
pub fn wire_link_type(point_name: &str) -> Option<i32> {
    match point_name.to_ascii_lowercase().as_str() {
        "p-ws-snap" => Some(0),
        "p-ws-input" => Some(1),
        "p-ws-output" => Some(2),
        _ => None,
    }
}

/// A placed reference from a handle (`BSPointerHandleManagerInterface::GetSmartPointer`);
/// the smart pointer's reference is dropped again (the reference stays alive in its cell).
fn reference_from_handle(handle: u32) -> Option<Reference> {
    let f = resolve(&addr::REFR_HANDLE_GET_SMART_POINTER)?;
    let mut out: *mut u8 = null_mut();
    // SAFETY: (const ObjectRefHandle&, NiPointer<TESObjectREFR>&) -> bool.
    unsafe { transmute::<usize, unsafe extern "C" fn(*const u32, *mut *mut u8) -> bool>(f)(&handle, &mut out) };
    let r = Form::checked(out)?.as_reference();
    if !out.is_null() {
        // SAFETY: NiRefObject refcount of the BSHandleRefObject base (+0x20, count at +8),
        // as the engine releases these pointers (low 10 bits are the count).
        unsafe { AtomicU32::from_ptr(out.add(0x28).cast::<u32>()) }.fetch_sub(1, Ordering::SeqCst);
    }
    r
}

/// Create a reference to `base` at `at`'s position in its cell or worldspace
/// (`TESDataHandler::CreateReferenceAtLocation` with a `NEW_REFR_DATA`; CommonLibF4
/// community `TESDataHandler.h` layout: 0x70 bytes, vtable at 0).
#[must_use]
pub fn create_reference(base: Form, at: Reference) -> Option<Reference> {
    let f = resolve(&addr::DATA_HANDLER_CREATE_REF_AT_LOCATION)?;
    let handler = crate::data::DataHandler::get()?;
    let vtable = crate::game().vtable(".?AUNEW_REFR_DATA@@")?;
    let cell = at.parent_cell()?;
    // SAFETY: a live TESObjectCELL; cellFlags bit 0 = kInterior; the `worldSpace` union
    // member sits at +0xC8, between `spinLock` (+0xC0) and `loadedData` (+0xD0)
    // (CommonLibF4 `TESForms.h` TESObjectCELL; not in the generated tables).
    let (interior, world) = unsafe {
        let flags: u16 = read(cell.as_ptr(), RE::TESObjectCELL::cellFlags);
        let world: *mut u8 = read(cell.as_ptr(), RE::TESObjectCELL::spinLock + 8);
        if flags & 1 != 0 { (cell.as_ptr(), null_mut()) } else { (null_mut(), world) }
    };
    let pos = at.position();
    #[repr(C, align(16))]
    struct NewRefrData([u8; 0x70]);
    let mut data = NewRefrData([0; 0x70]);
    // SAFETY: filling our own NEW_REFR_DATA buffer.
    unsafe {
        let d = data.0.as_mut_ptr();
        d.cast::<usize>().write_unaligned(vtable);
        d.add(0x08).cast::<[f32; 3]>().write_unaligned([pos.x, pos.y, pos.z]);
        d.add(0x20).cast::<*mut u8>().write_unaligned(base.as_ptr());
        d.add(0x28).cast::<*mut u8>().write_unaligned(interior);
        d.add(0x30).cast::<*mut u8>().write_unaligned(world);
        *d.add(0x6C) = 1; // initializeScripts
    }
    let mut handle: u32 = 0;
    // SAFETY: TESDataHandler::CreateReferenceAtLocation(this, ObjectRefHandle* out, NEW_REFR_DATA&).
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u32, *mut u8) -> *mut u32>(f)(handler.as_ptr(), &mut handle, data.0.as_mut_ptr()) };
    if handle == 0 {
        return None;
    }
    reference_from_handle(handle)
}

/// Connect two items with a wire reference, as workshop placement does: position the
/// spline between the endpoints' connect points, add the power links on all three
/// (`SplineUtils::ConnectSpline`), then join the workshop's power grids and update
/// power on both ends.
pub fn connect_wire(end1: Reference, link1: i32, end2: Reference, link2: i32, wire: Reference, workshop_data: Option<*mut u8>) -> bool {
    let (Some(geometry), Some(connect)) = (resolve(&addr::SPLINE_UPDATE_GEOMETRY), resolve(&addr::SPLINE_CONNECT)) else { return false };
    // Both engine functions use each reference's extra data list unchecked.
    if [end1, end2, wire].iter().any(|r| ExtraList::of(*r).is_none()) {
        return false;
    }
    let (a, b, w) = (end1.form().as_ptr(), end2.form().as_ptr(), wire.form().as_ptr());
    // SAFETY: (ContextData* | null, wire, end2, link2, end1, link1) -> bool; then
    // ConnectSpline(end1, link1, end2, link2, wire).
    unsafe {
        transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8, *mut u8, i32, *mut u8, i32) -> bool>(geometry)(null_mut(), w, b, link2, a, link1);
        transmute::<usize, unsafe extern "C" fn(*mut u8, i32, *mut u8, i32, *mut u8)>(connect)(a, link1, b, link2, w);
    }
    if let Some(wx) = workshop_data {
        if let (Some(add), Some(join)) = (resolve(&addr::WORKSHOP_GRID_ADD_ITEM), resolve(&addr::WORKSHOP_GRID_CONNECT)) {
            // SAFETY: (Workshop::ExtraData*, item) and (Workshop::ExtraData*, end1, end2, wire).
            unsafe {
                transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8)>(add)(wx, w);
                transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8, *mut u8, *mut u8)>(join)(wx, a, b, w);
            }
        }
        transmit_power(end2, wx);
        transmit_power(end1, wx);
    }
    if let Some(update) = resolve(&addr::WORKSHOP_UPDATE_ITEM) {
        // SAFETY: (TESObjectREFR*) -> void.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8)>(update)(w) };
    }
    true
}

// ---------------------------------------------------------------- favorites

/// The `FavoritesManager` singleton (its global, CommonLibF4 `REL::ID(198281)`; the
/// object lives in static storage, which the RTTI global scan does not look at).
#[must_use]
pub fn favorites_manager() -> Option<*mut u8> {
    let global = resolve(&addr::FAVORITES_MANAGER_SINGLETON)?;
    // SAFETY: the singleton global (a FavoritesManager*), inside the mapped image.
    let p: *mut u8 = unsafe { read(global as *const u8, 0) };
    crate::game().is_instance_of(p as usize, RE::FavoritesManager::RTTI).then_some(p)
}

pub const FAVORITE_SLOTS: usize = 12;

/// `FavoritesManager::storedFavTypes[12]`.
#[must_use]
pub fn favorites(manager: *mut u8) -> [*mut u8; FAVORITE_SLOTS] {
    // SAFETY: a live FavoritesManager; an array of 12 pointers.
    unsafe { read(manager, RE::FavoritesManager::storedFavTypes) }
}

/// Overwrite `storedFavTypes`.
pub fn set_favorites(manager: *mut u8, slots: [*mut u8; FAVORITE_SLOTS]) {
    // SAFETY: a live FavoritesManager; an array of 12 pointers.
    unsafe { manager.add(RE::FavoritesManager::storedFavTypes).cast::<[*mut u8; FAVORITE_SLOTS]>().write_unaligned(slots) };
}

/// The keys of `favoritedComponents` (a `BSTHashMap<const TESBoundObject*, u32>`;
/// CommonLibF4 `BSTScatterTable`: capacity at +0x0C, entries at +0x28, 0x18-byte
/// entries `{key, value, next}` in use when `next` is set), in table order.
#[must_use]
pub fn tagged_components(manager: *mut u8) -> Vec<*mut u8> {
    let map = RE::FavoritesManager::favoritedComponents;
    let mut out = Vec::new();
    // SAFETY: a live FavoritesManager; the map's fields.
    unsafe {
        let capacity: u32 = read(manager, map + 0x0C);
        let entries: *const u8 = read(manager, map + 0x28);
        if entries.is_null() || capacity > 1 << 20 || !mem::readable(entries as usize, capacity as usize * 0x18) {
            return out;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * 0x18);
            if read::<usize>(entry, 0x10) != 0 {
                out.push(read::<*mut u8>(entry, 0));
            }
        }
    }
    out
}

/// `FavoritesManager::IsComponentFavorite`.
#[must_use]
pub fn is_tagged(manager: *mut u8, form: *mut u8) -> bool {
    if form.is_null() {
        return false;
    }
    match resolve(&addr::FAVORITES_IS_COMPONENT_FAVORITE) {
        // SAFETY: (FavoritesManager*, const TESBoundObject*) -> bool.
        Some(f) => unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8) -> bool>(f)(manager, form) },
        None => tagged_components(manager).contains(&form),
    }
}

/// The engine's tag toggle (Pip-Boy "tag for search"): inserts the component with
/// value 1 or erases it, then notifies `ComponentFavoriteEvent` sinks.
pub fn toggle_tagged(manager: *mut u8, form: *mut u8) -> bool {
    let Some(f) = resolve(&addr::FAVORITES_TOGGLE_COMPONENT) else { return false };
    if form.is_null() {
        return false;
    }
    // SAFETY: (FavoritesManager*, const TESBoundObject*) -> void.
    unsafe { transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8)>(f)(manager, form) };
    true
}
