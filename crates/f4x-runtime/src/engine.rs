//! Minimal engine-structure readers used by the spikes. These move into `f4x-engine`
//! once the type database exists.
//!
//! Layouts from CommonLibF4 (MIT), commit 67173f8:
//! - `BSStringPool::Entry` (`RE/Bethesda/BSStringPool.h`): `_left`@0x00, `_flags: u16`@0x08
//!   (`kShallow = 1<<14`, `kWide = 1<<15`), `_length: u32` / `_right: Entry*`@0x10,
//!   text follows the 0x18-byte entry. `BSFixedString` is one `Entry*`.
//! - `BSScript::TypeInfo::RawType` (`RE/Bethesda/BSScript.h`): `kNone..kStruct` = 0..7,
//!   arrays `kArrayStart(10)+1 ..= kArrayStruct(17)`, `kArrayEnd = 18`; larger values are
//!   `IComplexType*` (low bit: array), whose `name` sits at +0x10 (`ObjectTypeInfo`).

use std::ffi::CStr;

const SHALLOW: u16 = 1 << 14;
const WIDE: u16 = 1 << 15;

/// Read a `BSFixedString` (a pointer to a string-pool entry).
///
/// # Safety
/// `entry` must be null or point to a live `BSStringPool::Entry`.
pub unsafe fn fixed_string(mut entry: *const u8) -> Option<String> {
    if entry.is_null() {
        return Some(String::new());
    }
    // SAFETY: the caller guarantees a live entry; shallow entries chain to live entries.
    unsafe {
        for _ in 0..16 {
            let flags = entry.add(8).cast::<u16>().read_unaligned();
            if flags & SHALLOW == 0 {
                if flags & WIDE != 0 {
                    return None;
                }
                return Some(CStr::from_ptr(entry.add(0x18).cast()).to_string_lossy().into_owned());
            }
            entry = entry.add(0x10).cast::<*const u8>().read_unaligned();
            if entry.is_null() {
                return None;
            }
        }
    }
    None
}

const SIMPLE_TYPES: [&str; 18] = [
    "None", "Object", "String", "Int", "Float", "Bool", "Var", "Struct", "?8", "?9", "?10", "Object[]",
    "String[]", "Int[]", "Float[]", "Bool[]", "Var[]", "Struct[]",
];

/// Render a `BSScript::TypeInfo` raw value.
///
/// # Safety
/// For complex types, `raw & !1` must point to a live `IComplexType`.
pub unsafe fn type_info(raw: u64) -> String {
    if raw < SIMPLE_TYPES.len() as u64 {
        return SIMPLE_TYPES[raw as usize].to_owned();
    }
    let array = raw & 1 != 0;
    let complex = (raw & !1) as *const u8;
    // SAFETY: guaranteed by the caller.
    let name = unsafe { fixed_string(complex.add(0x10).cast::<*const u8>().read_unaligned()) }
        .unwrap_or_else(|| format!("<complex {raw:#x}>"));
    if array { format!("{name}[]") } else { name }
}
