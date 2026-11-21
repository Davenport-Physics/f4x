//! Spike S3: catalog every native function the engine binds into the Papyrus VM.
//!
//! The catalog is a *map* of engine functionality: it records which C++ callback
//! backs each native so the analyzer can find the engine functions underneath. Nothing
//! here ever calls a native.
//!
//! Hook point: `BSScript::Internal::VirtualMachine`'s vtable, found by RTTI name, slot
//! 0x1B = `IVirtualMachine::BindNativeMethod(IFunction*)` (CommonLibF4 `BSScript.h`).
//! `NativeFunctionBase` fields (same header): `name`@0x10, `objName`@0x18,
//! `stateName`@0x20, `retType`@0x28, `descTable { entries@0x30, paramCount: u16@0x38 }`,
//! `isStatic`@0x40, `isLatent`@0x42; size 0x50. Descriptor entries are
//! `(BSFixedString name, TypeInfo type)` pairs (16 bytes). The pointer-sized field right
//! after the base (0x50) is recorded as the callback candidate; spike S3 verifies it.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_pe::rtti::{RttiIndex, class_info};
use f4x_pe::{Image, MappedImage};
use serde_json::{Value, json};
use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};

use crate::engine::{fixed_string, type_info};
use crate::session;

const BIND_NATIVE_METHOD_SLOT: usize = 0x1B;
const VM_CLASS: &str = ".?AVVirtualMachine@Internal@BSScript@@";

static REAL_BIND: AtomicUsize = AtomicUsize::new(0);
static IMAGE: std::sync::OnceLock<MappedImage> = std::sync::OnceLock::new();
static RECORDS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

type BindFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool;

pub fn count() -> usize {
    RECORDS.lock().map(|r| r.len()).unwrap_or(0)
}

/// Install the hook. Called from `DllMain`, before the game creates its VM.
pub fn install(base: usize) {
    // SAFETY: `base` is the game exe, mapped for the life of the process.
    let Some(image) = (unsafe { MappedImage::from_base(base) }) else {
        session::log("catalog: could not read game image headers");
        return;
    };
    let started = std::time::Instant::now();
    let index = RttiIndex::scan(&image);
    let Some(vtable) = index.primary_vtable(VM_CLASS) else {
        session::log("catalog: VirtualMachine vtable not found");
        return;
    };
    session::log(&format!(
        "catalog: RTTI scan {} classes in {:?}; VirtualMachine vtable rva {vtable:#x}",
        index.type_descriptors.len(),
        started.elapsed()
    ));
    let slot = (base + vtable as usize + 8 * BIND_NATIVE_METHOD_SLOT) as *mut usize;
    // SAFETY: `slot` is inside the VM vtable in .rdata; we make it writable briefly.
    unsafe {
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(slot.cast(), 8, PAGE_READWRITE, &mut old) == 0 {
            session::log("catalog: VirtualProtect failed");
            return;
        }
        REAL_BIND.store(slot.read(), Relaxed);
        slot.write(bind_hook as BindFn as usize);
        VirtualProtect(slot.cast(), 8, old, &mut old);
    }
    let _ = IMAGE.set(image);
    session::log("catalog: hooked IVirtualMachine::BindNativeMethod");
}

unsafe extern "C" fn bind_hook(vm: *mut c_void, function: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the VM passes a live IFunction.
        if let Some(record) = unsafe { describe(function) }
            && let Ok(mut records) = RECORDS.lock()
        {
            records.push(record);
        }
    });
    // SAFETY: REAL_BIND holds the original slot value, set before the hook went live.
    unsafe {
        let real: BindFn = std::mem::transmute(REAL_BIND.load(Relaxed));
        real(vm, function)
    }
}

/// # Safety
/// `function` must point to a live `IFunction`.
unsafe fn describe(function: *mut c_void) -> Option<Value> {
    let image = IMAGE.get()?;
    let base = image.base() as usize;
    let f = function as *const u8;
    // SAFETY: reading fields of a live NativeFunctionBase-derived object (caller).
    unsafe {
        let vtable = f.cast::<usize>().read();
        let class = class_info(image, (vtable - base) as u32).map(|c| c.mangled_name);
        let read_str = |off: usize| fixed_string(f.add(off).cast::<*const u8>().read_unaligned());
        let entries = f.add(0x30).cast::<*const u8>().read_unaligned();
        let param_count = f.add(0x38).cast::<u16>().read_unaligned() as usize;
        let mut params = Vec::with_capacity(param_count);
        for i in 0..param_count.min(64) {
            let entry = entries.add(16 * i);
            params.push(json!({
                "name": fixed_string(entry.cast::<*const u8>().read_unaligned()),
                "type": type_info(entry.add(8).cast::<u64>().read_unaligned()),
            }));
        }
        let callback = f.add(0x50).cast::<usize>().read_unaligned();
        let callback_rva = callback.checked_sub(base).filter(|&r| r < image.size()).map(|r| format!("{r:#x}"));
        Some(json!({
            "object": read_str(0x18),
            "name": read_str(0x10),
            "state": read_str(0x20),
            "returns": type_info(f.add(0x28).cast::<u64>().read_unaligned()),
            "params": params,
            "static": f.add(0x40).read() != 0,
            "latent": f.add(0x42).read() != 0,
            "class": class,
            "vtable_rva": format!("{:#x}", vtable - base),
            "callback_rva": callback_rva,
        }))
    }
}

/// Write the catalog (JSON Lines) into the session directory.
pub fn write(file: &str) -> usize {
    let Some(session) = session::get() else { return 0 };
    let Ok(records) = RECORDS.lock() else { return 0 };
    let mut text = String::with_capacity(records.len() * 256);
    for record in records.iter() {
        text.push_str(&record.to_string());
        text.push('\n');
    }
    session.write_atomic(file, &text);
    records.len()
}
