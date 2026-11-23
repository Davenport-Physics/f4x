//! Papyrus values of every type, read from and written to `BSScript::Variable`s:
//! primitives, forms and script objects, `Var`, arrays and structs.
//!
//! Layouts (CommonLibF4 `RE/Bethesda/BSScript.h`):
//! - `Array`: element `TypeInfo` at +0x08, `BSTArray<Variable>` at +0x18 (data, capacity
//!   at +0x20, size at +0x28);
//! - `Struct`: type at +0x10, variables at +0x20;
//! - `StructTypeInfo`: name at +0x10, variables (`StructVar`, 0x28 bytes, `varType` at
//!   +0x10) at +0x20, `varNameIndexMap` (`BSTHashMap<BSFixedString, u32>`) at +0x38.
//!
//! A `TypeInfo` is a raw type (0..=7, arrays 11..=17) or an `IComplexType*` (object or
//! struct type, bit 0 set for arrays of it).

use std::ffi::c_void;
use std::mem::transmute;
use std::ptr::null_mut;

use f4x_engine_sys::og_1_10_163_addresses as addr;

use crate::address::resolve;
use crate::mem;
use crate::papyrus::{FixedString, ScriptObject, Variable, Vm, object_handle, raw, slot, vcall};

/// A Papyrus value.
#[derive(Clone, Debug, PartialEq)]
pub enum PValue {
    None,
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
    /// A form; written as a script object of the declared type.
    Form(*mut u8),
    /// A script object that is not a form (an alias, a plain script instance).
    Object { handle: u64, script: String },
    Array(Vec<PValue>),
    /// Fields in declaration order.
    Struct(Vec<(String, PValue)>),
    Var(Box<PValue>),
}

// SAFETY: forms and script objects are engine objects; values are moved between the VM
// and our code on the threads the VM uses.
unsafe impl Send for PValue {}
// SAFETY: as above.
unsafe impl Sync for PValue {}

const ARRAY_OFFSET: usize = raw::ARRAY_START as usize;

fn complex_raw(complex: usize) -> u32 {
    if !mem::readable(complex, 0x18) {
        return raw::NONE;
    }
    // SAFETY: IComplexType vtable slot 1: GetRawType().
    unsafe { vcall::<unsafe extern "C" fn(usize) -> u32>(complex as *mut c_void, 1)(complex) }
}

/// The name of an object or struct type (`name` at +0x10 in both).
fn complex_name(complex: usize) -> Option<String> {
    // SAFETY: an IComplexType (ObjectTypeInfo / StructTypeInfo).
    unsafe { mem::fixed_string(mem::read::<*const u8>(complex as *const u8, 0x10)) }
}

/// A `TypeInfo` from a type name as scripts write it: `Int`, `form[]`,
/// `instancedata#owner`, `Var[]`, ...
pub fn type_info(vm: Vm, name: &str) -> Option<usize> {
    let (base, array) = match name.strip_suffix("[]") {
        Some(b) => (b, true),
        None => (name, false),
    };
    let primitive = match base.to_ascii_lowercase().as_str() {
        "none" | "" => Some(raw::NONE),
        "bool" => Some(raw::BOOL),
        "int" => Some(raw::INT),
        "float" => Some(raw::FLOAT),
        "string" => Some(raw::STRING),
        "var" => Some(raw::VAR),
        _ => None,
    };
    if let Some(p) = primitive {
        return Some(p as usize + if array && p != raw::NONE { ARRAY_OFFSET } else { 0 });
    }
    let complex = if base.contains('#') { vm.struct_type(base)? } else { vm.object_type(base)? };
    Some(complex | usize::from(array))
}

/// The element type of an array `TypeInfo`, if it is one.
fn element_type(kind: usize) -> Option<usize> {
    if kind >= raw::ARRAY_END as usize {
        (kind & 1 != 0).then_some(kind & !1)
    } else {
        ((raw::ARRAY_START as usize + 1)..raw::ARRAY_END as usize).contains(&kind).then(|| kind - ARRAY_OFFSET)
    }
}

/// Read a variable (no references are taken).
pub fn read(vm: Vm, var: &Variable) -> PValue {
    read_depth(vm, var, 0)
}

/// As [`read`], but every script object stays a [`PValue::Object`] with its own script
/// (the attached script, `MyQuestScript`, rather than the form's base type `Quest`).
pub fn read_exact(vm: Vm, var: &Variable) -> PValue {
    EXACT.with(|e| e.set(true));
    let value = read_depth(vm, var, 0);
    EXACT.with(|e| e.set(false));
    value
}

thread_local! {
    static EXACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn read_depth(vm: Vm, var: &Variable, depth: u32) -> PValue {
    if depth > 8 || var.kind == 0 {
        return PValue::None;
    }
    if element_type(var.kind).is_some() {
        return read_array(vm, var.value as *const u8, depth);
    }
    if var.kind < raw::ARRAY_END as usize {
        return match var.kind as u32 {
            raw::BOOL => PValue::Bool(var.value as u8 != 0),
            raw::INT => PValue::Int(var.value as u32 as i32),
            raw::FLOAT => PValue::Float(f32::from_bits(var.value as u32)),
            raw::STRING => PValue::String(var.as_string()),
            raw::OBJECT => read_object(vm, var.value as *mut ScriptObject),
            raw::VAR => {
                let inner = var.value as *const Variable;
                if inner.is_null() || !mem::readable(inner as usize, 16) {
                    PValue::None
                } else {
                    // SAFETY: a heap Variable owned by this one.
                    PValue::Var(Box::new(read_depth(vm, unsafe { &*inner }, depth + 1)))
                }
            }
            _ => PValue::None,
        };
    }
    match complex_raw(var.kind) {
        raw::OBJECT => read_object(vm, var.value as *mut ScriptObject),
        raw::STRUCT => read_struct(vm, var.value as *const u8, var.kind, depth),
        _ => PValue::None,
    }
}

fn read_object(vm: Vm, object: *mut ScriptObject) -> PValue {
    if object.is_null() {
        return PValue::None;
    }
    if !EXACT.with(std::cell::Cell::get)
        && let Some((_, game)) = vm.game_object(object)
        && crate::Form::checked(game).is_some()
    {
        return PValue::Form(game);
    }
    PValue::Object {
        handle: object_handle(object).unwrap_or(0),
        script: crate::papyrus::object_script_name(object).unwrap_or_default(),
    }
}

fn read_array(vm: Vm, array: *const u8, depth: u32) -> PValue {
    if array.is_null() || !mem::readable(array as usize, 0x30) {
        return PValue::None;
    }
    // SAFETY: a live BSScript::Array.
    let (data, size): (*const Variable, u32) = unsafe { (mem::read(array, 0x18), mem::read(array, 0x28)) };
    if data.is_null() || size > 1 << 16 {
        return PValue::Array(Vec::new());
    }
    // SAFETY: `size` elements.
    PValue::Array((0..size as usize).map(|i| read_depth(vm, unsafe { &*data.add(i) }, depth + 1)).collect())
}

/// Field names and indices of a struct type, in index order.
fn struct_fields(struct_type: usize) -> Vec<(String, u32, *const u8)> {
    let map = struct_type + 0x38;
    let mut out = Vec::new();
    // SAFETY: StructTypeInfo::varNameIndexMap (BSTHashMap: capacity +0x0C, entries +0x28,
    // entries {BSFixedString key, u32 value, next} of 0x18 bytes).
    unsafe {
        let capacity: u32 = mem::read(map as *const u8, 0x0C);
        let entries: *const u8 = mem::read(map as *const u8, 0x28);
        if entries.is_null() || capacity > 4096 {
            return out;
        }
        for i in 0..capacity as usize {
            let e = entries.add(i * 0x18);
            if mem::read::<usize>(e, 0x10) == 0 {
                continue;
            }
            let key: *const u8 = mem::read(e, 0);
            out.push((mem::fixed_string(key).unwrap_or_default(), mem::read::<u32>(e, 8), key));
        }
    }
    out.sort_by_key(|f| f.1);
    out
}

fn read_struct(vm: Vm, st: *const u8, struct_type: usize, depth: u32) -> PValue {
    if st.is_null() || !mem::readable(st as usize, 0x20) {
        return PValue::None;
    }
    let fields = struct_fields(struct_type);
    PValue::Struct(
        fields
            .into_iter()
            .map(|(name, index, _)| {
                // SAFETY: Struct::variables[index].
                let var = unsafe { &*(st.add(0x20) as *const Variable).add(index as usize) };
                (name, read_depth(vm, var, depth + 1))
            })
            .collect(),
    )
}

/// The declared type of field `index` of a struct type.
fn struct_field_type(struct_type: usize, index: u32) -> usize {
    // SAFETY: StructTypeInfo::variables (BSTArray<StructVar>, 0x28-byte entries).
    unsafe {
        let data: *const u8 = mem::read(struct_type as *const u8, 0x20);
        if data.is_null() {
            return 0;
        }
        mem::read(data.add(index as usize * 0x28), 0x10)
    }
}

/// Write `value` into a fresh variable declared as `kind`. The variable takes ownership
/// of any references created.
pub fn write(vm: Vm, var: &mut Variable, kind: usize, value: &PValue) -> Result<(), String> {
    var.kind = 0;
    var.value = 0;
    // A Var-typed slot holds any value, boxed.
    if kind == raw::VAR as usize && !matches!(value, PValue::Var(_) | PValue::None) {
        return write_var(vm, var, value);
    }
    match value {
        PValue::None => Ok(()),
        PValue::Bool(b) => {
            var.set_bool(*b);
            Ok(())
        }
        PValue::Int(i) => {
            if kind == raw::FLOAT as usize {
                var.set_float(*i as f32);
            } else {
                var.set_int(*i);
            }
            Ok(())
        }
        PValue::Float(f) => {
            var.set_float(*f);
            Ok(())
        }
        PValue::String(s) => {
            var.set_string(s);
            Ok(())
        }
        PValue::Form(form) => {
            let script = if kind >= raw::ARRAY_END as usize { complex_name(kind & !1) } else { None };
            let f = crate::Form::checked(*form).ok_or("not a form")?;
            let script = match script {
                Some(s) => s,
                None => vm.script_for_type(u32::from(f.form_type().0)).ok_or("no script type for the form")?,
            };
            let object = vm.object_for_form(*form, f.form_type(), &script).ok_or_else(|| format!("no {script} object for the form"))?;
            var.set_object(object);
            Ok(())
        }
        PValue::Object { handle, script } => {
            let object = vm.bound_object(*handle, script).ok_or("no bound object")?;
            var.set_object(object);
            Ok(())
        }
        PValue::Array(items) => {
            let elem = element_type(kind).ok_or("not an array type")?;
            let array = vm.create_array(elem, items.len() as u32).ok_or("CreateArray failed")?;
            // SAFETY: the new array's element storage (`items.len()` None variables).
            let data: *mut Variable = unsafe { mem::read(array as *const u8, 0x18) };
            for (i, item) in items.iter().enumerate() {
                // SAFETY: within the array's size.
                write(vm, unsafe { &mut *data.add(i) }, elem, item)?;
            }
            var.kind = kind;
            var.value = array as u64;
            Ok(())
        }
        PValue::Struct(fields) => {
            let st_type = if kind >= raw::ARRAY_END as usize && kind & 1 == 0 { kind } else { return Err("not a struct type".into()) };
            let name = complex_name(st_type).ok_or("unnamed struct type")?;
            let st = vm.create_struct(&name).ok_or("CreateStruct failed")?;
            let known = struct_fields(st_type);
            for (fname, fvalue) in fields {
                let key = FixedString::new(fname).map(|s| s.as_raw() as *const u8);
                let Some(&(_, index, _)) = known.iter().find(|(_, _, k)| Some(*k) == key) else {
                    return Err(format!("{name} has no field {fname}"));
                };
                // SAFETY: Struct::variables[index].
                let slot = unsafe { &mut *(st.add(0x20) as *mut Variable).add(index as usize) };
                slot.reset();
                write(vm, slot, struct_field_type(st_type, index), fvalue)?;
            }
            var.kind = st_type;
            var.value = st as u64;
            Ok(())
        }
        PValue::Var(inner) => write_var(vm, var, inner),
    }
}

/// A heap `Variable` holding `inner` (game allocator: the VM frees it).
fn write_var(vm: Vm, var: &mut Variable, inner: &PValue) -> Result<(), String> {
    let singleton = resolve(&addr::MEMORY_MANAGER_SINGLETON).ok_or("MemoryManager not found")?;
    let allocate = resolve(&addr::MEMORY_MANAGER_ALLOCATE).ok_or("MemoryManager::Allocate not found")?;
    // SAFETY: MemoryManager::GetSingleton / Allocate(this, size, alignment, alignmentRequired).
    let heap = unsafe {
        let manager = transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(singleton)();
        transmute::<usize, unsafe extern "C" fn(*mut c_void, usize, u32, bool) -> *mut Variable>(allocate)(manager, 16, 8, false)
    };
    if heap.is_null() {
        return Err("allocation failed".into());
    }
    // SAFETY: fresh 16-byte allocation.
    let boxed = unsafe {
        heap.write(Variable { kind: 0, value: 0 });
        &mut *heap
    };
    match inner {
        // An empty `Var` (F4SE's conversion of null/undefined/objects from Scaleform reads
        // as None in Papyrus): the box holds a None variable.
        PValue::None => {}
        // `Var(Array([]))` is a `Var` holding a None `Var[]`, as F4SE's
        // `Utility.VarArrayToVar` of an empty array gives.
        PValue::Array(items) if items.is_empty() => boxed.kind = raw::VAR as usize + ARRAY_OFFSET,
        _ => {
            let kind = natural_type(vm, inner);
            write(vm, boxed, kind, inner)?;
        }
    }
    var.kind = raw::VAR as usize;
    var.value = heap as u64;
    Ok(())
}

/// The type a value has on its own (for boxing into `Var`).
fn natural_type(vm: Vm, value: &PValue) -> usize {
    match value {
        PValue::Bool(_) => raw::BOOL as usize,
        PValue::Int(_) => raw::INT as usize,
        PValue::Float(_) => raw::FLOAT as usize,
        PValue::String(_) => raw::STRING as usize,
        PValue::Form(f) => crate::Form::checked(*f)
            .and_then(|f| vm.script_for_type(u32::from(f.form_type().0)))
            .and_then(|s| vm.object_type(&s))
            .unwrap_or(raw::OBJECT as usize),
        PValue::Object { script, .. } => vm.object_type(script).unwrap_or(raw::OBJECT as usize),
        // An array of `Var` when its elements are; else of its first element's type.
        PValue::Array(items) => match items.first() {
            None | Some(PValue::Var(_)) => raw::VAR as usize + ARRAY_OFFSET,
            Some(first) => match natural_type(vm, first) {
                k if k == raw::NONE as usize => raw::VAR as usize + ARRAY_OFFSET,
                k if k < raw::ARRAY_START as usize => k + ARRAY_OFFSET,
                k if k >= raw::ARRAY_END as usize && k & 1 == 0 => k | 1,
                _ => raw::VAR as usize + ARRAY_OFFSET,
            },
        },
        _ => raw::NONE as usize,
    }
}

impl Vm {
    /// The `StructTypeInfo*` for `script#struct`, loading the script if needed.
    #[must_use]
    pub fn struct_type(self, name: &str) -> Option<usize> {
        let name = FixedString::new(name)?;
        let mut info: *mut c_void = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, *const FixedString, *mut *mut c_void) -> bool;
        // SAFETY: GetScriptStructType(const BSFixedString&, BSTSmartPointer<StructTypeInfo>&).
        let ok = unsafe { vcall::<F>(self.0, slot::GET_SCRIPT_STRUCT_TYPE)(self.0, &name, &mut info) };
        (ok && !info.is_null()).then_some(info as usize)
    }

    /// A new array of `count` None elements of `element` type (one reference).
    #[must_use]
    pub fn create_array(self, element: usize, count: u32) -> Option<*mut u8> {
        let mut out: *mut u8 = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, *const usize, u32, *mut *mut u8) -> bool;
        // SAFETY: CreateArray(const TypeInfo&, u32, BSTSmartPointer<Array>&).
        let ok = unsafe { vcall::<F>(self.0, slot::CREATE_ARRAY)(self.0, &element, count, &mut out) };
        (ok && !out.is_null()).then_some(out)
    }

    /// A new struct of type `script#struct` (one reference).
    #[must_use]
    pub fn create_struct(self, name: &str) -> Option<*mut u8> {
        let name = FixedString::new(name)?;
        let mut out: *mut u8 = null_mut();
        type F = unsafe extern "C" fn(*mut c_void, *const FixedString, *mut *mut u8) -> bool;
        // SAFETY: CreateStruct(const BSFixedString&, BSTSmartPointer<Struct>&).
        let ok = unsafe { vcall::<F>(self.0, slot::CREATE_STRUCT)(self.0, &name, &mut out) };
        (ok && !out.is_null()).then_some(out)
    }
}
