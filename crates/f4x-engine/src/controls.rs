//! `ControlMap` (CommonLibF4 `RE/Bethesda/ControlMap.h`): the input contexts and the
//! text-entry counter that turns keyboard input into text instead of controls.

use f4x_engine_sys::addresses::CONTROL_MAP_SINGLETON;

use crate::address::resolve;
use crate::layout::RE;
use crate::mem;

/// The `ControlMap` singleton, once created.
#[must_use]
pub fn control_map() -> Option<*mut u8> {
    let global = resolve(&CONTROL_MAP_SINGLETON)?;
    // SAFETY: the ControlMap* global.
    let map: *mut u8 = unsafe { mem::read(global as *const u8, 0) };
    (!map.is_null()).then_some(map)
}

/// `ControlMap::byTextEntryCount`: while above zero, keys go to text input.
#[must_use]
pub fn text_entry_count() -> Option<i8> {
    // SAFETY: a live ControlMap.
    control_map().map(|m| unsafe { mem::read::<i8>(m, RE::ControlMap::byTextEntryCount) })
}

/// Raise (`true`) or lower (`false`, not below zero) the text-entry count.
pub fn allow_text_input(allow: bool) -> Option<i8> {
    let map = control_map()?;
    // SAFETY: a live ControlMap; a one-byte counter.
    unsafe {
        let p = map.add(RE::ControlMap::byTextEntryCount).cast::<i8>();
        if allow {
            *p = (*p).saturating_add(1);
        } else if *p > 0 {
            *p -= 1;
        }
        Some(*p)
    }
}
