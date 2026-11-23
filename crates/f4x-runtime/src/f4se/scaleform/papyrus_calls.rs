//! `root.f4se.CallFunctionNoWait(object, function, args...)` and
//! `CallGlobalFunctionNoWait(script, function, args...)`: queue a Papyrus call from
//! ActionScript ("Scaleform to Papyrus adapters", F4SE 0.3.1).
//!
//! Black-box (F4SE 0.6.23, sessions 1795438622 and 1795439564: a test script whose
//! functions take each parameter type, called with every kind of AS value, with the
//! VM's type checks and `Debug.Trace` of the values in the Papyrus log;
//! `provenance/scaleform-features.jsonl`):
//! - `object` is a Papyrus object as Scaleform holds one (`{__type__, __handleHigh__,
//!   __handleLow__}`, see `UI.Set`); the call goes to the object of type `__type__` bound
//!   to that handle, and returns true once queued, false when there is no such object.
//!   The function is looked up by the VM (case-insensitive); for a missing one it still
//!   returns true and the Papyrus log gets "error: Failed to call member function F on
//!   [S < (formid)>] - function name likely invalid, or object incorrect type";
//! - `CallGlobalFunctionNoWait` returns true whenever `script` and `function` are
//!   strings; when the VM refuses the call (missing script, or not a global function) the
//!   Papyrus log gets "error: Failed to call global function S.F - script or function
//!   name likely invalid" and an empty stack;
//! - arguments, with no regard for the parameter types (the VM then reports "Type
//!   mismatch" for those that differ):
//!   - Number -> Float (integral values too), int and uint -> Int, boolean -> Bool,
//!     string -> String;
//!   - array -> `Var[]`, each element converted the same way and boxed (null -> None);
//!   - a Papyrus object -> that object, typed as its own script;
//!   - display object -> None (the VM warns "Passing NONE to non-object argument");
//!   - null, undefined, other objects and functions -> an empty `Var` (known difference:
//!     F4SE's reads as None; f4x's `papyrus_value` boxes an empty `Var` as a None `Var[]`,
//!     which reads as `[]`; the VM accepts it for exactly the same parameters);
//! - F4SE crashes when `script` is not a string or when too few arguments are given;
//!   f4x returns false.

use f4x_engine::papyrus::{Arg, Vm};
use f4x_engine::papyrus_value::PValue;
use f4x_engine::scaleform::{Call, Value, kind};

const TYPE_MEMBER: &str = "__type__";
const HANDLE_HIGH: &str = "__handleHigh__";
const HANDLE_LOW: &str = "__handleLow__";

fn vm() -> Option<Vm> {
    f4x_agent::observe::virtual_machine().map(|v| Vm(v as *mut _))
}

/// A Papyrus object held by Scaleform: (handle, script).
fn papyrus_object(v: &Value) -> Option<(u64, String)> {
    if v.kind() != kind::OBJECT || !v.has_member(TYPE_MEMBER) {
        return None;
    }
    let script = v.get_member(TYPE_MEMBER)?.as_string()?;
    let half = |name: &str| v.get_member(name).and_then(|h| h.as_number()).map_or(0, |n| n as u32);
    Some(((u64::from(half(HANDLE_HIGH)) << 32) | u64::from(half(HANDLE_LOW)), script))
}

fn empty_var() -> Arg {
    Arg::Typed("Var".into(), PValue::Var(Box::new(PValue::None)))
}

/// An array element, before boxing.
fn element(vm: Vm, v: &Value, depth: u32) -> PValue {
    match v.kind() {
        kind::BOOL => PValue::Bool(v.as_bool().unwrap_or(false)),
        kind::INT | kind::UINT => PValue::Int(v.as_number().unwrap_or(0.0) as i64 as i32),
        kind::NUMBER => PValue::Float(v.as_number().unwrap_or(0.0) as f32),
        kind::STRING => PValue::String(v.as_string().unwrap_or_default()),
        kind::ARRAY if depth < 8 => array(vm, v, depth + 1),
        // Only objects that exist: writing an unknown one would fail the whole call.
        kind::OBJECT => papyrus_object(v)
            .filter(|(handle, script)| f4x_engine::scaleform_ext::bound_object_exists(vm, *handle, script))
            .map_or(PValue::None, |(handle, script)| PValue::Object { handle, script }),
        _ => PValue::None,
    }
}

fn array(vm: Vm, v: &Value, depth: u32) -> PValue {
    let n = v.length().unwrap_or(0).min(1 << 12);
    // A None element stays a bare None in its `Var` slot (it reads as None, as under F4SE).
    let boxed = |e: PValue| if e == PValue::None { e } else { PValue::Var(Box::new(e)) };
    PValue::Array((0..n).map(|i| boxed(v.element(i).map_or(PValue::None, |e| element(vm, &e, depth)))).collect())
}

/// A Scaleform value as F4SE passes it to Papyrus.
fn argument(vm: Vm, v: &Value) -> Arg {
    match v.kind() {
        kind::BOOL => Arg::Bool(v.as_bool().unwrap_or(false)),
        kind::INT | kind::UINT => Arg::Int(v.as_number().unwrap_or(0.0) as i64 as i32),
        kind::NUMBER => Arg::Float(v.as_number().unwrap_or(0.0) as f32),
        kind::STRING => Arg::String(v.as_string().unwrap_or_default()),
        kind::ARRAY => Arg::Typed("Var[]".into(), array(vm, v, 0)),
        kind::DISPLAY_OBJECT => Arg::None,
        // The bound object's reference goes to the argument.
        kind::OBJECT => papyrus_object(v).and_then(|(handle, script)| vm.bound_object(handle, &script)).map_or_else(empty_var, Arg::Object),
        _ => empty_var(),
    }
}

pub(super) fn call_function_no_wait(call: &mut Call) {
    let target = call.args.first().and_then(papyrus_object);
    let function = call.args.get(1).and_then(Value::as_string);
    let (Some((handle, script)), Some(function), Some(vm)) = (target, function, vm()) else {
        call.ret(Value::bool(false));
        return;
    };
    let args: Vec<Arg> = call.args[2..].iter().map(|v| argument(vm, v)).collect();
    let mut queued = vm.dispatch_method_call(handle, &script, &function, args);
    if !queued && f4x_engine::scaleform_ext::bound_object_exists(vm, handle, &script) {
        // The object exists but the VM refused the call (no such method): log it as the
        // game does and report success, as F4SE does.
        let object = f4x_engine::scaleform_ext::handle_string(vm, handle).unwrap_or_default();
        post_error(vm, &format!("Failed to call member function {function} on [{script} <{object}>] - function name likely invalid, or object incorrect type"));
        queued = true;
    }
    log::debug!(target: "f4x::scaleform", "CallFunctionNoWait({script} {handle:#x}, {function}) -> {queued}");
    call.ret(Value::bool(queued));
}

pub(super) fn call_global_function_no_wait(call: &mut Call) {
    let script = call.args.first().and_then(Value::as_string);
    let function = call.args.get(1).and_then(Value::as_string);
    let (Some(script), Some(function), Some(vm)) = (script, function, vm()) else {
        call.ret(Value::bool(false));
        return;
    };
    let args: Vec<Arg> = call.args[2..].iter().map(|v| argument(vm, v)).collect();
    let queued = vm.call_static(&script, &function, args).is_some();
    if !queued {
        post_error(vm, &format!("Failed to call global function {script}.{function} - script or function name likely invalid"));
    }
    log::debug!(target: "f4x::scaleform", "CallGlobalFunctionNoWait({script}.{function}) -> queued {queued}");
    call.ret(Value::bool(true));
}

/// `BSScript::ICachedErrorMessage` (CommonLibF4 `BSScript.h`): dtor, `GetErrorMsg(BSFixedString&)`.
#[repr(C)]
struct CachedError {
    vtbl: *const CachedErrorVtbl,
    text: String,
}

#[repr(C)]
struct CachedErrorVtbl {
    dtor: unsafe extern "C" fn(*mut CachedError, u32) -> *mut CachedError,
    get_error_msg: unsafe extern "C" fn(*const CachedError, *mut *mut u8),
}

unsafe extern "C" fn cached_error_dtor(this: *mut CachedError, _flags: u32) -> *mut CachedError {
    this
}

unsafe extern "C" fn cached_error_get(this: *const CachedError, out: *mut *mut u8) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: our message; `out` is the engine's BSFixedString, assigned here.
        unsafe {
            let Some(text) = f4x_engine::papyrus::FixedString::new(&(*this).text) else { return };
            let old = out.read();
            out.write(text.into_raw());
            drop(f4x_engine::papyrus::FixedString::from_raw(old));
        }
    });
}

static CACHED_ERROR_VTBL: CachedErrorVtbl = CachedErrorVtbl { dtor: cached_error_dtor, get_error_msg: cached_error_get };

/// The VM's error log (`PostCachedErrorToLogger(message, stackID, kError)`, vtable slot
/// 0x3C), as the game reports a failed global call: the message, then an empty stack.
fn post_error(vm: Vm, text: &str) {
    const POST_CACHED_ERROR_WITH_STACK: usize = 0x3C;
    const SEVERITY_ERROR: u32 = 2;
    let message = CachedError { vtbl: &CACHED_ERROR_VTBL, text: text.to_owned() };
    // SAFETY: IVirtualMachine::PostCachedErrorToLogger(const ICachedErrorMessage&, u32 stackID,
    // Severity) on the live VM; the message lives for the call.
    unsafe {
        let vtbl: *const usize = *(vm.0 as *const *const usize);
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *const CachedError, u32, u32) = std::mem::transmute(*vtbl.add(POST_CACHED_ERROR_WITH_STACK));
        f(vm.0.cast(), &message, 0, SEVERITY_ERROR);
    }
}
