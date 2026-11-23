//! Reading the engine's `ControlMap` (CommonLibF4 `RE/Bethesda/ControlMap.h`): per input
//! context (`UserEvents::INPUT_CONTEXT_ID`, 33) and device (keyboard, mouse, gamepad),
//! the user events with their keys.
//!
//! The game builds the map from a text table compiled into the exe (our analysis of
//! 1.10.163: the `ControlMap` constructor at 0x1b27830 passes the text at 0x2e0b7e0 to
//! its parser, `control_map_parse`; lines are `name<TAB>keyboard<TAB>mouse<TAB>gamepad
//! <TAB>remappable...<TAB>flags`).

use crate::controls::control_map;
use crate::layout::RE;
use crate::mem::{self, BSTArray};

/// `UserEvents::INPUT_CONTEXT_ID` names, in order (CommonLibF4 `UserEvents.h`).
pub const CONTEXTS: [&str; 33] = [
    "MainGameplay",
    "BasicMenuNav",
    "ThumbNav",
    "VirtualController",
    "Cursor",
    "LThumbCursor",
    "Console",
    "DebugText",
    "Book",
    "DebugOverlay",
    "TFC",
    "DebugMap",
    "Lockpick",
    "VATS",
    "VATSPlayback",
    "MultiActivate",
    "Workshop",
    "Scope",
    "SitWait",
    "LooksMenu",
    "WorkshopAddendum",
    "PauseMenu",
    "LevelupMenu",
    "LevelupMenuPrevNext",
    "MainMenu",
    "QuickContainerMenu",
    "SpecialActivateRollover",
    "TwoButtonRollover",
    "QuickContainerMenuPerk",
    "VertiBird",
    "PlayBinkMenu",
    "RobotModAddendum",
    "CreationClub",
];

/// `INPUT_DEVICE` names for the three mapped devices.
pub const DEVICES: [&str; 3] = ["keyboard", "mouse", "gamepad"];

/// One `ControlMap::UserEventMapping`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub event: String,
    pub key: i32,
    pub index: i8,
    pub remappable: bool,
    pub linked: bool,
    pub flags: i32,
}

/// The mappings of one context and device (`None`: the context has no table).
#[must_use]
pub fn mappings(context: usize, device: usize) -> Option<Vec<Mapping>> {
    use RE::ControlMap::UserEventMapping as M;
    let map = control_map()?;
    if context >= CONTEXTS.len() || device >= DEVICES.len() {
        return None;
    }
    // SAFETY: a live ControlMap; controlMaps is InputContext*[33].
    let input_context: *const u8 = unsafe { mem::read(map, RE::ControlMap::controlMaps + context * 8) };
    if input_context.is_null() || !mem::readable(input_context as usize, RE::ControlMap::InputContext::SIZE) {
        return None;
    }
    // SAFETY: InputContext: BSTArray<UserEventMapping>[3].
    let array: BSTArray<[u8; M::SIZE]> = unsafe {
        BSTArray::at(input_context.add(RE::ControlMap::InputContext::deviceMappings + device * 0x18))
    };
    let mut out = Vec::with_capacity(array.len());
    for raw in array.iter() {
        let p = raw.as_ptr();
        // SAFETY: a copied UserEventMapping; eventID is a live BSFixedString entry.
        unsafe {
            out.push(Mapping {
                event: mem::fixed_string(mem::read(p, M::eventID)).unwrap_or_default(),
                key: mem::read(p, M::inputKey),
                index: mem::read(p, M::byIndexInContext),
                remappable: mem::read::<u8>(p, M::remappable) != 0,
                linked: mem::read::<u8>(p, M::linked) != 0,
                flags: mem::read(p, M::userEventGroupFlag),
            });
        }
    }
    Some(out)
}

/// One `ControlMap::LinkedMapping`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub name: String,
    pub context: i32,
    pub device: i32,
    pub from_context: i32,
    pub from_name: String,
}

/// `ControlMap::linkedMappings`.
#[must_use]
pub fn linked_mappings() -> Vec<Linked> {
    use RE::ControlMap::LinkedMapping as L;
    let Some(map) = control_map() else { return Vec::new() };
    // SAFETY: a live ControlMap.
    let array: BSTArray<[u8; L::SIZE]> = unsafe { BSTArray::at(map.add(RE::ControlMap::linkedMappings)) };
    array
        .iter()
        .map(|raw| {
            let p = raw.as_ptr();
            // SAFETY: a copied LinkedMapping with live BSFixedString entries.
            unsafe {
                Linked {
                    name: mem::fixed_string(mem::read(p, L::linkedMappingName)).unwrap_or_default(),
                    context: mem::read(p, L::linkedMappingContext),
                    device: mem::read(p, L::device),
                    from_context: mem::read(p, L::linkFromContext),
                    from_name: mem::fixed_string(mem::read(p, L::linkFromName)).unwrap_or_default(),
                }
            }
        })
        .collect()
}
