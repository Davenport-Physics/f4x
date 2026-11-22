//! The Papyrus VM through engine internals: pooled strings, variables, Rust natives and
//! script calls.
//!
//! Layouts and vtable slots come from CommonLibF4 (`RE/Bethesda/BSScript.h`,
//! `BSScriptUtil.h`, `BSStringPool.h`, `MemoryManager.h`, `BSTArray.h`,
//! `msvc/functional.h`). The overloaded `IVirtualMachine` slots were cross-checked
//! against clang's MS-ABI vtable dump (`target/clangdump/vtables.txt`).
//!
//! A Rust native is an object laid out like `NF_util::NativeFunctionBase` (0x50 bytes)
//! with a vtable f4x builds at run time. Its `Call` slot is the engine's own
//! `NativeFunctionBase::Call`, which sets up the frame and calls back into our
//! `MarshallAndDispatch`, where arguments are read with `Stack::GetStackFrameVariable`.

use std::ffi::{CString, c_char, c_void};
use std::mem::transmute;
use std::ptr::null_mut;
use std::sync::{Arc, OnceLock};

use f4x_engine_sys::og_1_10_163_addresses as addr;

use crate::address::resolve;
use crate::form_type::FormType;
use crate::mem;

// ---------------------------------------------------------------- string pool

/// An owned reference to a pooled string (`BSFixedString`, case-insensitive pool).
#[repr(transparent)]
pub struct FixedString(*mut u8);

// SAFETY: pool entries are reference counted by the engine with atomics.
unsafe impl Send for FixedString {}
// SAFETY: as above; the entry is immutable.
unsafe impl Sync for FixedString {}

type GetEntry = unsafe extern "C" fn(*mut *mut u8, *const c_char, bool);
type ReleaseEntry = unsafe extern "C" fn(*mut *mut u8);

impl FixedString {
    /// Intern `text` (`BSStringPool::GetEntry<char>`).
    pub fn new(text: &str) -> Option<FixedString> {
        let get = resolve(&addr::STRING_POOL_GET_ENTRY)?;
        let c = CString::new(text.replace('\0', "")).ok()?;
        let mut entry = null_mut();
        // SAFETY: the engine's interning function: (Entry*& out, const char*, bool cs).
        unsafe { transmute::<usize, GetEntry>(get)(&mut entry, c.as_ptr(), false) };
        Some(FixedString(entry))
    }

    #[must_use]
    pub const fn null() -> FixedString {
        FixedString(null_mut())
    }

    #[must_use]
    pub fn as_raw(&self) -> *mut u8 {
        self.0
    }

    /// Give up ownership of the reference (e.g. to a `Variable`).
    #[must_use]
    pub fn into_raw(self) -> *mut u8 {
        let p = self.0;
        std::mem::forget(self);
        p
    }

    /// Take ownership of one reference to `entry`.
    ///
    /// # Safety
    /// `entry` must be null or a pool entry whose reference the caller owns.
    #[must_use]
    pub unsafe fn from_raw(entry: *mut u8) -> FixedString {
        FixedString(entry)
    }

    /// Another reference to the same entry.
    #[must_use]
    pub fn clone_ref(&self) -> FixedString {
        if self.0.is_null() {
            return FixedString::null();
        }
        FixedString::new(&self.to_string_lossy()).unwrap_or(FixedString::null())
    }

    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        // SAFETY: a live entry or null.
        unsafe { mem::fixed_string(self.0) }.unwrap_or_default()
    }
}

impl Drop for FixedString {
    fn drop(&mut self) {
        // SAFETY: we own one reference.
        unsafe { release_entry(&mut self.0) };
    }
}

/// Release the reference held in a `BSFixedString` field (`BSStringPool::Entry::release`).
///
/// # Safety
/// `slot` must point to a field holding null or an owned entry reference.
pub unsafe fn release_entry(slot: *mut *mut u8) {
    // SAFETY: caller contract.
    if unsafe { (*slot).is_null() } {
        return;
    }
    if let Some(release) = resolve(&addr::STRING_POOL_ENTRY_RELEASE) {
        // SAFETY: (Entry*&); nulls the field.
        unsafe { transmute::<usize, ReleaseEntry>(release)(slot) };
    }
}

// ---------------------------------------------------------------- variables

/// `TypeInfo::RawType`.
pub mod raw {
    pub const NONE: u32 = 0;
    pub const OBJECT: u32 = 1;
    pub const STRING: u32 = 2;
    pub const INT: u32 = 3;
    pub const FLOAT: u32 = 4;
    pub const BOOL: u32 = 5;
    pub const VAR: u32 = 6;
    pub const STRUCT: u32 = 7;
    pub const ARRAY_START: u32 = 10;
    pub const ARRAY_END: u32 = 18;
}

/// `BSScript::Variable`: a `TypeInfo` (raw type, or an `IComplexType*` with bit 0 as the
/// array flag) and a value.
#[repr(C)]
pub struct Variable {
    pub kind: usize,
    pub value: u64,
}

const _: () = assert!(std::mem::size_of::<Variable>() == 0x10);

/// `BSScript::Object` (a bound script instance).
pub type ScriptObject = c_void;

impl Variable {
    /// The raw type (for objects and structs, from the complex type).
    #[must_use]
    pub fn raw_type(&self) -> u32 {
        if self.kind >= raw::ARRAY_END as usize {
            let complex = self.kind & !1;
            if !mem::readable(complex, 8) {
                return raw::NONE;
            }
            // SAFETY: IComplexType vtable slot 1: GetRawType() -> RawType.
            let raw_type = unsafe { vcall::<unsafe extern "C" fn(usize) -> u32>(complex as *mut c_void, 1)(complex) };
            if self.kind & 1 != 0 { raw_type + raw::ARRAY_START } else { raw_type }
        } else {
            self.kind as u32
        }
    }

    #[must_use]
    pub fn is_none(&self) -> bool {
        self.kind == 0 || (self.raw_type() == raw::OBJECT && self.value == 0)
    }

    #[must_use]
    pub fn as_bool(&self) -> bool {
        self.value as u8 != 0
    }

    #[must_use]
    pub fn as_int(&self) -> i32 {
        self.value as u32 as i32
    }

    #[must_use]
    pub fn as_float(&self) -> f32 {
        match self.raw_type() {
            raw::INT => self.as_int() as f32,
            _ => f32::from_bits(self.value as u32),
        }
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        // SAFETY: a string variable holds a pool entry.
        unsafe { mem::fixed_string(self.value as *const u8) }.unwrap_or_default()
    }

    /// The script object, for object variables.
    #[must_use]
    pub fn as_object(&self) -> Option<*mut ScriptObject> {
        (self.raw_type() == raw::OBJECT && self.value != 0).then_some(self.value as *mut ScriptObject)
    }

    /// Drop what the variable holds (strings and objects; structs/arrays are left).
    pub fn reset(&mut self) {
        match self.raw_type() {
            raw::STRING => {
                // SAFETY: the variable owns its string reference.
                unsafe { release_entry((&raw mut self.value).cast()) };
            }
            raw::OBJECT if self.value != 0 => object_dec_ref(self.value as *mut ScriptObject),
            raw::NONE | raw::INT | raw::FLOAT | raw::BOOL => {}
            other => log::debug!(target: "f4x::papyrus", "variable of raw type {other} overwritten without release"),
        }
        self.kind = 0;
        self.value = 0;
    }

    pub fn set_none(&mut self) {
        self.reset();
    }

    pub fn set_bool(&mut self, v: bool) {
        self.reset();
        self.kind = raw::BOOL as usize;
        self.value = u64::from(v);
    }

    pub fn set_int(&mut self, v: i32) {
        self.reset();
        self.kind = raw::INT as usize;
        self.value = u64::from(v as u32);
    }

    pub fn set_float(&mut self, v: f32) {
        self.reset();
        self.kind = raw::FLOAT as usize;
        self.value = u64::from(v.to_bits());
    }

    pub fn set_string(&mut self, v: &str) {
        self.reset();
        if let Some(s) = FixedString::new(v) {
            self.kind = raw::STRING as usize;
            self.value = s.into_raw() as u64;
        }
    }

    /// Store an object whose reference the caller hands over.
    pub fn set_object(&mut self, object: *mut ScriptObject) {
        self.reset();
        if object.is_null() {
            return;
        }
        // SAFETY: Object::type (ObjectTypeInfo*) at +0x08.
        self.kind = unsafe { mem::read::<usize>(object as *const u8, 8) };
        self.value = object as u64;
    }
}

fn object_dec_ref(object: *mut ScriptObject) {
    if let Some(f) = resolve(&addr::SCRIPT_OBJECT_DEC_REF) {
        // SAFETY: Object::DecRef(this) -> u32 on a live object we hold a reference to.
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut ScriptObject) -> u32>(f)(object) };
    }
}

/// The VM handle of a script object (`Object::GetHandle`).
#[must_use]
pub fn object_handle(object: *mut ScriptObject) -> Option<u64> {
    let f = resolve(&addr::SCRIPT_OBJECT_GET_HANDLE)?;
    // SAFETY: Object::GetHandle(this) -> usize.
    Some(unsafe { transmute::<usize, unsafe extern "C" fn(*mut ScriptObject) -> u64>(f)(object) })
}

/// The script (type) name of a script object (`Object::type->name`).
#[must_use]
pub fn object_script_name(object: *mut ScriptObject) -> Option<String> {
    // SAFETY: Object::type at +0x08; ObjectTypeInfo::name at +0x10.
    unsafe {
        let info: *const u8 = mem::read(object as *const u8, 8);
        if info.is_null() {
            return None;
        }
        mem::fixed_string(mem::read::<*const u8>(info, crate::layout::RE::BSScript::ObjectTypeInfo::name))
    }
}

/// Read vtable slot `slot` of `this`.
///
/// # Safety
/// `this` must be a live polymorphic object with at least `slot + 1` slots.
unsafe fn vcall<F: Copy>(this: *mut c_void, slot: usize) -> F {
    // SAFETY: caller contract.
    unsafe {
        let vtbl: *const usize = *(this as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(slot))
    }
}

// ---------------------------------------------------------------- the VM

mod slot {
    pub const GET_SCRIPT_OBJECT_TYPE_BY_NAME: usize = 0x08;
    pub const CREATE_OBJECT: usize = 0x17;
    pub const BIND_NATIVE_METHOD: usize = 0x1B;
    pub const FIND_BOUND_OBJECT: usize = 0x1F;
    pub const DISPATCH_METHOD_CALL_BY_HANDLE: usize = 0x2E;
    pub const GET_OBJECT_HANDLE_POLICY: usize = 0x33;
    pub const GET_OBJECT_BIND_POLICY: usize = 0x35;
    // IObjectHandlePolicy
    pub const IS_HANDLE_LOADED: usize = 0x03;
    pub const EMPTY_HANDLE: usize = 0x06;
    pub const GET_HANDLE_FOR_OBJECT: usize = 0x07;
    pub const GET_OBJECT_FOR_HANDLE: usize = 0x0C;
    pub const GET_HANDLE_TYPE: usize = 0x02;
    // IVirtualMachine (more)
    pub const GET_SCRIPT_OBJECT_TYPE_BY_ID: usize = 0x09;
    pub const DISPATCH_STATIC_CALL: usize = 0x2C;
    pub const DISPATCH_METHOD_CALL_ON_OBJECT: usize = 0x2D;
}

/// The Papyrus VM (`BSScript::IVirtualMachine*`).
#[derive(Clone, Copy, Debug)]
pub struct Vm(pub *mut c_void);

// SAFETY: the VM is a process-wide engine object with its own locking.
unsafe impl Send for Vm {}
// SAFETY: as above.
unsafe impl Sync for Vm {}

impl Vm {
    /// The `ObjectTypeInfo*` for a script name, loading the script if needed. The
    /// reference taken is kept, so the pointer stays valid.
    #[must_use]
    pub fn object_type(self, script: &str) -> Option<usize> {
        let name = FixedString::new(script)?;
        let mut info: *mut c_void = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, *const FixedString, *mut *mut c_void) -> bool;
        // SAFETY: GetScriptObjectType(const BSFixedString&, BSTSmartPointer<ObjectTypeInfo>&).
        let ok = unsafe { vcall::<F>(self.0, slot::GET_SCRIPT_OBJECT_TYPE_BY_NAME)(self.0, &name, &mut info) };
        (ok && !info.is_null()).then_some(info as usize)
    }

    fn handle_policy(self) -> *mut c_void {
        // SAFETY: GetObjectHandlePolicy() -> IObjectHandlePolicy&.
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(self.0, slot::GET_OBJECT_HANDLE_POLICY)(self.0) }
    }

    fn bind_policy(self) -> *mut c_void {
        // SAFETY: GetObjectBindPolicy() -> ObjectBindPolicy&.
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(self.0, slot::GET_OBJECT_BIND_POLICY)(self.0) }
    }

    /// The handle for a game object of VM type `type_id` (for forms, the form type).
    #[must_use]
    pub fn handle_for(self, type_id: u32, object: *const c_void) -> Option<u64> {
        let policy = self.handle_policy();
        // SAFETY: IObjectHandlePolicy::EmptyHandle / GetHandleForObject(type, const void*).
        unsafe {
            let empty = vcall::<unsafe extern "C" fn(*mut c_void) -> u64>(policy, slot::EMPTY_HANDLE)(policy);
            let handle =
                vcall::<unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u64>(policy, slot::GET_HANDLE_FOR_OBJECT)(policy, type_id, object);
            (handle != empty).then_some(handle)
        }
    }

    /// The game object behind a handle, if loaded and of VM type `type_id`.
    #[must_use]
    pub fn object_for_handle(self, type_id: u32, handle: u64) -> Option<*mut u8> {
        let policy = self.handle_policy();
        // SAFETY: IObjectHandlePolicy::IsHandleLoaded / GetObjectForHandle(type, handle).
        unsafe {
            if !vcall::<unsafe extern "C" fn(*mut c_void, u64) -> bool>(policy, slot::IS_HANDLE_LOADED)(policy, handle) {
                return None;
            }
            let p = vcall::<unsafe extern "C" fn(*mut c_void, u32, u64) -> *mut u8>(policy, slot::GET_OBJECT_FOR_HANDLE)(policy, type_id, handle);
            (!p.is_null()).then_some(p)
        }
    }

    /// The form behind a script object variable.
    #[must_use]
    pub fn form_of(self, variable: &Variable, form_type: FormType) -> Option<*mut u8> {
        let object = variable.as_object()?;
        self.object_for_handle(u32::from(form_type.0), object_handle(object)?)
    }

    /// A script object for a form (bound or newly created and bound), with one reference
    /// for the caller. `script` is the script type, e.g. `EncounterZone`.
    #[must_use]
    pub fn object_for_form(self, form: *const u8, form_type: FormType, script: &str) -> Option<*mut ScriptObject> {
        let handle = self.handle_for(u32::from(form_type.0), form.cast())?;
        let script_c = CString::new(script).ok()?;
        let mut object: *mut ScriptObject = null_mut();
        type Find = unsafe extern "C" fn(*mut c_void, u64, *const c_char, bool, *mut *mut ScriptObject, bool) -> bool;
        // SAFETY: FindBoundObject(handle, const char*, allowConst, BSTSmartPointer<Object>&, exact).
        let found = unsafe { vcall::<Find>(self.0, slot::FIND_BOUND_OBJECT)(self.0, handle, script_c.as_ptr(), false, &mut object, false) };
        if found && !object.is_null() {
            return Some(object);
        }
        let name = FixedString::new(script)?;
        type Create = unsafe extern "C" fn(*mut c_void, *const FixedString, *mut *mut ScriptObject) -> bool;
        // SAFETY: CreateObject(const BSFixedString&, BSTSmartPointer<Object>&).
        let created = unsafe { vcall::<Create>(self.0, slot::CREATE_OBJECT)(self.0, &name, &mut object) };
        if !created || object.is_null() {
            return None;
        }
        let bind = resolve(&addr::OBJECT_BIND_POLICY_BIND)?;
        let policy = self.bind_policy();
        // SAFETY: ObjectBindPolicy::BindObject(const BSTSmartPointer<Object>&, handle).
        unsafe { transmute::<usize, unsafe extern "C" fn(*mut c_void, *const *mut ScriptObject, u64)>(bind)(policy, &object, handle) };
        Some(object)
    }

    /// Bind a native function object (`BindNativeMethod`).
    ///
    /// # Safety
    /// `function` must be a live `IFunction` that outlives the VM.
    pub unsafe fn bind_native_method(self, function: *mut c_void) -> bool {
        // SAFETY: caller contract.
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool>(self.0, slot::BIND_NATIVE_METHOD)(self.0, function) }
    }

    /// The script type name the VM uses for a VM type id (for forms, the form type),
    /// e.g. `Location` for `LCTN`.
    #[must_use]
    pub fn script_for_type(self, type_id: u32) -> Option<String> {
        let mut info: *mut c_void = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void) -> bool;
        // SAFETY: GetScriptObjectType(u32 typeID, BSTSmartPointer<ObjectTypeInfo>&).
        let ok = unsafe { vcall::<F>(self.0, slot::GET_SCRIPT_OBJECT_TYPE_BY_ID)(self.0, type_id, &mut info) };
        if !ok || info.is_null() {
            return None;
        }
        // SAFETY: ObjectTypeInfo::name.
        unsafe { mem::fixed_string(mem::read::<*const u8>(info as *const u8, crate::layout::RE::BSScript::ObjectTypeInfo::name)) }
    }

    /// The game object (e.g. a form) a script object is bound to, with its VM type id.
    #[must_use]
    pub fn game_object(self, object: *mut ScriptObject) -> Option<(u32, *mut u8)> {
        let handle = object_handle(object)?;
        let policy = self.handle_policy();
        let mut type_id = 0u32;
        // SAFETY: IObjectHandlePolicy::GetHandleType(handle, u32&).
        let ok = unsafe { vcall::<unsafe extern "C" fn(*mut c_void, u64, *mut u32) -> bool>(policy, slot::GET_HANDLE_TYPE)(policy, handle, &mut type_id) };
        if !ok {
            return None;
        }
        Some((type_id, self.object_for_handle(type_id, handle)?))
    }

    /// Call `function` on a script object; the result arrives on a VM thread.
    pub fn call_method(self, object: *mut ScriptObject, function: &str, args: Vec<Arg>) -> Option<Pending> {
        let function = FixedString::new(function)?;
        let functor = ArgsFunctor::new(args);
        let (callback, pending) = ResultCallback::new();
        type F = unsafe extern "C" fn(*mut c_void, *const *mut ScriptObject, *const FixedString, *const ArgsFunctor, *const *mut c_void) -> bool;
        // SAFETY: DispatchMethodCall(const BSTSmartPointer<Object>&, const BSFixedString&,
        // const BSTThreadScrapFunction<...>&, const BSTSmartPointer<IStackCallbackFunctor>&).
        let ok = unsafe { vcall::<F>(self.0, slot::DISPATCH_METHOD_CALL_ON_OBJECT)(self.0, &object, &function, &*functor, &callback) };
        ok.then_some(pending)
    }

    /// The script object of type `script` bound to `handle`, with one reference for the
    /// caller (`FindBoundObject`, exact type match).
    #[must_use]
    pub fn bound_object(self, handle: u64, script: &str) -> Option<*mut ScriptObject> {
        let script = CString::new(script).ok()?;
        let mut object: *mut ScriptObject = null_mut();
        type Find = unsafe extern "C" fn(*mut c_void, u64, *const c_char, bool, *mut *mut ScriptObject, bool) -> bool;
        // SAFETY: FindBoundObject(handle, const char*, allowConst, BSTSmartPointer<Object>&, exact).
        let found = unsafe { vcall::<Find>(self.0, slot::FIND_BOUND_OBJECT)(self.0, handle, script.as_ptr(), true, &mut object, true) };
        (found && !object.is_null()).then_some(object)
    }

    /// Queue `function` on the `script` object bound to `handle`, through the object (so
    /// the VM itself reports a missing function, as it does for F4SE's dispatches).
    pub fn dispatch_on_bound(self, handle: u64, script: &str, function: &str, args: Vec<Arg>) -> bool {
        let Some(object) = self.bound_object(handle, script) else { return false };
        let queued = self.call_method_no_result(object, function, args);
        object_dec_ref(object);
        queued
    }

    fn call_method_no_result(self, object: *mut ScriptObject, function: &str, args: Vec<Arg>) -> bool {
        let Some(function) = FixedString::new(function) else { return false };
        let functor = ArgsFunctor::new(args);
        let callback: *mut c_void = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, *const *mut ScriptObject, *const FixedString, *const ArgsFunctor, *const *mut c_void) -> bool;
        // SAFETY: DispatchMethodCall(const BSTSmartPointer<Object>&, name, args, callback).
        unsafe { vcall::<F>(self.0, slot::DISPATCH_METHOD_CALL_ON_OBJECT)(self.0, &object, &function, &*functor, &callback) }
    }

    /// Call a global (static) function `script.function`.
    pub fn call_static(self, script: &str, function: &str, args: Vec<Arg>) -> Option<Pending> {
        let (script, function) = (FixedString::new(script)?, FixedString::new(function)?);
        let functor = ArgsFunctor::new(args);
        let (callback, pending) = ResultCallback::new();
        type F = unsafe extern "C" fn(*mut c_void, *const FixedString, *const FixedString, *const ArgsFunctor, *const *mut c_void) -> bool;
        // SAFETY: DispatchStaticCall(const BSFixedString&, const BSFixedString&, args, callback).
        let ok = unsafe { vcall::<F>(self.0, slot::DISPATCH_STATIC_CALL)(self.0, &script, &function, &*functor, &callback) };
        ok.then_some(pending)
    }

    /// Queue a call of `script.function` on the object bound to `handle` (an event
    /// such as `OnControlDown` is an ordinary function call here). Returns whether the
    /// VM accepted it.
    pub fn dispatch_method_call(self, handle: u64, script: &str, function: &str, args: Vec<Arg>) -> bool {
        let (Some(script), Some(function)) = (FixedString::new(script), FixedString::new(function)) else { return false };
        let functor = ArgsFunctor::new(args);
        let callback: *mut c_void = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, u64, *const FixedString, *const FixedString, *const ArgsFunctor, *const *mut c_void) -> bool;
        // SAFETY: DispatchMethodCall(handle, const BSFixedString& obj, const BSFixedString& fn,
        // const BSTThreadScrapFunction<bool(BSScrapArray<Variable>&)>&, const BSTSmartPointer<IStackCallbackFunctor>&).
        unsafe { vcall::<F>(self.0, slot::DISPATCH_METHOD_CALL_BY_HANDLE)(self.0, handle, &script, &function, &*functor, &callback) }
    }
}

// ---------------------------------------------------------------- call results

/// A value returned by a script function.
#[derive(Clone, Debug, PartialEq)]
pub enum Returned {
    None,
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
    /// A script object: its handle and script name (and the game object, if any).
    Object { handle: u64, script: String },
    /// Arrays, structs and variants, by raw type.
    Other(u32),
}

/// The result of a queued call, delivered by the VM.
#[derive(Clone)]
pub struct Pending(Arc<std::sync::Mutex<Option<Returned>>>);

impl Pending {
    /// The result, once the call has finished.
    #[must_use]
    pub fn get(&self) -> Option<Returned> {
        self.0.lock().ok().and_then(|r| r.clone())
    }
}

/// `BSScript::IStackCallbackFunctor` (CommonLibF4 community branch,
/// `BSScript/IStackCallbackFunctor.h`): refcount at +8, then our result slot.
#[repr(C)]
struct ResultCallback {
    vtbl: *const usize,
    refcount: u32,
    _pad: u32,
    result: *const std::sync::Mutex<Option<Returned>>,
}

impl ResultCallback {
    /// A callback the VM may keep: it is never freed (one small object per call).
    fn new() -> (*mut c_void, Pending) {
        static VTABLE: OnceLock<[usize; 7]> = OnceLock::new();
        let vtbl = VTABLE.get_or_init(|| {
            [
                cb_dtor as *const () as usize,
                cb_nop as *const () as usize, // CallQueued
                cb_canceled as *const () as usize,
                cb_nop as *const () as usize, // StartMultiDispatch
                cb_nop as *const () as usize, // EndMultiDispatch
                cb_call as *const () as usize,
                cb_false as *const () as usize, // CanSave
            ]
        });
        let slot = Arc::new(std::sync::Mutex::new(None));
        let callback = Box::leak(Box::new(ResultCallback { vtbl: vtbl.as_ptr(), refcount: 1, _pad: 0, result: Arc::into_raw(slot.clone()) }));
        ((callback as *mut ResultCallback).cast(), Pending(slot))
    }
}

unsafe extern "C" fn cb_dtor(this: *mut ResultCallback, _flags: u32) -> *mut ResultCallback {
    this
}
unsafe extern "C" fn cb_nop(_this: *mut ResultCallback) {}
unsafe extern "C" fn cb_false(_this: *mut ResultCallback) -> bool {
    false
}
unsafe extern "C" fn cb_canceled(this: *mut ResultCallback) {
    // SAFETY: our live callback.
    if let Ok(mut r) = unsafe { &*(*this).result }.lock() {
        r.get_or_insert(Returned::Other(u32::MAX));
    }
}
/// `operator()(Variable)`: the by-value `Variable` arrives as a pointer to the caller's
/// temporary. It is read, not released (a leaked reference at worst).
unsafe extern "C" fn cb_call(this: *mut ResultCallback, value: *const Variable) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the VM's result variable.
        let v = unsafe { &*value };
        let returned = match v.raw_type() {
            raw::NONE => Returned::None,
            raw::BOOL => Returned::Bool(v.as_bool()),
            raw::INT => Returned::Int(v.as_int()),
            raw::FLOAT => Returned::Float(v.as_float()),
            raw::STRING => Returned::String(v.as_string()),
            raw::OBJECT => match v.as_object() {
                None => Returned::None,
                Some(o) => Returned::Object { handle: object_handle(o).unwrap_or(0), script: object_script_name(o).unwrap_or_default() },
            },
            other => Returned::Other(other),
        };
        // SAFETY: our live callback.
        if let Ok(mut r) = unsafe { &*(*this).result }.lock() {
            *r = Some(returned);
        }
    });
}

// ---------------------------------------------------------------- call arguments

/// A value passed to a script function.
#[derive(Debug)]
pub enum Arg {
    None,
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
    /// A script object; one reference is handed over.
    Object(*mut ScriptObject),
}

// SAFETY: objects are reference counted by the engine.
unsafe impl Send for Arg {}
// SAFETY: as above.
unsafe impl Sync for Arg {}

/// `BSScrapArray<Variable>`: scrap-heap allocator (heap, data, capacity) then size.
#[repr(C)]
struct ScrapArray {
    heap: *mut c_void,
    data: *mut Variable,
    capacity: u32,
    _pad: u32,
    size: u32,
}

const _: () = assert!(std::mem::offset_of!(ScrapArray, size) == 0x18);

fn scrap_alloc(array: &mut ScrapArray, bytes: usize) -> Option<*mut Variable> {
    if array.heap.is_null() {
        let singleton = resolve(&addr::MEMORY_MANAGER_SINGLETON)?;
        let scrap = resolve(&addr::MEMORY_MANAGER_SCRAP_HEAP)?;
        // SAFETY: MemoryManager::GetSingleton() -> MemoryManager&; GetThreadScrapHeap(this).
        unsafe {
            let manager = transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)();
            array.heap = transmute::<usize, unsafe extern "C" fn(*mut c_void) -> *mut c_void>(scrap)(manager);
        }
    }
    if array.heap.is_null() {
        return None;
    }
    let allocate = resolve(&addr::SCRAP_HEAP_ALLOCATE)?;
    // SAFETY: ScrapHeap::Allocate(this, size, align).
    let p = unsafe { transmute::<usize, unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut u8>(allocate)(array.heap, bytes, 8) };
    (!p.is_null()).then_some(p.cast())
}

/// `msvc::function<bool(BSScrapArray<Variable>&)>`: 0x18 bytes of inline storage, then
/// the callable's proxy pointer (CommonLibF4 `msvc/functional.h`). Our proxy fits the
/// inline storage: vtable + shared argument list.
#[repr(C)]
struct ArgsFunctor {
    storage: [usize; 3],
    proxy: *mut ArgsProxy,
}

#[repr(C)]
struct ArgsProxy {
    vtbl: *const [usize; 7],
    args: *const Vec<Arg>, // Arc<Vec<Arg>>::into_raw
    _unused: usize,
}

impl ArgsFunctor {
    fn new(args: Vec<Arg>) -> Box<ArgsFunctor> {
        let mut functor = Box::new(ArgsFunctor { storage: [0; 3], proxy: null_mut() });
        let proxy = functor.storage.as_mut_ptr().cast::<ArgsProxy>();
        // SAFETY: the storage is 0x18 bytes, exactly an ArgsProxy.
        unsafe { proxy.write(ArgsProxy { vtbl: args_vtable(), args: Arc::into_raw(Arc::new(args)), _unused: 0 }) };
        functor.proxy = proxy;
        functor
    }
}

impl Drop for ArgsFunctor {
    fn drop(&mut self) {
        // SAFETY: our own proxy in our storage.
        unsafe { proxy_delete_this(self.proxy, false) };
    }
}

fn args_vtable() -> *const [usize; 7] {
    static VTABLE: OnceLock<[usize; 7]> = OnceLock::new();
    VTABLE.get_or_init(|| {
        [
            proxy_copy as *const () as usize,
            proxy_copy as *const () as usize, // move: the moved-from proxy is deleted afterwards
            proxy_call as *const () as usize,
            proxy_target_type as *const () as usize,
            proxy_delete_this as *const () as usize,
            proxy_dtor as *const () as usize,
            proxy_get as *const () as usize,
        ]
    })
}

unsafe extern "C" fn proxy_copy(this: *mut ArgsProxy, storage: *mut c_void) -> *mut ArgsProxy {
    // SAFETY: `this` is a live proxy; the destination storage holds 0x18 bytes.
    unsafe {
        Arc::increment_strong_count((*this).args);
        let out = storage.cast::<ArgsProxy>();
        out.write(ArgsProxy { vtbl: (*this).vtbl, args: (*this).args, _unused: 0 });
        out
    }
}

unsafe extern "C" fn proxy_delete_this(this: *mut ArgsProxy, _deallocate: bool) {
    // SAFETY: each proxy owns one count of its argument list.
    unsafe {
        if !(*this).args.is_null() {
            Arc::decrement_strong_count((*this).args);
            (*this).args = std::ptr::null();
        }
    }
}

unsafe extern "C" fn proxy_dtor(_this: *mut ArgsProxy) {}

unsafe extern "C" fn proxy_target_type(_this: *mut ArgsProxy) -> *const c_void {
    static NONE: [usize; 4] = [0; 4];
    NONE.as_ptr().cast()
}

unsafe extern "C" fn proxy_get(this: *mut ArgsProxy) -> *const c_void {
    // SAFETY: live proxy.
    unsafe { (*this).args.cast() }
}

/// Fill the VM's argument array.
unsafe extern "C" fn proxy_call(this: *mut ArgsProxy, array: *mut ScrapArray) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: live proxy and the VM's array.
        let (args, array) = unsafe { (&*(*this).args, &mut *array) };
        let n = args.len() as u32;
        if n == 0 {
            return true;
        }
        if array.capacity < n {
            let Some(data) = scrap_alloc(array, n as usize * size_of::<Variable>()) else { return false };
            array.data = data;
            array.capacity = n;
        }
        for (i, arg) in args.iter().enumerate() {
            // SAFETY: within capacity; slots are written fresh.
            let var = unsafe { &mut *array.data.add(i) };
            var.kind = 0;
            var.value = 0;
            match arg {
                Arg::None => {}
                Arg::Bool(v) => var.set_bool(*v),
                Arg::Int(v) => var.set_int(*v),
                Arg::Float(v) => var.set_float(*v),
                Arg::String(v) => var.set_string(v),
                Arg::Object(o) => {
                    // Each call of the functor hands out its own reference.
                    if let Some(f) = resolve(&addr::SCRIPT_OBJECT_INC_REF) {
                        // SAFETY: Object::IncRef(this).
                        unsafe { transmute::<usize, unsafe extern "C" fn(*mut ScriptObject)>(f)(*o) };
                    }
                    var.set_object(*o);
                }
            }
        }
        array.size = n;
        true
    })
    .unwrap_or(false)
}

// ---------------------------------------------------------------- natives

/// A parameter or return type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    None,
    Bool,
    Int,
    Float,
    String,
    /// A script object type (e.g. `EncounterZone`).
    Object(&'static str),
}

/// The Rust side of a native.
pub type Handler = fn(&mut NativeCall) -> Result<(), String>;

/// A native function declaration.
#[derive(Clone, Copy, Debug)]
pub struct NativeSpec {
    pub script: &'static str,
    pub name: &'static str,
    pub params: &'static [(&'static str, Type)],
    pub ret: Type,
    pub is_static: bool,
    pub handler: Handler,
}

/// One call of a native: typed access to `self`, the arguments and the result.
pub struct NativeCall {
    pub vm: Vm,
    pub stack_id: u32,
    pub spec: &'static NativeSpec,
    self_var: *mut Variable,
    args: Vec<*mut Variable>,
    ret: *mut Variable,
}

impl NativeCall {
    fn arg(&self, i: usize) -> Option<&Variable> {
        // SAFETY: frame variables are live for the call.
        self.args.get(i).and_then(|&p| (!p.is_null()).then(|| unsafe { &*p }))
    }

    /// `self` as a script object.
    #[must_use]
    pub fn self_object(&self) -> Option<*mut ScriptObject> {
        // SAFETY: live for the call.
        unsafe { self.self_var.as_ref() }.and_then(Variable::as_object)
    }

    /// `self` as a form of the given type.
    #[must_use]
    pub fn self_form(&self, form_type: FormType) -> Option<*mut u8> {
        // SAFETY: live for the call.
        self.vm.form_of(unsafe { self.self_var.as_ref()? }, form_type)
    }

    #[must_use]
    pub fn bool(&self, i: usize) -> bool {
        self.arg(i).is_some_and(Variable::as_bool)
    }

    #[must_use]
    pub fn int(&self, i: usize) -> i32 {
        self.arg(i).map_or(0, Variable::as_int)
    }

    #[must_use]
    pub fn float(&self, i: usize) -> f32 {
        self.arg(i).map_or(0.0, Variable::as_float)
    }

    #[must_use]
    pub fn string(&self, i: usize) -> String {
        self.arg(i).map(Variable::as_string).unwrap_or_default()
    }

    #[must_use]
    pub fn form(&self, i: usize, form_type: FormType) -> Option<*mut u8> {
        self.vm.form_of(self.arg(i)?, form_type)
    }

    fn ret(&mut self) -> &mut Variable {
        // SAFETY: the VM's return slot, live for the call.
        unsafe { &mut *self.ret }
    }

    pub fn ret_bool(&mut self, v: bool) {
        self.ret().set_bool(v);
    }

    pub fn ret_int(&mut self, v: i32) {
        self.ret().set_int(v);
    }

    pub fn ret_float(&mut self, v: f32) {
        self.ret().set_float(v);
    }

    pub fn ret_string(&mut self, v: &str) {
        self.ret().set_string(v);
    }

    /// Return a form as an object of the declared return script type (None if null).
    pub fn ret_form(&mut self, form: Option<*mut u8>, form_type: FormType) {
        let Type::Object(script) = self.spec.ret else {
            self.ret().set_none();
            return;
        };
        let object = form.and_then(|f| self.vm.object_for_form(f, form_type, script));
        match object {
            Some(object) => self.ret().set_object(object),
            None => self.ret().set_none(),
        }
    }
}

/// Our `NF_util::NativeFunctionBase` (0x50 bytes) plus the Rust spec.
#[repr(C)]
struct NativeFunction {
    vtbl: *const usize,
    refcount: u32,
    _pad: u32,
    name: FixedString,
    object_name: FixedString,
    state_name: FixedString,
    ret_type: usize,
    desc_entries: *mut DescEntry,
    desc_param_count: u16,
    desc_total: u16,
    _pad2: u32,
    is_static: bool,
    callable_from_tasklet: bool,
    is_latent: bool,
    _pad3: u8,
    user_flags: u32,
    doc_string: FixedString,
    // ---- f4x
    spec: &'static NativeSpec,
}

/// `BSTTuple<BSFixedString, TypeInfo>`.
#[repr(C)]
struct DescEntry {
    name: FixedString,
    kind: usize,
}

const _: () = {
    assert!(std::mem::offset_of!(NativeFunction, name) == 0x10);
    assert!(std::mem::offset_of!(NativeFunction, ret_type) == 0x28);
    assert!(std::mem::offset_of!(NativeFunction, desc_entries) == 0x30);
    assert!(std::mem::offset_of!(NativeFunction, desc_param_count) == 0x38);
    assert!(std::mem::offset_of!(NativeFunction, is_static) == 0x40);
    assert!(std::mem::offset_of!(NativeFunction, user_flags) == 0x44);
    assert!(std::mem::offset_of!(NativeFunction, doc_string) == 0x48);
    assert!(std::mem::offset_of!(NativeFunction, spec) == 0x50);
};

fn type_info(vm: Vm, t: Type) -> Result<usize, String> {
    Ok(match t {
        Type::None => raw::NONE as usize,
        Type::Bool => raw::BOOL as usize,
        Type::Int => raw::INT as usize,
        Type::Float => raw::FLOAT as usize,
        Type::String => raw::STRING as usize,
        Type::Object(script) => vm.object_type(script).ok_or_else(|| format!("no script type {script}"))?,
    })
}

fn native_vtable() -> Option<*const usize> {
    static VTABLE: OnceLock<Option<Box<[usize; 23]>>> = OnceLock::new();
    VTABLE
        .get_or_init(|| {
            let call = resolve(&addr::NATIVE_FUNCTION_BASE_CALL)?;
            Some(Box::new([
                nf_dtor as *const () as usize,
                nf_get_name as *const () as usize,
                nf_get_object_type_name as *const () as usize,
                nf_get_state_name as *const () as usize,
                nf_get_return_type as *const () as usize,
                nf_get_param_count as *const () as usize,
                nf_get_param as *const () as usize,
                nf_get_param_count as *const () as usize, // GetStackFrameSize: no locals
                nf_true as *const () as usize,            // GetIsNative
                nf_get_is_static as *const () as usize,
                nf_false as *const () as usize, // GetIsEmpty
                nf_zero as *const () as usize,  // GetFunctionType: kNormal
                nf_get_user_flags as *const () as usize,
                nf_get_doc_string as *const () as usize,
                nf_insert_locals as *const () as usize,
                call, // the engine's NativeFunctionBase::Call
                nf_get_source_filename as *const () as usize,
                nf_translate_ip as *const () as usize,
                nf_get_var_name as *const () as usize,
                nf_can_be_called_from_tasklets as *const () as usize,
                nf_set_callable_from_tasklets as *const () as usize,
                nf_true as *const () as usize, // HasStub
                nf_marshall_and_dispatch as *const () as usize,
            ]))
        })
        .as_ref()
        .map(|b| b.as_ptr())
}

/// Bind a Rust native into the VM. The function object lives for the process.
pub fn bind(vm: Vm, spec: &'static NativeSpec) -> Result<(), String> {
    let vtbl = native_vtable().ok_or("NativeFunctionBase::Call not found")?;
    let fs = |s: &str| FixedString::new(s).ok_or_else(|| "string pool unavailable".to_string());
    let mut entries = Vec::with_capacity(spec.params.len());
    for (name, t) in spec.params {
        entries.push(DescEntry { name: fs(name)?, kind: type_info(vm, *t)? });
    }
    let count = entries.len() as u16;
    let entries = Box::leak(entries.into_boxed_slice());
    let function = Box::leak(Box::new(NativeFunction {
        vtbl,
        refcount: 1 << 20, // never reaches zero
        _pad: 0,
        name: fs(spec.name)?,
        object_name: fs(spec.script)?,
        state_name: fs("")?,
        ret_type: type_info(vm, spec.ret)?,
        desc_entries: if entries.is_empty() { null_mut() } else { entries.as_mut_ptr() },
        desc_param_count: count,
        desc_total: count,
        _pad2: 0,
        is_static: spec.is_static,
        callable_from_tasklet: false,
        is_latent: false,
        _pad3: 0,
        user_flags: 0,
        doc_string: fs("")?,
        spec,
    }));
    // SAFETY: a leaked, fully initialised IFunction.
    if unsafe { vm.bind_native_method((function as *mut NativeFunction).cast()) } {
        Ok(())
    } else {
        Err(format!("the VM refused {}.{} (not declared native in the script?)", spec.script, spec.name))
    }
}

// The vtable entries. All are `this`-call member functions (rcx = this). Functions that
// return a `TypeInfo` by value use the MSVC member-function convention (hidden result
// pointer after `this`).

unsafe extern "C" fn nf_dtor(this: *mut NativeFunction, _flags: u32) -> *mut NativeFunction {
    this
}
unsafe extern "C" fn nf_get_name(this: *const NativeFunction) -> *const FixedString {
    // SAFETY: live function object.
    unsafe { &raw const (*this).name }
}
unsafe extern "C" fn nf_get_object_type_name(this: *const NativeFunction) -> *const FixedString {
    // SAFETY: as above.
    unsafe { &raw const (*this).object_name }
}
unsafe extern "C" fn nf_get_state_name(this: *const NativeFunction) -> *const FixedString {
    // SAFETY: as above.
    unsafe { &raw const (*this).state_name }
}
unsafe extern "C" fn nf_get_return_type(this: *const NativeFunction, out: *mut usize) -> *mut usize {
    // SAFETY: as above; `out` is the caller's TypeInfo.
    unsafe { *out = (*this).ret_type };
    out
}
unsafe extern "C" fn nf_get_param_count(this: *const NativeFunction) -> u32 {
    // SAFETY: as above.
    u32::from(unsafe { (*this).desc_total })
}
unsafe extern "C" fn nf_get_param(this: *const NativeFunction, index: u32, name: *mut *mut u8, kind: *mut usize) {
    // SAFETY: as above; `name`/`kind` are the caller's BSFixedString/TypeInfo.
    unsafe {
        let (new_name, new_kind) = if index < u32::from((*this).desc_param_count) {
            let entry = &*(*this).desc_entries.add(index as usize);
            (entry.name.clone_ref(), entry.kind)
        } else {
            (FixedString::new("").unwrap_or(FixedString::null()), 0)
        };
        release_entry(name);
        *name = new_name.into_raw();
        *kind = new_kind;
    }
}
unsafe extern "C" fn nf_true(_this: *const NativeFunction) -> bool {
    true
}
unsafe extern "C" fn nf_false(_this: *const NativeFunction) -> bool {
    false
}
unsafe extern "C" fn nf_zero(_this: *const NativeFunction) -> u32 {
    0
}
unsafe extern "C" fn nf_get_is_static(this: *const NativeFunction) -> bool {
    // SAFETY: live function object.
    unsafe { (*this).is_static }
}
unsafe extern "C" fn nf_get_user_flags(this: *const NativeFunction) -> u32 {
    // SAFETY: as above.
    unsafe { (*this).user_flags }
}
unsafe extern "C" fn nf_get_doc_string(this: *const NativeFunction) -> *const FixedString {
    // SAFETY: as above.
    unsafe { &raw const (*this).doc_string }
}
unsafe extern "C" fn nf_insert_locals(_this: *const NativeFunction, _frame: *mut c_void) {}
unsafe extern "C" fn nf_get_source_filename(_this: *const NativeFunction) -> *const FixedString {
    static NATIVE: OnceLock<FixedString> = OnceLock::new();
    NATIVE.get_or_init(|| FixedString::new("<native>").unwrap_or(FixedString::null()))
}
unsafe extern "C" fn nf_translate_ip(_this: *const NativeFunction, _ip: u32, line: *mut u32) -> bool {
    // SAFETY: the caller's out parameter.
    unsafe { *line = 0 };
    false
}
unsafe extern "C" fn nf_get_var_name(this: *const NativeFunction, index: u32, name: *mut *mut u8) -> bool {
    // SAFETY: live function object; `name` is the caller's BSFixedString.
    unsafe {
        let (new_name, ok) = if index < u32::from((*this).desc_total) {
            ((*(*this).desc_entries.add(index as usize)).name.clone_ref(), true)
        } else {
            (FixedString::new("").unwrap_or(FixedString::null()), false)
        };
        release_entry(name);
        *name = new_name.into_raw();
        ok
    }
}
unsafe extern "C" fn nf_can_be_called_from_tasklets(this: *const NativeFunction) -> bool {
    // SAFETY: live function object.
    unsafe { (*this).callable_from_tasklet }
}
unsafe extern "C" fn nf_set_callable_from_tasklets(this: *mut NativeFunction, callable: bool) {
    // SAFETY: as above.
    unsafe { (*this).callable_from_tasklet = callable };
}

type GetPage = unsafe extern "C" fn(*const c_void, *const c_void) -> u32;
type GetFrameVariable = unsafe extern "C" fn(*const c_void, *const c_void, u32, u32) -> *mut Variable;

unsafe extern "C" fn nf_marshall_and_dispatch(
    this: *const NativeFunction,
    self_var: *mut Variable,
    vm: *mut c_void,
    stack_id: u32,
    ret: *mut Variable,
    frame: *const c_void,
) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: live function object.
        let spec = unsafe { (*this).spec };
        // SAFETY: the VM's return slot.
        unsafe { (*ret).reset() };
        // SAFETY: StackFrame::parent (Stack*) at +0; StackFrame::size at +0x38.
        let (stack, frame_size) = unsafe { (mem::read::<*const c_void>(frame.cast(), 0), mem::read::<u32>(frame.cast(), 0x38)) };
        if stack.is_null() {
            return false;
        }
        let (Some(get_page), Some(get_var)) = (resolve(&addr::STACK_GET_PAGE_FOR_FRAME), resolve(&addr::STACK_GET_FRAME_VARIABLE)) else {
            return false;
        };
        // SAFETY: Stack::GetPageForFrame / GetStackFrameVariable on the calling stack.
        let args = unsafe {
            let page = transmute::<usize, GetPage>(get_page)(stack, frame);
            (0..spec.params.len() as u32)
                .map(|i| if i < frame_size { transmute::<usize, GetFrameVariable>(get_var)(stack, frame, i, page) } else { null_mut() })
                .collect()
        };
        let mut call = NativeCall { vm: Vm(vm), stack_id, spec, self_var, args, ret };
        if let Err(error) = (spec.handler)(&mut call) {
            log::warn!(target: "f4x::papyrus", "{}.{}: {error}", spec.script, spec.name);
        }
        true
    })
    .unwrap_or(false)
}

/// The name of a native's script and function, for logs.
#[must_use]
pub fn describe(spec: &NativeSpec) -> String {
    format!("{}.{}", spec.script, spec.name)
}

/// A NUL-terminated copy for engine APIs taking `const char*`.
#[must_use]
pub fn c_string(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}
