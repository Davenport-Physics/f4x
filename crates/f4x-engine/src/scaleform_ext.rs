//! More of the game's Scaleform: member enumeration and `img://` images.
//!
//! Found by our own analysis of `Fallout4.exe` (`provenance/scaleform-features.jsonl`):
//! - `GFx::Value::ObjectInterface::VisitMembers(data, ObjVisitor*, isdobj)` visits an AS3
//!   object's dynamic members, then (when the visitor's `IncludeAS3PublicMembers`, slot 1,
//!   says so) its public traits, then a display object's children by instance name.
//!   `ObjVisitor` (CommonLibF4 `GFx_Player.h`): dtor, `IncludeAS3PublicMembers() -> bool`,
//!   `Visit(const char* name, const Value&)`;
//! - `BSScaleformManager::imageLoader` (+0x28, CommonLibF4) is the game's
//!   `BSScaleformImageLoader`. Its `img://<name>` lookup (vtable slot 1) finds the image
//!   under the game's CRC32 of the lower-cased name in a map at +0x28. The game fills that
//!   map through `BSScaleformExternalTexture` (CommonLibF4 `IMenu.h`: `NiPointer<NiTexture>`
//!   +0x00, render target +0x08, path `BSFixedString` +0x10): `SetTexture` registers the
//!   texture under its own name (`NiObjectNET::name`, +0x10), `ReleaseTexture` takes it out.
//!   BookMenu and TerminalMenu load a `.dds` with the texture loader, name the texture after
//!   the `img://` name, and call `SetTexture`; images are counted, so the same name can be
//!   registered more than once.
//!
//! These layouts (`ObjVisitor`'s slots, `NiTexture::name` +0x10, `NiRefObject` refcount
//! +0x08 and `DeleteThis` slot 1, `BSScaleformExternalTexture` 0x18 bytes) are the same on
//! NG 1.11.240 (next-gen fork `GFx_Value.h`, `NiTexture.h`, `NiRefObject.h`,
//! `BSScaleformExternalTexture.h`; generated NG tables).

use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::transmute;

use f4x_engine_sys::addresses as addr;

use crate::address::resolve;
use crate::layout::RE;
use crate::mem;
use crate::papyrus::FixedString;
use crate::scaleform::Value;

// ---------------------------------------------------------------- VisitMembers

/// Our `ObjVisitor`: vtable pointer, then the names collected.
#[repr(C)]
struct Visitor {
    vtbl: *const VisitorVtbl,
    public: bool,
    names: Vec<String>,
}

unsafe extern "C" fn visitor_dtor(this: *mut Visitor, _flags: u32) -> *mut Visitor {
    this
}

unsafe extern "C" fn visitor_include_public(this: *const Visitor) -> bool {
    // SAFETY: our visitor.
    unsafe { (*this).public }
}

unsafe extern "C" fn visitor_visit(this: *mut Visitor, name: *const c_char, _value: *const Value) {
    if name.is_null() {
        return;
    }
    // SAFETY: our visitor; the engine's NUL-terminated name, valid for the call.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (*this).names.push(CStr::from_ptr(name).to_string_lossy().into_owned());
    }));
}

/// `ObjVisitor`'s vtable: dtor, `IncludeAS3PublicMembers`, `Visit`.
#[repr(C)]
struct VisitorVtbl {
    dtor: unsafe extern "C" fn(*mut Visitor, u32) -> *mut Visitor,
    include_public: unsafe extern "C" fn(*const Visitor) -> bool,
    visit: unsafe extern "C" fn(*mut Visitor, *const c_char, *const Value),
}

static VISITOR_VTABLE: VisitorVtbl = VisitorVtbl { dtor: visitor_dtor, include_public: visitor_include_public, visit: visitor_visit };

/// The member names of an object, in the order the engine visits them (`VisitMembers`).
/// `public` also visits AS3 public traits. Empty for values that are not objects.
#[must_use]
pub fn visit_members(value: &Value, public: bool) -> Vec<String> {
    if !value.is_object() {
        return Vec::new();
    }
    // SAFETY: GFx::Value is four plain words: object interface, type, data, aux.
    let [iface, _, data, _] = unsafe { std::ptr::read((value as *const Value).cast::<[usize; 4]>()) };
    let Some(f) = resolve(&addr::GFX_OBJECT_VISIT_MEMBERS) else { return Vec::new() };
    if iface == 0 || data == 0 {
        return Vec::new();
    }
    let mut visitor = Visitor { vtbl: &VISITOR_VTABLE, public, names: Vec::new() };
    let dobj = value.kind() == crate::scaleform::kind::DISPLAY_OBJECT;
    // SAFETY: ObjectInterface::VisitMembers(this, void* data, ObjVisitor*, bool isdobj).
    unsafe { transmute::<usize, unsafe extern "C" fn(usize, usize, *mut Visitor, bool)>(f)(iface, data, &mut visitor, dobj) };
    visitor.names
}

// ---------------------------------------------------------------- img:// images

/// One reference to an `NiTexture` (`NiRefObject`: refcount `u32` at +0x08, `DeleteThis`
/// in vtable slot 1).
pub struct Texture(*mut u8);

// SAFETY: NiTextures are reference counted with atomic operations.
unsafe impl Send for Texture {}

impl Texture {
    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.0
    }

    /// The texture's name (`NiObjectNET::name`).
    #[must_use]
    pub fn name(&self) -> Option<String> {
        // SAFETY: a live NiTexture; a BSFixedString field.
        unsafe { mem::fixed_string(mem::read(self.0, RE::NiTexture::name)) }
    }

    /// Rename the texture (as BookMenu does before registering it for `img://`).
    pub fn set_name(&self, name: &str) -> bool {
        let Some(new) = FixedString::new(name) else { return false };
        // SAFETY: the BSFixedString at +0x10: swap in our reference, release the old one.
        unsafe {
            let slot = self.0.add(RE::NiTexture::name).cast::<*mut u8>();
            let old = slot.read();
            slot.write(new.into_raw());
            if !old.is_null() {
                drop(FixedString::from_raw(old));
            }
        }
        true
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        // SAFETY: our reference; the last one deletes the texture (vtable slot 1).
        unsafe {
            let count = &*(self.0.add(RE::NiRefObject::refCount) as *const std::sync::atomic::AtomicU32);
            if count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
                let vtbl: *const usize = mem::read(self.0, 0);
                transmute::<usize, unsafe extern "C" fn(*mut u8)>(*vtbl.add(RE::NiRefObject::vtbl::DeleteThis))(self.0);
            }
        }
    }
}

/// Load a texture at full resolution (`textures\` is put in front of paths that lack
/// it). None for a missing file (status 1).
///
/// The loader's arguments are BookMenu's, except the last: BookMenu passes true and gets
/// mipmapped textures one level down (512x512 for a 1024x1024 file); false loads the top
/// level, as F4SE's mounts are (tested in-game, `provenance/scaleform-features.jsonl`).
#[must_use]
pub fn load_texture(path: &str) -> Option<Texture> {
    let f = resolve(&addr::TEXTURE_LOAD_BY_PATH)?;
    let path = CString::new(path).ok()?;
    let mut out: *mut u8 = std::ptr::null_mut();
    type Load = unsafe extern "C" fn(*const c_char, *mut *mut u8, bool, bool, u32, bool, bool, bool) -> u32;
    // SAFETY: the engine's texture loader (path, NiPointer<NiTexture>& out, flags).
    let status = unsafe { transmute::<usize, Load>(f)(path.as_ptr(), &mut out, true, false, 0, false, false, false) };
    log::debug!(target: "f4x::scaleform", "texture load {path:?}: status {status:#x}, texture {out:p}");
    (!out.is_null() && mem::readable(out as usize, 0x18)).then(|| Texture(out))
}

/// A `BSScaleformExternalTexture` of ours: while it holds a texture, `img://<the
/// texture's name>` finds it.
pub struct ExternalTexture(Box<[usize; 3]>);

// Texture in word 0, path (`BSFixedString`) in word 2.
const _: () = assert!(
    RE::BSScaleformExternalTexture::SIZE == 0x18
        && RE::BSScaleformExternalTexture::gamebryoTexture == 0
        && RE::BSScaleformExternalTexture::texturePath == 0x10
);

// SAFETY: engine data we own; used from one thread at a time (callers lock).
unsafe impl Send for ExternalTexture {}

impl Default for ExternalTexture {
    fn default() -> Self {
        ExternalTexture(Box::new([0; 3]))
    }
}

impl ExternalTexture {
    fn ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }

    /// The texture held, if any.
    #[must_use]
    pub fn texture(&self) -> *mut u8 {
        self.0[0] as *mut u8
    }

    /// `BSScaleformExternalTexture::SetTexture`: hold `texture` and register it under its
    /// name. False when the address is missing or the texture is the one already held.
    pub fn set(&mut self, texture: &Texture) -> bool {
        let Some(f) = resolve(&addr::SCALEFORM_EXTERNAL_TEXTURE_SET) else { return false };
        // SAFETY: SetTexture(this, NiTexture*) on our zero-initialised 0x18-byte object.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut c_void, *mut u8) -> bool>(f)(self.ptr(), texture.as_ptr()) }
    }

    /// `BSScaleformExternalTexture::ReleaseTexture`: unregister and drop the texture.
    pub fn release(&mut self) {
        if self.0.iter().all(|&w| w == 0) {
            return;
        }
        let Some(f) = resolve(&addr::SCALEFORM_EXTERNAL_TEXTURE_RELEASE) else { return };
        // SAFETY: ReleaseTexture(this) on our object.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut c_void)>(f)(self.ptr()) };
    }
}

impl Drop for ExternalTexture {
    fn drop(&mut self) {
        self.release();
        // The path field (+0x10) is a BSFixedString; ReleaseTexture clears it.
        if self.0[2] != 0 {
            // SAFETY: a BSFixedString reference we hold.
            drop(unsafe { FixedString::from_raw(self.0[2] as *mut u8) });
        }
    }
}

// ---------------------------------------------------------------- Papyrus objects held by Scaleform

/// Whether a script object of type `script` is bound to `handle` (`FindBoundObject`).
#[must_use]
pub fn bound_object_exists(vm: crate::papyrus::Vm, handle: u64, script: &str) -> bool {
    match vm.bound_object(handle, script) {
        Some(object) => {
            crate::papyrus::object_dec_ref(object);
            true
        }
        None => false,
    }
}

/// The VM's text for an object handle (`IObjectHandlePolicy::ConvertHandleToString`, slot
/// 0x0F; CommonLibF4 `BSScript.h`), e.g. ` (00000014)` for the player (name, then the form ID).
#[must_use]
pub fn handle_string(vm: crate::papyrus::Vm, handle: u64) -> Option<String> {
    // 0x33 and 0x0F on 1.10.163 and 1.11.240 (the generated tables; see `papyrus::slot`).
    use crate::papyrus::slot::{CONVERT_HANDLE_TO_STRING, GET_OBJECT_HANDLE_POLICY};
    // SAFETY: IVirtualMachine::GetObjectHandlePolicy() and the policy's
    // ConvertHandleToString(handle, BSFixedString&) on the live VM.
    unsafe {
        let policy = crate::papyrus::vcall::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(vm.0, GET_OBJECT_HANDLE_POLICY)(vm.0);
        if policy.is_null() {
            return None;
        }
        let mut text = FixedString::null();
        crate::papyrus::vcall::<unsafe extern "C" fn(*mut c_void, u64, *mut FixedString)>(policy, CONVERT_HANDLE_TO_STRING)(policy, handle, &mut text);
        Some(text.to_string_lossy())
    }
}
