//! `Data\F4SE\CustomControlMap.txt`: the user's own internal key mappings (F4SE
//! changelog 0.1.0: "customization of internal keymappings", same format as the game's
//! table).
//!
//! The game builds its `ControlMap` from a text table compiled into the exe: the
//! `ControlMap` constructor and the two reset-to-defaults paths each load the table's
//! address (`lea rdx, <text>`) and call or jump to `control_map_parse` (our disassembly
//! of 1.10.163: 0x1b278fb, 0x1b279e0, 0x1b282b0). When the file exists, f4x points those
//! three `lea`s at the file's text instead, so the file replaces the built-in table at
//! startup and on every reset. Black-box facts: `provenance/compat-features.jsonl`.

use std::ffi::c_void;

use f4x_engine_sys::og_1_10_163_addresses::CONTROL_MAP_PARSE;
use f4x_pe::Image;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::trampoline;

/// Columns the game's parser reads without checking: the name, three device columns and
/// three flag columns (our disassembly of `control_map_parse`: the flag columns' tokens
/// are dereferenced unchecked, so a shorter line crashes the game).
const REQUIRED_COLUMNS: usize = 7;

/// The parser reads lines into a 0x400-byte buffer; a longer line would be split, and its
/// tail parsed as a line of its own, so such lines are dropped too.
const MAX_LINE: usize = 0x3FF;

/// Turn every line the game's parser would crash on into a comment, keeping the line
/// count (blank lines separate the input contexts). The parser reads lines that start
/// with NUL, TAB, LF, CR or space as blank, skips lines starting with `//`, and cuts the
/// rest into tokens at TAB and CR. Returns the text and the 1-based numbers of the lines
/// commented out.
#[must_use]
pub(super) fn sanitize(text: &[u8]) -> (Vec<u8>, Vec<usize>) {
    let mut out = Vec::with_capacity(text.len() + 16);
    let mut dropped = Vec::new();
    for (i, line) in text.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        let blank = line.first().is_none_or(|&c| matches!(c, 0 | b'\t' | b'\n' | b'\r' | b' '));
        let comment = line.starts_with(b"//");
        let columns = line.split(|&b| b == b'\t' || b == b'\r').filter(|t| !t.is_empty()).count();
        if !blank && !comment && (columns < REQUIRED_COLUMNS || line.len() > MAX_LINE) {
            out.extend_from_slice(b"//");
            dropped.push(i + 1);
        }
        out.extend_from_slice(line);
    }
    (out, dropped)
}

/// `lea rdx, [rip + disp32]`.
const LEA_RDX: [u8; 3] = [0x48, 0x8D, 0x15];

/// The `lea rdx` sites that feed `parse` its text: within 10 bytes before each `call`/`jmp`
/// to it. Returns (site, current target).
fn table_sites(parse: usize) -> Vec<(usize, usize)> {
    let game = f4x_engine::game();
    let Some(text) = game.image().section(".text").cloned() else { return Vec::new() };
    let start = game.base() + text.rva as usize;
    // SAFETY: the mapped game code.
    let code = unsafe { std::slice::from_raw_parts(start as *const u8, text.readable_size as usize) };
    let mut sites = Vec::new();
    for i in 10..code.len().saturating_sub(5) {
        if !matches!(code[i], 0xE8 | 0xE9) {
            continue;
        }
        let rel = i32::from_le_bytes(code[i + 1..i + 5].try_into().unwrap_or_default());
        if (start + i + 5).wrapping_add_signed(rel as isize) != parse {
            continue;
        }
        for back in [7usize, 10] {
            let at = i - back;
            if code[at..at + 3] == LEA_RDX {
                let disp = i32::from_le_bytes(code[at + 3..at + 7].try_into().unwrap_or_default());
                sites.push((start + at, (start + at + 7).wrapping_add_signed(disp as isize)));
                break;
            }
        }
    }
    sites
}

pub(super) fn install() {
    let Some(path) =
        super::translations::game_dir().map(|d| d.join("Data").join("F4SE").join("CustomControlMap.txt"))
    else {
        return;
    };
    let Ok(mut text) = std::fs::read(&path) else { return };
    let Some(parse) = f4x_engine::address::resolve(&CONTROL_MAP_PARSE) else {
        log::warn!(target: "f4x::f4se", "control map parser not found; {} ignored", path.display());
        return;
    };
    let sites = table_sites(parse);
    // Every site must load the same (built-in) table.
    if sites.is_empty() || sites.iter().any(|s| s.1 != sites[0].1) {
        log::warn!(target: "f4x::f4se", "control map table references not recognised ({sites:x?}); {} ignored", path.display());
        return;
    }
    text.retain(|&b| b != 0);
    let (mut text, dropped) = sanitize(&text);
    if !dropped.is_empty() {
        log::warn!(target: "f4x::f4se", "{}: lines {dropped:?} have fewer than {REQUIRED_COLUMNS} columns or are too long; ignored (they would crash the game's parser)", path.display());
    }
    text.push(0);
    let game = f4x_engine::game();
    let buffer = trampoline::BRANCH
        .lock()
        .map(|mut pool| pool.allocate(game.base(), text.len()))
        .unwrap_or(std::ptr::null_mut()) as usize;
    if buffer == 0 {
        return;
    }
    // SAFETY: our fresh buffer near the image, then each `lea`'s displacement in game
    // code (made writable for the write).
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer as *mut u8, text.len());
        for &(site, _) in &sites {
            let Ok(rel) = i32::try_from(buffer as isize - (site as isize + 7)) else { continue };
            let mut old: PAGE_PROTECTION_FLAGS = 0;
            VirtualProtect((site + 3) as *const c_void, 4, PAGE_EXECUTE_READWRITE, &mut old);
            ((site + 3) as *mut i32).write_unaligned(rel);
            VirtualProtect((site + 3) as *const c_void, 4, old, &mut old);
            FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 7);
        }
    }
    log::info!(target: "f4x::f4se", "{} replaces the built-in control map ({} bytes, {} sites)", path.display(), text.len() - 1, sites.len());
}
