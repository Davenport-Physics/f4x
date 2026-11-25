//! The patch map: which bytes of the game's code differ from `Fallout4.exe` on disk, and
//! where the jumps/calls written there lead (attributed to modules).
//!
//! The exe has a `.reloc` section; when it is mapped at its preferred base the mapped
//! code equals the file, so any difference is a patch. (At another base, relocated
//! pointers would differ too; those spots are excluded using the relocation table.)

use std::collections::HashSet;

use f4x_pe::{Image, MappedImage, PeFile};
use serde_json::{Value, json};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

pub fn map() -> Result<Value, String> {
    let mut buf = [0u16; 1024];
    // SAFETY: valid buffer; null = the game exe.
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let file = PeFile::parse(&bytes).map_err(|e| e.to_string())?;
    // SAFETY: the game exe is mapped for the process lifetime.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let mapped = unsafe { MappedImage::from_base(base) }.ok_or("game image headers")?;
    let delta = base as i64 - file.image_base() as i64;
    let mut relocated: HashSet<u32> = if delta != 0 {
        file.relocations().into_iter().flat_map(|r| r..r + 8).collect()
    } else {
        HashSet::new()
    };
    // The import address table is filled in by the loader: not a patch.
    if let Some((iat, size)) = file.iat_range() {
        relocated.extend(iat..iat + size);
    }

    let mut patches = Vec::new();
    for section in Image::sections(&file).iter().filter(|s| s.is_executable() || s.name == ".rdata") {
        let len = section.readable_size.min(section.virtual_size) as usize;
        // `.text` of a DRM-packed exe is encrypted on disk: compare with the copy taken
        // once the stub had decrypted it (see `f4x_engine::address`).
        let reference = (section.name == ".text")
            .then(f4x_engine::address::reference_text)
            .flatten()
            .filter(|(rva, _)| *rva == section.rva)
            .and_then(|(_, bytes)| bytes.get(..len));
        let (Some(disk), Some(memory)) = (reference.or_else(|| file.bytes(section.rva, len)), mapped.bytes(section.rva, len)) else {
            continue;
        };
        let mut i = 0;
        while i < disk.len() {
            if disk[i] == memory[i] || relocated.contains(&(section.rva + i as u32)) {
                i += 1;
                continue;
            }
            let start = i;
            while i < disk.len() && (disk[i] != memory[i] && !relocated.contains(&(section.rva + i as u32)) || (i + 1 < disk.len() && disk[i + 1] != memory[i + 1] && i - start < 64)) {
                i += 1;
            }
            let rva = section.rva + start as u32;
            let new = &memory[start..i.max(start + 1)];
            if section.is_executable() {
                patches.push(describe(section.name.as_str(), rva, &disk[start..i.max(start + 1)], new, base));
            } else {
                // Data: report changed pointer slots (vtable/function-pointer hooks).
                let first = (start & !7) as u32;
                let mut slot = first;
                while (slot as usize) < i {
                    let at = slot as usize;
                    if at + 8 <= memory.len() && memory[at..at + 8] != disk[at..at + 8] {
                        let target = u64::from_le_bytes(memory[at..at + 8].try_into().unwrap_or_default());
                        let frame = crate::crash::frame(target);
                        patches.push(json!({
                            "section": section.name, "rva": format!("{:#x}", section.rva + slot), "kind": "pointer",
                            "target": {"address": format!("{target:#x}"), "module": frame.module, "rva": frame.rva.map(|r| format!("{r:#x}"))},
                        }));
                    }
                    slot += 8;
                }
            }
            if patches.len() >= 500 {
                break;
            }
        }
    }
    let mut by_module: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for patch in &patches {
        let module = patch["target"]["via_trampoline"]["module"]
            .as_str()
            .or(patch["target"]["module"].as_str())
            .unwrap_or(if patch["target"].is_null() { "(data/immediate)" } else { "(trampoline)" });
        *by_module.entry(module.to_owned()).or_default() += 1;
    }
    Ok(json!({"exe": path, "base": format!("{base:#x}"), "count": patches.len(), "by_module": by_module, "patches": patches}))
}

fn describe(section: &str, rva: u32, old: &[u8], new: &[u8], base: usize) -> Value {
    let hex = |b: &[u8]| b.iter().take(16).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    let mut value = json!({
        "section": section,
        "rva": format!("{rva:#x}"),
        "len": new.len(),
        "old": hex(old),
        "new": hex(new),
    });
    // rel32 call/jmp written at the start: where does it go?
    let instruction_start = base + rva as usize;
    let target = match new {
        [0xE8 | 0xE9, a, b, c, d, ..] => Some(
            (instruction_start as i64 + 5 + i32::from_le_bytes([*a, *b, *c, *d]) as i64) as u64,
        ),
        [0xFF, 0x25, a, b, c, d, ..] => {
            let slot = (instruction_start as i64 + 6 + i32::from_le_bytes([*a, *b, *c, *d]) as i64) as usize;
            crate::crash::readable(slot as *const _, 8).then(|| unsafe { *(slot as *const u64) })
        }
        _ => None,
    };
    if let Some(target) = target {
        let kind = match new[0] {
            0xE8 => "call",
            0xE9 => "jmp",
            _ => "jmp [rip]",
        };
        value["kind"] = kind.into();
        let frame = crate::crash::frame(target);
        value["target"] = json!({"address": format!("{target:#x}"), "module": frame.module, "rva": frame.rva.map(|r| format!("{r:#x}"))});
        // A trampoline: follow one more jump if it is an absolute jmp.
        if frame.module.is_none() && crate::crash::readable(target as *const _, 14) {
            // SAFETY: readable (checked).
            let code = unsafe { std::slice::from_raw_parts(target as *const u8, 14) };
            if code[0] == 0xFF && code[1] == 0x25 && code[2..6] == [0, 0, 0, 0] {
                let next = u64::from_le_bytes(code[6..14].try_into().unwrap_or_default());
                let f = crate::crash::frame(next);
                value["target"]["via_trampoline"] = json!({"address": format!("{next:#x}"), "module": f.module, "rva": f.rva.map(|r| format!("{r:#x}"))});
            }
        }
    }
    value
}

/// Write a copy of `Fallout4.exe` whose `.text` is the code as it runs (the reference copy
/// taken once a DRM stub had decrypted it, with relocations undone), for our own offline
/// analysis of builds that ship encrypted. Everything else is the file on disk. The copy
/// is the user's game data: it goes to a local path only, never into the repository.
pub fn dump_image(out: &std::path::Path) -> Result<Value, String> {
    let mut buf = [0u16; 1024];
    // SAFETY: valid buffer; null = the game exe.
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let file = PeFile::parse(&bytes).map_err(|e| e.to_string())?;
    let (text_rva, text) = f4x_engine::address::reference_text().ok_or("no reference .text")?;
    let mut text = text.clone();
    // SAFETY: the game exe is mapped for the process lifetime.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let delta = (base as u64).wrapping_sub(file.image_base());
    if delta != 0 {
        for reloc in file.relocations() {
            let Some(at) = reloc.checked_sub(*text_rva).map(|o| o as usize).filter(|&o| o + 8 <= text.len()) else { continue };
            let value = u64::from_le_bytes(text[at..at + 8].try_into().unwrap_or_default()).wrapping_sub(delta);
            text[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
    }
    let start = file.raw_offset(*text_rva).ok_or(".text has no raw data")?;
    let section = Image::sections(&file).iter().find(|s| s.rva == *text_rva).ok_or("no .text section")?;
    let raw = (section.readable_size as usize).min(text.len());
    let mut copy = bytes.clone();
    copy[start..start + raw].copy_from_slice(&text[..raw]);
    std::fs::write(out, &copy).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(json!({"exe": path, "out": out.display().to_string(), "text_rva": format!("{text_rva:#x}"), "text_bytes": raw, "changed": bytes != copy}))
}
