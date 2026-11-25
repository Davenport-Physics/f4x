//! Material swaps on loaded 3D: the shader properties of a reference's scene graph and
//! the engine's own material-swap application (what the game runs when it loads a
//! reference's 3D with a `BGSMaterialSwap`).
//!
//! Layouts: `NiObjectNET::name`, `NiNode::children` (a `NiTArray`), `BSShaderProperty::
//! material` and `BGSMaterialSwap::swapMap` come from CommonLibF4 through the generated
//! tables; `BSGeometry`'s shader property slot and the engine functions come from our own
//! analysis of `Fallout4.exe` (see `provenance/natives-references.jsonl`).

// These functions take engine pointers (3D roots, forms) as the engine hands them out and
// check them (`mem::readable`, RTTI) before use; they are safe to call with any pointer.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::form::Form;
use crate::layout::RE;
use crate::mem::{self, read};

/// `BSGeometry`'s shader property (`NiPointer<NiProperty>`; the engine's material-swap
/// visitor reads it at +0x138, after the alpha property at +0x130). Same on NG 1.11.240
/// (clf4-ng `BSGeometry.h` `properties[2]` at 0x130; re: worker 0x256390 reads +0x138).
const GEOMETRY_SHADER_PROPERTY: usize = 0x138;
/// `NiTArray` fields relative to the array: data pointer at +8, the first free index (the
/// bound the engine's visitors iterate to) at +0x12. Same on NG (clf4-ng `NiTArray.h`:181;
/// re: visitor 0x256250 reads children +0x132).
const NI_ARRAY_DATA: usize = 0x08;
const NI_ARRAY_FREE_INDEX: usize = 0x12;

/// Depth and node limits for walking a scene graph (a guard against corrupt pointers).
const MAX_DEPTH: usize = 64;
const MAX_OBJECTS: usize = 1 << 14;

/// The kind of shader property, as the engine's material-swap visitor distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderKind {
    Lighting,
    Effect,
    Other,
}

/// One geometry of a scene graph and its shader property.
#[derive(Clone, Debug)]
pub struct ShaderSlot {
    pub geometry: *mut u8,
    pub geometry_name: String,
    pub property: *mut u8,
    pub kind: ShaderKind,
    /// The property's name (`NiObjectNET::name`): the material file it was loaded from.
    pub name: String,
    /// `BSShaderProperty::material`.
    pub material: *mut u8,
}

fn object_name(p: *mut u8) -> String {
    // SAFETY: a live NiObjectNET (checked by the caller); the name is a BSFixedString.
    unsafe { mem::fixed_string(read(p, RE::NiObjectNET::name)) }.unwrap_or_default()
}

/// The geometries under `root` (depth first, in child order) and their shader properties.
#[must_use]
pub fn shader_slots(root: *mut u8) -> Vec<ShaderSlot> {
    let game = crate::game();
    let mut out = Vec::new();
    let mut stack = vec![(root, 0usize)];
    let mut seen = 0usize;
    while let Some((object, depth)) = stack.pop() {
        seen += 1;
        if seen > MAX_OBJECTS || !game.is_instance_of(object as usize, ".?AVNiAVObject@@") {
            continue;
        }
        if game.is_instance_of(object as usize, ".?AVBSGeometry@@") {
            // SAFETY: a live BSGeometry (RTTI checked).
            let property: *mut u8 = unsafe { read(object, GEOMETRY_SHADER_PROPERTY) };
            if !game.is_instance_of(property as usize, ".?AVBSShaderProperty@@") {
                continue;
            }
            let kind = if game.is_instance_of(property as usize, ".?AVBSLightingShaderProperty@@") {
                ShaderKind::Lighting
            } else if game.is_instance_of(property as usize, ".?AVBSEffectShaderProperty@@") {
                ShaderKind::Effect
            } else {
                ShaderKind::Other
            };
            out.push(ShaderSlot {
                geometry: object,
                geometry_name: object_name(object),
                property,
                kind,
                name: object_name(property),
                // SAFETY: a live BSShaderProperty (RTTI checked).
                material: unsafe { read(property, RE::BSShaderProperty::material) },
            });
            continue;
        }
        if depth >= MAX_DEPTH || !game.is_instance_of(object as usize, ".?AVNiNode@@") {
            continue;
        }
        let children = object.wrapping_add(RE::NiNode::children);
        // SAFETY: a live NiNode (RTTI checked); its children NiTArray.
        let (data, count): (*const *mut u8, u16) = unsafe { (read(children, NI_ARRAY_DATA), read(children, NI_ARRAY_FREE_INDEX)) };
        if count == 0 || !mem::readable(data as usize, usize::from(count) * 8) {
            continue;
        }
        // Push in reverse so children pop in order.
        for i in (0..usize::from(count)).rev() {
            // SAFETY: within the readable children array.
            let child = unsafe { data.add(i).read_unaligned() };
            if !child.is_null() {
                stack.push((child, depth + 1));
            }
        }
    }
    out
}

/// One `BGSMaterialSwap` entry: source material, target material, color remapping index.
#[derive(Clone, Debug, PartialEq)]
pub struct SwapEntry {
    pub source: String,
    pub target: String,
    pub color_index: f32,
}

/// `BSTScatterTable` fields (CommonLibF4 `BSTHashMap.h`): capacity (u32 at +0x0C),
/// entries (+0x28); a `swapMap` entry is {key, Entry {swapMaterial, colorRemappingIndex},
/// next} (0x20 bytes) and is in use when `next` is set. Same on NG (clf4-ng `BSTHashMap.h`:577).
const TABLE_CAPACITY: usize = 0x0C;
const TABLE_ENTRIES: usize = 0x28;
const SWAP_ENTRY_SIZE: usize = 0x20;
const SWAP_ENTRY_VALUE: usize = 0x08;
const SWAP_ENTRY_NEXT: usize = 0x18;

/// The entries of a material swap form, in table order.
#[must_use]
pub fn swap_entries(swap: Form) -> Vec<SwapEntry> {
    let mut out = Vec::new();
    if !crate::game().is_instance_of(swap.as_ptr() as usize, RE::BGSMaterialSwap::RTTI) {
        return out;
    }
    let map = swap.as_ptr().wrapping_add(RE::BGSMaterialSwap::swapMap);
    // SAFETY: a live BGSMaterialSwap; its scatter table's fields.
    let (capacity, entries): (u32, *const u8) = unsafe { (read(map, TABLE_CAPACITY), read(map, TABLE_ENTRIES)) };
    let capacity = capacity as usize;
    if entries.is_null() || capacity > 1 << 20 || !mem::readable(entries as usize, capacity * SWAP_ENTRY_SIZE) {
        return out;
    }
    for i in 0..capacity {
        let e = entries.wrapping_add(i * SWAP_ENTRY_SIZE);
        // SAFETY: an entry within the readable table; used entries own their strings.
        unsafe {
            if read::<usize>(e, SWAP_ENTRY_NEXT) == 0 {
                continue;
            }
            let value = e.add(SWAP_ENTRY_VALUE);
            out.push(SwapEntry {
                source: mem::fixed_string(read(e, 0)).unwrap_or_default(),
                target: mem::fixed_string(read(value, RE::BGSMaterialSwap::Entry::swapMaterial)).unwrap_or_default(),
                color_index: read(value, RE::BGSMaterialSwap::Entry::colorRemappingIndex),
            });
        }
    }
    out
}

/// The key the engine looks a shader property's material up by: its name without a
/// leading `data\materials\` or `materials\` (case-insensitive, first occurrence).
#[must_use]
pub fn material_key(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for prefix in ["data\\materials\\", "materials\\"] {
        if let Some(at) = lower.find(prefix) {
            return &name[at + prefix.len()..];
        }
    }
    name
}

// ---------------------------------------------------------------- applying a swap

/// "No color remapping index" (the engine's sentinel, `FLT_MAX`).
pub const NO_COLOR: f32 = f32::MAX;

/// The lighting material's color remapping index (`BSLightingShaderMaterialBase` +0xB8:
/// where the engine's swap worker stores it when the property has the grayscale-to-palette
/// flag, `BSShaderProperty::flags` bit 4). Same on NG (re: worker 0x256390 writes +0xB8).
const LIGHTING_COLOR_INDEX: usize = 0xB8;

/// One geometry a swap would change: what it uses now and what the swap maps it to.
#[derive(Clone, Debug)]
pub struct SwapMatch {
    pub slot: ShaderSlot,
    /// The property's material key (its name without the materials folder).
    pub key: String,
    /// The swap's target material for that key.
    pub target: String,
    /// The current material's color remapping index (lighting properties; `NO_COLOR`
    /// for others).
    pub color_index: f32,
}

/// The geometries under `root` that `swap` changes, in scene-graph order, with the
/// engine's matching rules (its per-geometry worker): lighting and effect properties
/// only, the key looked up case-insensitively, skipped when the target is empty or
/// the key itself.
#[must_use]
pub fn swap_matches(root: *mut u8, swap: Form) -> Vec<SwapMatch> {
    let entries = swap_entries(swap);
    if entries.is_empty() {
        return Vec::new();
    }
    shader_slots(root)
        .into_iter()
        .filter(|s| s.kind != ShaderKind::Other)
        .filter_map(|slot| {
            let key = material_key(&slot.name).to_owned();
            let entry = entries.iter().find(|e| e.source.eq_ignore_ascii_case(&key))?;
            if entry.target.is_empty() || entry.target.eq_ignore_ascii_case(&key) {
                return None;
            }
            let color_index = if slot.kind == ShaderKind::Lighting && mem::readable(slot.material as usize + LIGHTING_COLOR_INDEX, 4) {
                // SAFETY: readable; a float of the live lighting material.
                unsafe { read(slot.material, LIGHTING_COLOR_INDEX) }
            } else {
                NO_COLOR
            };
            Some(SwapMatch { target: entry.target.clone(), key, color_index, slot })
        })
        .collect()
}

/// Apply `swap` to one geometry through the engine's material-swap application (as the
/// game does when it loads a reference's 3D): the target material is loaded and set on
/// the geometry's shader property. A lighting property also takes the entry's color
/// remapping index; an effect property keeps its own (`colorOverride` above `FLT_MAX`
/// makes the engine skip the color write).
pub fn apply_to_geometry(geometry: *mut u8, kind: ShaderKind, swap: Form) -> bool {
    let Some(f) = crate::address::resolve(&f4x_engine_sys::addresses::MATSWAP_APPLY_3D) else { return false };
    let game = crate::game();
    if !game.is_instance_of(geometry as usize, ".?AVBSGeometry@@") || !game.is_instance_of(swap.as_ptr() as usize, RE::BGSMaterialSwap::RTTI) {
        return false;
    }
    let color_override = if kind == ShaderKind::Effect { f32::INFINITY } else { NO_COLOR };
    type Apply = unsafe extern "C" fn(*mut u8, *mut u8, f32, f32, *mut u8);
    // SAFETY: (NiAVObject* root, BGSMaterialSwap*, float colorIndex, float colorOverride,
    // texture queue or null); a live geometry and swap form (RTTI checked).
    unsafe { std::mem::transmute::<usize, Apply>(f)(geometry, swap.as_ptr(), NO_COLOR, color_override, std::ptr::null_mut()) };
    true
}

/// Rename a shader property (`NiObjectNET::name`), as `NiObjectNET::SetName` does: the
/// new pool entry replaces the old one, which is released.
pub fn set_property_name(property: *mut u8, name: &str) -> bool {
    if !crate::game().is_instance_of(property as usize, ".?AVBSShaderProperty@@") {
        return false;
    }
    let Some(entry) = crate::papyrus::FixedString::new(name) else { return false };
    let slot = property.wrapping_add(RE::NiObjectNET::name).cast::<*mut u8>();
    // SAFETY: the name field of a live property; it owns one reference to its entry.
    unsafe {
        crate::papyrus::release_entry(slot);
        slot.write(entry.into_raw());
    }
    true
}
