//! Import address table hooks on the in-memory game image.

use std::ffi::{CStr, c_void};
use std::ptr;

use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};

const IMPORT_DIRECTORY: usize = 1;

/// Replace the IAT slot for `dll!function` in the module at `base` with `replacement`.
/// Returns the previous slot value (the real function).
///
/// # Safety
/// `base` must be the base of a mapped PE32+ image whose imports are already bound.
pub unsafe fn hook(base: *mut u8, dll: &str, function: &str, replacement: usize) -> Result<usize, String> {
    // SAFETY: the caller guarantees a mapped PE32+ image at `base`; all offsets come
    // from its own headers.
    unsafe {
        let slot = find_slot(base, dll, function)?;
        let previous = slot.read();
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(slot.cast::<c_void>(), 8, PAGE_READWRITE, &mut old) == 0 {
            return Err(format!("VirtualProtect on IAT slot for {dll}!{function} failed"));
        }
        slot.write(replacement);
        VirtualProtect(slot.cast::<c_void>(), 8, old, &mut old);
        Ok(previous)
    }
}

/// # Safety
/// See [`hook`].
unsafe fn find_slot(base: *mut u8, dll: &str, function: &str) -> Result<*mut usize, String> {
    // SAFETY: see `hook`.
    unsafe {
        let e_lfanew = base.add(0x3C).cast::<i32>().read_unaligned() as usize;
        let nt = base.add(e_lfanew);
        if nt.cast::<u32>().read_unaligned() != 0x0000_4550 {
            return Err("bad PE signature".into());
        }
        let optional = nt.add(4 + 20);
        if optional.cast::<u16>().read_unaligned() != 0x20B {
            return Err("not a PE32+ image".into());
        }
        let directory = optional.add(112 + 8 * IMPORT_DIRECTORY);
        let import_rva = directory.cast::<u32>().read_unaligned() as usize;
        if import_rva == 0 {
            return Err("image has no imports".into());
        }

        let mut descriptor = base.add(import_rva);
        loop {
            let original_first_thunk = descriptor.cast::<u32>().read_unaligned() as usize;
            let name_rva = descriptor.add(12).cast::<u32>().read_unaligned() as usize;
            let first_thunk = descriptor.add(16).cast::<u32>().read_unaligned() as usize;
            if name_rva == 0 {
                break;
            }
            let name = CStr::from_ptr(base.add(name_rva).cast());
            if name.to_string_lossy().eq_ignore_ascii_case(dll) {
                let lookup = if original_first_thunk != 0 { original_first_thunk } else { first_thunk };
                let mut index = 0;
                loop {
                    let entry = base.add(lookup).cast::<u64>().add(index).read_unaligned();
                    if entry == 0 {
                        break;
                    }
                    if entry & (1 << 63) == 0 {
                        let by_name = base.add(entry as usize + 2);
                        if CStr::from_ptr(by_name.cast()).to_bytes() == function.as_bytes() {
                            return Ok(base.add(first_thunk).cast::<usize>().add(index));
                        }
                    }
                    index += 1;
                }
                return Err(format!("{dll} does not import {function}"));
            }
            descriptor = descriptor.add(20);
        }
        let _ = ptr::null::<u8>();
        Err(format!("image does not import from {dll}"))
    }
}
