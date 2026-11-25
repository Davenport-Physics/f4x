//! Scaleform GFx: values, movies and Rust function handlers.
//!
//! Layouts and slots from CommonLibF4 (`RE/Scaleform/GFx/GFx_Player.h`,
//! `GFx_ASMovieRootBase.h`):
//! - `GFx::Value` (0x20): object interface, type, value union, aux data;
//! - `Movie::asMovieRoot` at +0x18; its vtable creates values and gets/sets variables;
//! - `FunctionHandler`: refcounted, `Call(const Params&)` in slot 1.
//!
//! All the same on NG 1.11.240: the next-gen fork's `Scaleform/G/GFx_Value.h`,
//! `GFx_Movie.h`, `GFx_ASMovieRootBase.h`, `GFx_FunctionHandler.h`, and our RE (the
//! exe's `Movie` wrappers load `[movie+18h]` and jump through the same root slots).
//!
//! The `Value::ObjectInterface` member functions resolve through signature recipes.
//! Movies are not thread-safe: use them on the thread that advances them (for menus,
//! inside `IMenu::AdvanceMovie`, which the agent hooks).

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::null_mut;
use std::sync::OnceLock;

use f4x_engine_sys::addresses as addr;

use crate::address::resolve;
use crate::mem;

/// `GFx::Value::ValueType`.
pub mod kind {
    pub const UNDEFINED: u32 = 0;
    pub const NULL: u32 = 1;
    pub const BOOL: u32 = 2;
    pub const INT: u32 = 3;
    pub const UINT: u32 = 4;
    pub const NUMBER: u32 = 5;
    pub const STRING: u32 = 6;
    pub const STRING_W: u32 = 7;
    pub const OBJECT: u32 = 8;
    pub const ARRAY: u32 = 9;
    pub const DISPLAY_OBJECT: u32 = 10;
    pub const CLOSURE: u32 = 11;
    pub const MANAGED: u32 = 0x40;
    pub const TYPE_MASK: u32 = 0x0F;

    #[must_use]
    pub fn name(k: u32) -> &'static str {
        match k & TYPE_MASK {
            UNDEFINED => "undefined",
            NULL => "null",
            BOOL => "boolean",
            INT => "int",
            UINT => "uint",
            NUMBER => "number",
            STRING => "string",
            STRING_W => "wstring",
            OBJECT => "object",
            ARRAY => "array",
            DISPLAY_OBJECT => "displayobject",
            CLOSURE => "function",
            _ => "?",
        }
    }
}

/// `GFx::Value`. Managed values (strings, objects, arrays, functions) hold a reference
/// that is released on drop.
#[repr(C)]
pub struct Value {
    iface: *mut c_void,
    kind: u32,
    _pad: u32,
    data: u64,
    aux: u64,
}

const _: () = assert!(std::mem::size_of::<Value>() == crate::layout::RE::Scaleform::GFx::Value::SIZE);

type AddRelease = unsafe extern "C" fn(*mut c_void, *mut Value, u64);
type HasMember = unsafe extern "C" fn(*mut c_void, u64, *const c_char, bool) -> bool;
type GetMember = unsafe extern "C" fn(*mut c_void, u64, *const c_char, *mut Value, bool) -> bool;
type SetMember = unsafe extern "C" fn(*mut c_void, u64, *const c_char, *const Value, bool) -> bool;
type Invoke = unsafe extern "C" fn(*mut c_void, u64, *mut Value, *const c_char, *const Value, usize, bool) -> bool;
type PushBack = unsafe extern "C" fn(*mut c_void, u64, *const Value) -> bool;

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Default for Value {
    fn default() -> Self {
        Value::undefined()
    }
}

impl Value {
    #[must_use]
    pub const fn undefined() -> Value {
        Value { iface: null_mut(), kind: kind::UNDEFINED, _pad: 0, data: 0, aux: 0 }
    }

    #[must_use]
    pub fn null() -> Value {
        Value { kind: kind::NULL, ..Value::undefined() }
    }

    #[must_use]
    pub fn bool(v: bool) -> Value {
        Value { kind: kind::BOOL, data: v as u64, ..Value::undefined() }
    }

    #[must_use]
    pub fn number(v: f64) -> Value {
        Value { kind: kind::NUMBER, data: v.to_bits(), ..Value::undefined() }
    }

    #[must_use]
    pub fn int(v: i32) -> Value {
        Value { kind: kind::INT, data: v as u32 as u64, ..Value::undefined() }
    }

    #[must_use]
    pub fn uint(v: u32) -> Value {
        Value { kind: kind::UINT, data: v as u64, ..Value::undefined() }
    }

    /// An unmanaged string: `text` must outlive every use of the value (pass it as a
    /// call argument or to `set_member`, which copy it).
    #[must_use]
    pub fn borrowed_str(text: &CStr) -> Value {
        Value { kind: kind::STRING, data: text.as_ptr() as u64, ..Value::undefined() }
    }

    #[must_use]
    pub fn kind(&self) -> u32 {
        self.kind & kind::TYPE_MASK
    }

    #[must_use]
    pub fn type_name(&self) -> &'static str {
        kind::name(self.kind)
    }

    #[must_use]
    pub fn is_object(&self) -> bool {
        matches!(self.kind(), kind::OBJECT | kind::ARRAY | kind::DISPLAY_OBJECT | kind::CLOSURE)
    }

    fn is_display_object(&self) -> bool {
        self.kind() == kind::DISPLAY_OBJECT
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        (self.kind() == kind::BOOL).then_some(self.data as u8 != 0)
    }

    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self.kind() {
            kind::INT => Some(f64::from(self.data as u32 as i32)),
            kind::UINT => Some(f64::from(self.data as u32)),
            kind::NUMBER => Some(f64::from_bits(self.data)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_string(&self) -> Option<String> {
        if self.kind() != kind::STRING || self.data == 0 {
            return None;
        }
        // SAFETY: a string value: `const char*`, or `const char**` when managed.
        unsafe {
            let p: *const c_char = if self.kind & kind::MANAGED != 0 { *(self.data as *const *const c_char) } else { self.data as *const c_char };
            (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        }
    }

    fn call<F: Copy>(id: &f4x_engine_sys::Address) -> Option<F> {
        // SAFETY: the resolved function has signature F (caller's choice of id).
        resolve(id).map(|p| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
    }

    #[must_use]
    pub fn has_member(&self, name: &str) -> bool {
        if !self.is_object() || self.iface.is_null() {
            return false;
        }
        let Some(f) = Value::call::<HasMember>(&addr::GFX_OBJECT_HAS_MEMBER) else { return false };
        let name = c(name);
        // SAFETY: ObjectInterface::HasMember(data, name, isDisplayObject).
        unsafe { f(self.iface, self.data, name.as_ptr(), self.is_display_object()) }
    }

    #[must_use]
    pub fn get_member(&self, name: &str) -> Option<Value> {
        if !self.is_object() || self.iface.is_null() {
            return None;
        }
        let f = Value::call::<GetMember>(&addr::GFX_OBJECT_GET_MEMBER)?;
        let name = c(name);
        let mut out = Value::undefined();
        // SAFETY: ObjectInterface::GetMember(data, name, Value* out, isDisplayObject).
        unsafe { f(self.iface, self.data, name.as_ptr(), &mut out, self.is_display_object()) }.then_some(out)
    }

    pub fn set_member(&mut self, name: &str, value: &Value) -> bool {
        if !self.is_object() || self.iface.is_null() {
            return false;
        }
        let Some(f) = Value::call::<SetMember>(&addr::GFX_OBJECT_SET_MEMBER) else { return false };
        let name = c(name);
        // SAFETY: ObjectInterface::SetMember(data, name, const Value&, isDisplayObject).
        unsafe { f(self.iface, self.data, name.as_ptr(), value, self.is_display_object()) }
    }

    /// Call method `name` of this object.
    #[must_use]
    pub fn invoke(&self, name: &str, args: &[Value]) -> Option<Value> {
        if !self.is_object() || self.iface.is_null() {
            return None;
        }
        let f = Value::call::<Invoke>(&addr::GFX_OBJECT_INVOKE)?;
        let name = c(name);
        let mut out = Value::undefined();
        // SAFETY: ObjectInterface::Invoke(data, Value* result, name, args, count, isDisplayObject).
        unsafe { f(self.iface, self.data, &mut out, name.as_ptr(), args.as_ptr(), args.len(), self.is_display_object()) }.then_some(out)
    }

    pub fn push_back(&mut self, value: &Value) -> bool {
        if self.kind() != kind::ARRAY || self.iface.is_null() {
            return false;
        }
        let Some(f) = Value::call::<PushBack>(&addr::GFX_OBJECT_PUSH_BACK) else { return false };
        // SAFETY: ObjectInterface::PushBack(data, const Value&).
        unsafe { f(self.iface, self.data, value) }
    }

    /// Array length (the AS3 `length` property).
    #[must_use]
    pub fn length(&self) -> Option<usize> {
        self.get_member("length")?.as_number().map(|n| n as usize)
    }

    /// Element `i` of an array.
    #[must_use]
    pub fn element(&self, i: usize) -> Option<Value> {
        self.get_member(&i.to_string())
    }

    /// Another reference to the same value.
    #[must_use]
    pub fn clone_ref(&self) -> Value {
        let mut out = Value { iface: self.iface, kind: self.kind, _pad: 0, data: self.data, aux: self.aux };
        if self.kind & kind::MANAGED != 0
            && !self.iface.is_null()
            && let Some(f) = Value::call::<AddRelease>(&addr::GFX_OBJECT_ADD_REF)
        {
            // SAFETY: ObjectInterface::ObjectAddRef(Value*, data).
            unsafe { f(self.iface, &mut out, self.data) };
        }
        out
    }

    /// Move this value into a caller-provided `GFx::Value` slot (e.g. a function's
    /// return value), handing over its reference.
    ///
    /// # Safety
    /// `slot` must be a valid `GFx::Value` the caller owns; what it held is not released.
    pub unsafe fn write_to(self, slot: *mut Value) {
        // SAFETY: caller contract.
        unsafe { slot.write(self) };
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        if self.kind & kind::MANAGED != 0
            && !self.iface.is_null()
            && let Some(f) = Value::call::<AddRelease>(&addr::GFX_OBJECT_RELEASE)
        {
            // SAFETY: ObjectInterface::ObjectRelease(Value*, data) for our reference.
            unsafe { f(self.iface, self, self.data) };
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Value({}", self.type_name())?;
        if let Some(s) = self.as_string() {
            write!(f, " {s:?}")?;
        } else if let Some(n) = self.as_number() {
            write!(f, " {n}")?;
        } else if let Some(b) = self.as_bool() {
            write!(f, " {b}")?;
        }
        write!(f, ")")
    }
}

// ---------------------------------------------------------------- movies

/// `ASMovieRootBase` slots, from the generated tables: 0x2C, 0x2E, 0x2F, 0x30, 0x31, 0x32
/// and 0x39 in both builds (next-gen fork `GFx_ASMovieRootBase.h`:112; RE: the 1.11.240
/// `Movie::CreateString`/`CreateObject`/`CreateArray`/`CreateFunction`/`GetVariable`
/// wrappers, matched to 1.10.163's, use the same displacements, as do the
/// `SetVariable`/`Invoke` ones).
mod slot {
    use crate::layout::RE::Scaleform::GFx::ASMovieRootBase::vtbl;
    pub const CREATE_STRING: usize = vtbl::CreateString;
    pub const CREATE_OBJECT: usize = vtbl::CreateObject;
    pub const CREATE_ARRAY: usize = vtbl::CreateArray;
    pub const CREATE_FUNCTION: usize = vtbl::CreateFunction;
    pub const SET_VARIABLE: usize = vtbl::SetVariable;
    pub const GET_VARIABLE: usize = vtbl::GetVariable;
    /// `Invoke(path, Value* result, const Value* args, u32 count)`; kept literal because
    /// the tables name the two `Invoke` overloads by declaration order (`Invoke_2` = 0x39).
    pub const INVOKE: usize = 0x39;
}

/// A `GFx::Movie` (`GFxMovieView` in F4SE's API).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Movie(pub *mut c_void);

// SAFETY: a handle; callers use it on the movie's own thread.
unsafe impl Send for Movie {}

unsafe fn vcall<F: Copy>(this: *mut c_void, slot: usize) -> F {
    // SAFETY: caller contract: live polymorphic object with that slot.
    unsafe {
        let vtbl: *const usize = *(this as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(slot))
    }
}

impl Movie {
    /// `Movie::asMovieRoot`.
    fn root(self) -> Option<*mut c_void> {
        if self.0.is_null() || !mem::readable(self.0 as usize, 0x20) {
            return None;
        }
        // SAFETY: Ptr<ASMovieRootBase> at +0x18.
        let root: *mut c_void = unsafe { mem::read(self.0 as *const u8, crate::layout::RE::Scaleform::GFx::Movie::asMovieRoot) };
        (!root.is_null()).then_some(root)
    }

    #[must_use]
    pub fn get_variable(self, path: &str) -> Option<Value> {
        let root = self.root()?;
        let path = c(path);
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::GetVariable(Value*, const char*) const.
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void, *mut Value, *const c_char) -> bool>(root, slot::GET_VARIABLE)(root, &mut out, path.as_ptr()) }
            .then_some(out)
    }

    pub fn set_variable(self, path: &str, value: &Value) -> bool {
        let Some(root) = self.root() else { return false };
        let path = c(path);
        // SAFETY: ASMovieRootBase::SetVariable(const char*, const Value&, SetVarType = kSticky).
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void, *const c_char, *const Value, i32) -> bool>(root, slot::SET_VARIABLE)(root, path.as_ptr(), value, 1) }
    }

    #[must_use]
    pub fn create_object(self) -> Option<Value> {
        let root = self.root()?;
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::CreateObject(Value*, className, args, count).
        unsafe {
            vcall::<unsafe extern "C" fn(*mut c_void, *mut Value, *const c_char, *const Value, u32)>(root, slot::CREATE_OBJECT)(
                root,
                &mut out,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
        }
        out.is_object().then_some(out)
    }

    /// `new <class>(args...)`, e.g. `Date`.
    #[must_use]
    pub fn create_instance(self, class: &str, args: &[Value]) -> Option<Value> {
        let root = self.root()?;
        let class = c(class);
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::CreateObject(Value*, className, args, count).
        unsafe {
            vcall::<unsafe extern "C" fn(*mut c_void, *mut Value, *const c_char, *const Value, u32)>(root, slot::CREATE_OBJECT)(
                root,
                &mut out,
                class.as_ptr(),
                args.as_ptr(),
                args.len() as u32,
            );
        }
        out.is_object().then_some(out)
    }

    #[must_use]
    pub fn create_array(self) -> Option<Value> {
        let root = self.root()?;
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::CreateArray(Value*).
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void, *mut Value)>(root, slot::CREATE_ARRAY)(root, &mut out) };
        (out.kind() == kind::ARRAY).then_some(out)
    }

    /// A managed string owned by the movie.
    #[must_use]
    pub fn create_string(self, text: &str) -> Option<Value> {
        let root = self.root()?;
        let text = c(text);
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::CreateString(Value*, const char*).
        unsafe { vcall::<unsafe extern "C" fn(*mut c_void, *mut Value, *const c_char)>(root, slot::CREATE_STRING)(root, &mut out, text.as_ptr()) };
        (out.kind() == kind::STRING).then_some(out)
    }

    /// A function object backed by `handler` (see [`Function`]).
    #[must_use]
    pub fn create_function(self, handler: &'static Function, user: usize) -> Option<Value> {
        let root = self.root()?;
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::CreateFunction(Value*, FunctionHandler*, void* userData).
        unsafe {
            vcall::<unsafe extern "C" fn(*mut c_void, *mut Value, *const Function, usize)>(root, slot::CREATE_FUNCTION)(root, &mut out, handler, user);
        }
        // AS3 functions are objects (kind OBJECT), AS2 ones closures.
        out.is_object().then_some(out)
    }

    /// Call the function at `path` (e.g. `root.f4se.GetVersion`).
    #[must_use]
    pub fn invoke(self, path: &str, args: &[Value]) -> Option<Value> {
        let root = self.root()?;
        let path = c(path);
        let mut out = Value::undefined();
        // SAFETY: ASMovieRootBase::Invoke(const char*, Value* result, const Value* args, u32 count).
        unsafe {
            vcall::<unsafe extern "C" fn(*mut c_void, *const c_char, *mut Value, *const Value, u32) -> bool>(root, slot::INVOKE)(
                root,
                path.as_ptr(),
                &mut out,
                args.as_ptr(),
                args.len() as u32,
            )
        }
        .then_some(out)
    }
}

// ---------------------------------------------------------------- functions

/// One call of a Rust function from ActionScript.
pub struct Call<'a> {
    pub movie: Movie,
    pub this: Option<&'a Value>,
    pub args: &'a [Value],
    ret: *mut Value,
    pub user: usize,
}

impl Call<'_> {
    /// Set the return value.
    pub fn ret(&mut self, value: Value) {
        // SAFETY: the engine's return slot, live for the call.
        unsafe { value.write_to(self.ret) };
    }
}

pub type Handler = fn(&mut Call);

/// A `GFx::FunctionHandler` implemented in Rust. Create with [`Function::leak`]; it is
/// never freed (movies may keep it for the process).
#[repr(C)]
pub struct Function {
    vtbl: *const usize,
    refcount: i32,
    _pad: u32,
    handler: Handler,
}

// SAFETY: immutable after creation; the refcount is only touched by the engine.
unsafe impl Sync for Function {}
// SAFETY: as above.
unsafe impl Send for Function {}

/// `FunctionHandler::Params`.
#[repr(C)]
struct Params {
    ret: *mut Value,
    movie: *mut c_void,
    this: *mut Value,
    args_with_this: *mut Value,
    args: *mut Value,
    count: u32,
    _pad: u32,
    user: usize,
}

impl Function {
    #[must_use]
    pub fn leak(handler: Handler) -> &'static Function {
        static VTABLE: OnceLock<[usize; 2]> = OnceLock::new();
        let vtbl = VTABLE.get_or_init(|| [fn_dtor as *const () as usize, fn_call as *const () as usize]);
        Box::leak(Box::new(Function { vtbl: vtbl.as_ptr(), refcount: 1 << 24, _pad: 0, handler }))
    }
}

unsafe extern "C" fn fn_dtor(this: *mut Function, _flags: u32) -> *mut Function {
    this
}

unsafe extern "C" fn fn_call(this: *const Function, params: *const Params) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the engine's Params for this call.
        let (handler, p) = unsafe { ((*this).handler, &*params) };
        let args: &[Value] = if p.args.is_null() || p.count == 0 {
            &[]
        } else {
            // SAFETY: `count` values.
            unsafe { std::slice::from_raw_parts(p.args, p.count as usize) }
        };
        // SAFETY: live for the call.
        let this_value = unsafe { p.this.as_ref() };
        let mut call = Call { movie: Movie(p.movie), this: this_value, args, ret: p.ret, user: p.user };
        handler(&mut call);
    });
}
