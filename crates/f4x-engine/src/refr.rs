//! `TESObjectREFR`: placed references (and the player).

use crate::form::Form;
use crate::layout::RE;
use crate::mem::{BSTArray, read};

/// A placed reference.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Reference(pub(crate) Form);

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Reference {
    pub fn form(self) -> Form {
        self.0
    }

    fn data(self, field: usize) -> *const u8 {
        // SAFETY: OBJ_REFR is embedded at TESObjectREFR::data.
        unsafe { self.0.as_ptr().add(RE::TESObjectREFR::data + field) }
    }

    pub fn position(self) -> Point3 {
        // SAFETY: OBJ_REFR::location is a NiPoint3A (x, y, z floats).
        unsafe { read(self.data(RE::OBJ_REFR::location), 0) }
    }

    /// Rotation in radians (x, y, z).
    pub fn rotation(self) -> Point3 {
        // SAFETY: OBJ_REFR::angle is a NiPoint3A.
        unsafe { read(self.data(RE::OBJ_REFR::angle), 0) }
    }

    /// The base object this reference places.
    pub fn base_object(self) -> Option<Form> {
        // SAFETY: OBJ_REFR::objectReference is a TESBoundObject*.
        unsafe { Form::from_ptr(read(self.data(RE::OBJ_REFR::objectReference), 0)) }
    }

    /// The cell it is in.
    pub fn parent_cell(self) -> Option<Form> {
        // SAFETY: TESObjectREFR::parentCell is a TESObjectCELL*.
        unsafe { Form::from_ptr(read(self.0.as_ptr(), RE::TESObjectREFR::parentCell)) }
    }
}

// Read NiPoint3A as Point3 (the trailing pad of NiPoint3A is ignored).
const _: () = assert!(std::mem::size_of::<Point3>() == 12);

/// The player (`PlayerCharacter` singleton, found by RTTI).
pub fn player() -> Option<Reference> {
    let ptr = crate::game().singleton(RE::PlayerCharacter::RTTI)?;
    // SAFETY: the engine's PlayerCharacter object.
    unsafe { Form::from_ptr(ptr) }.map(Reference)
}

/// The references in a cell (`TESObjectCELL::references`, `BSTArray<NiPointer<TESObjectREFR>>`).
pub fn cell_references(cell: Form) -> Vec<Reference> {
    // SAFETY: a live TESObjectCELL; NiPointer<T> is a single T*.
    let array = unsafe { BSTArray::<*mut u8>::at(cell.as_ptr().add(RE::TESObjectCELL::references)) };
    array.iter().filter_map(|p| unsafe { Form::from_ptr(p) }).map(Reference).collect()
}
