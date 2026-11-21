//! Low-level patching helpers: IAT slots, vtable slots, absolute jumps.

use std::ffi::{CStr, c_void};

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Replace the pointer at `slot` (in a read-only section) and return the old value.
///
/// # Safety
/// `slot` must point to a pointer-sized, mapped location whose new value is safe to use.
pub unsafe fn patch_pointer(slot: *mut usize, new: usize) -> Result<usize, String> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(slot.cast::<c_void>(), 8, PAGE_READWRITE, &mut old) == 0 {
            return Err(format!("VirtualProtect({slot:p}) failed"));
        }
        let previous = slot.read();
        slot.write(new);
        VirtualProtect(slot.cast::<c_void>(), 8, old, &mut old);
        Ok(previous)
    }
}

/// Overwrite the start of a function with `jmp [rip+0]; dq target` (14 bytes). The
/// original code is lost: only for functions that are never called through again.
///
/// # Safety
/// `at` must be the start of a function at least 14 bytes long.
pub unsafe fn write_abs_jmp(at: *mut u8, target: usize) -> Result<(), String> {
    let mut code = [0u8; 14];
    code[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
    code[6..].copy_from_slice(&(target as u64).to_le_bytes());
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(at.cast(), code.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return Err(format!("VirtualProtect({at:p}) failed"));
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(), at, code.len());
        VirtualProtect(at.cast(), code.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), at.cast(), code.len());
    }
    Ok(())
}

/// Replace the IAT slot for `dll!function` in the image at `base`; returns the bound
/// target it held.
///
/// # Safety
/// `base` must be a mapped PE32+ image whose imports are bound.
pub unsafe fn iat_hook(base: *mut u8, dll: &str, function: &str, replacement: usize) -> Result<usize, String> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let slot = iat_slot(base, dll, function)?;
        patch_pointer(slot, replacement)
    }
}

/// # Safety
/// See [`iat_hook`].
pub unsafe fn iat_slot(base: *mut u8, dll: &str, function: &str) -> Result<*mut usize, String> {
    // SAFETY: offsets come from the image's own headers.
    unsafe {
        let nt = base.add(base.add(0x3C).cast::<i32>().read_unaligned() as usize);
        if nt.cast::<u32>().read_unaligned() != 0x4550 {
            return Err("bad PE signature".into());
        }
        let import_rva = nt.add(24 + 112 + 8).cast::<u32>().read_unaligned() as usize;
        if import_rva == 0 {
            return Err("no imports".into());
        }
        let mut descriptor = base.add(import_rva);
        loop {
            let lookup = descriptor.cast::<u32>().read_unaligned() as usize;
            let name_rva = descriptor.add(12).cast::<u32>().read_unaligned() as usize;
            let iat = descriptor.add(16).cast::<u32>().read_unaligned() as usize;
            if name_rva == 0 {
                return Err(format!("image does not import from {dll}"));
            }
            if CStr::from_ptr(base.add(name_rva).cast()).to_string_lossy().eq_ignore_ascii_case(dll) {
                let lookup = if lookup != 0 { lookup } else { iat };
                for i in 0.. {
                    let entry = base.add(lookup).cast::<u64>().add(i).read_unaligned();
                    if entry == 0 {
                        break;
                    }
                    if entry >> 63 == 0 && CStr::from_ptr(base.add(entry as usize + 2).cast()).to_bytes() == function.as_bytes() {
                        return Ok(base.add(iat).cast::<usize>().add(i));
                    }
                }
                return Err(format!("{dll} does not export {function} to this image"));
            }
            descriptor = descriptor.add(20);
        }
    }
}
