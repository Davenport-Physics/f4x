//! `TESForm`.

use std::ptr::NonNull;

use crate::form_id::FormId;
use crate::form_type::FormType;
use crate::layout::RE;
use crate::mem::{self, read};
use crate::refr::Reference;

/// A live engine form. Valid while the engine keeps the form alive (forms loaded from
/// plugins live for the session; created forms can be deleted).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Form(NonNull<u8>);

impl Form {
    /// # Safety
    /// `ptr` must point to a live `TESForm` (or a class deriving from it at offset 0).
    pub unsafe fn from_ptr(ptr: *mut u8) -> Option<Form> {
        NonNull::new(ptr).map(Form)
    }

    /// A form from a pointer read out of an engine field that may be stale or not yet
    /// initialised: checked to be a readable object whose RTTI says `TESForm`.
    pub fn checked(ptr: *mut u8) -> Option<Form> {
        if crate::game().is_instance_of(ptr as usize, ".?AVTESForm@@") { NonNull::new(ptr).map(Form) } else { None }
    }

    pub fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }

    pub fn id(self) -> FormId {
        // SAFETY: TESForm::formID (generated layout).
        FormId(unsafe { read(self.as_ptr(), RE::TESForm::formID) })
    }

    pub fn form_type(self) -> FormType {
        // SAFETY: TESForm::formType is a u8 enumeration.
        FormType(unsafe { read(self.as_ptr(), RE::TESForm::formType) })
    }

    pub fn flags(self) -> u32 {
        // SAFETY: TESForm::formFlags.
        unsafe { read(self.as_ptr(), RE::TESForm::formFlags) }
    }

    /// The deleted flag (bit 5, CommonLibF4 `TESForm::IsDeleted`).
    pub fn is_deleted(self) -> bool {
        self.flags() & (1 << 5) != 0
    }

    /// Editor ID through the `GetFormEditorID` virtual. Release builds keep editor IDs
    /// for some form types only; the rest return "".
    pub fn editor_id(self) -> String {
        type GetEditorId = unsafe extern "C" fn(*const u8) -> *const u8;
        // SAFETY: vtable slot from the generated layout; a const member returning char*.
        unsafe {
            let vtable: *const usize = read(self.as_ptr(), 0);
            let f: GetEditorId = std::mem::transmute(*vtable.add(RE::TESForm::vtbl::GetFormEditorID));
            mem::c_str(f(self.as_ptr()))
        }
    }

    /// The display name, if the form has a `TESFullName` component.
    pub fn full_name(self) -> Option<String> {
        let game = crate::game();
        // SAFETY: a live polymorphic form.
        let component = unsafe { game.cast(self.as_ptr(), ".?AVTESFullName@@")? };
        // SAFETY: TESFullName::fullName is a BGSLocalizedString (string-pool entry).
        unsafe { mem::fixed_string(read(component, RE::TESFullName::fullName)) }
    }

    /// The engine class, e.g. `TESObjectWEAP`.
    pub fn class_name(self) -> Option<String> {
        // SAFETY: a live polymorphic form.
        unsafe { crate::game().class_name(self.as_ptr()) }
    }

    /// This form as a placed reference (`REFR`, `ACHR`, …), if it is one.
    pub fn as_reference(self) -> Option<Reference> {
        let game = crate::game();
        // SAFETY: a live polymorphic form.
        unsafe { game.cast(self.as_ptr(), ".?AVTESObjectREFR@@") }.map(|_| Reference(self))
    }
}

impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Form({} {})", self.id(), self.form_type().signature())
    }
}
