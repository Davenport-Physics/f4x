//! The version text on the main menu and in the pause menu's settings, with the
//! extender's version next to the game's.
//!
//! The game passes its version string to the menu movie's `SetVersionText` from
//! `StartMenuBase::ProcessMessage` (vtable slot 3, the `IMenu::ProcessMessage` slot in
//! CommonLibF4 `RE/Bethesda/IMenu.h`): `lea rax, "1.10.163.0"` then
//! `lea r9, "SetVersionText"` (our disassembly of 1.10.163, 0xbbc503). F4SE 0.6.23
//! rewrites that `lea` (black-box: the patch map shows its displacement changed) so the
//! text becomes its own string. f4x does the same with its own version, so the menu says
//! which extender is running (see [`text`]).

use std::ffi::{CStr, c_void};

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::trampoline;

const START_MENU_BASE: &str = ".?AVStartMenuBase@@";
/// Same on NG 1.11.240: `StartMenuBase`'s slot 3 (0xb01320) is the function that uses
/// "SetVersionText" (f4x-vtcmp; next-gen fork `IMenu.h`).
const PROCESS_MESSAGE_SLOT: usize = f4x_engine::layout::RE::IMenu::vtbl::ProcessMessage;
/// How far into `ProcessMessage` to look for the two `lea`s.
const SCAN: usize = 0x200;
/// How far before the `SetVersionText` `lea` the version string's `lea` may be.
const VERSION_LEA_WINDOW: usize = 0x80;

/// A game version string such as `1.10.163.0`.
fn is_version(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit()) && s.contains('.') && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The string f4x passes: F4SE's format (black-box: the menu shows
/// `v (F4SE 0.6.23 rel 25) 1.10.163.0.0`, the movie adding `v` and `.<n>`) with f4x's
/// name and version, e.g. `(f4x 0.0.1) 1.10.163.0`.
#[must_use]
pub(super) fn text(game_version: &str) -> String {
    format!("(f4x {}) {game_version}", env!("CARGO_PKG_VERSION"))
}

/// `lea r64, [rip + disp32]`: (register, target) at `at`.
fn lea_target(code: &[u8], at: usize, address: usize) -> Option<(u8, usize)> {
    let b = code.get(at..at + 7)?;
    if b[0] & 0xFB != 0x48 || b[1] != 0x8D || b[2] & 0xC7 != 0x05 {
        return None;
    }
    let reg = ((b[2] >> 3) & 7) | if b[0] & 4 != 0 { 8 } else { 0 };
    let disp = i32::from_le_bytes([b[3], b[4], b[5], b[6]]);
    Some((reg, (address + at + 7).wrapping_add_signed(disp as isize)))
}

fn c_string_at(address: usize, max: usize) -> Option<String> {
    if !f4x_engine::mem::readable(address, max) {
        return None;
    }
    // SAFETY: readable (checked); bounded scan for the terminator.
    let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, max) };
    let end = bytes.iter().position(|&b| b == 0)?;
    CStr::from_bytes_with_nul(&bytes[..=end]).ok().map(|s| s.to_string_lossy().into_owned())
}

pub(super) fn install() {
    let game = f4x_engine::game();
    let Some(vtable) = game.vtable(START_MENU_BASE) else {
        log::warn!(target: "f4x::f4se", "StartMenuBase not found; no version text");
        return;
    };
    // SAFETY: a vtable in the game image.
    let function = unsafe { *(vtable as *const usize).add(PROCESS_MESSAGE_SLOT) };
    if !f4x_engine::mem::readable(function, SCAN + 7) {
        return;
    }
    // SAFETY: readable game code (checked).
    let code = unsafe { std::slice::from_raw_parts(function as *const u8, SCAN + 7) };
    // `lea r9, "SetVersionText"`, then the nearest `lea` before it that loads the version
    // string. 1.10.163: `lea rax, "1.10.163.0"` right before it (0xbbc503); NG 1.11.240:
    // `lea rax, "1.11.240.0"` 0x3d bytes before it (0xb01370; our disassembly).
    let site = (0..SCAN)
        .find(|&j| {
            lea_target(code, j, function)
                .is_some_and(|(reg, t)| reg == 9 && c_string_at(t, 16).as_deref() == Some("SetVersionText"))
        })
        .and_then(|j| {
            (j.saturating_sub(VERSION_LEA_WINDOW)..j).rev().find_map(|i| {
                let (_, version) = lea_target(code, i, function)?;
                c_string_at(version, 32).is_some_and(|s| is_version(&s)).then_some((i, version))
            })
        });
    let Some((offset, version_address)) = site else {
        log::warn!(target: "f4x::f4se", "SetVersionText call not found; no version text");
        return;
    };
    let Some(game_version) = c_string_at(version_address, 32) else { return };
    let text = text(&game_version);
    let buffer = trampoline::BRANCH
        .lock()
        .map(|mut pool| pool.allocate(game.base(), text.len() + 1))
        .unwrap_or(std::ptr::null_mut());
    if buffer.is_null() {
        return;
    }
    let site = function + offset;
    let rel = buffer as isize - (site as isize + 7);
    let Ok(rel) = i32::try_from(rel) else { return };
    // SAFETY: our own fresh buffer near the image; the `lea`'s displacement in game code,
    // made writable for the write.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer.cast::<u8>(), text.len());
        *buffer.cast::<u8>().add(text.len()) = 0;
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        VirtualProtect((site + 3) as *const c_void, 4, PAGE_EXECUTE_READWRITE, &mut old);
        ((site + 3) as *mut i32).write_unaligned(rel);
        VirtualProtect((site + 3) as *const c_void, 4, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 7);
    }
    log::info!(target: "f4x::f4se", "version text: {text:?} (lea at {:#x})", site - game.base());
}
