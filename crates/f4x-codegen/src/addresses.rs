//! `addresses`: resolve wanted non-virtual functions and derive signature recipes.
//!
//! For each entry of `crates/f4x-engine-sys/wanted.txt`:
//! 1. the RVA comes from a locally installed Address Library bin (a hint; the bin's
//!    data is never written anywhere in the repo);
//! 2. the RVA must be a function start (`.pdata` or a leaf);
//! 3. instructions from the RVA are decoded (iced-x86) and the bytes of relative branch
//!    targets and RIP-relative displacements are masked; bytes are added until the masked
//!    pattern matches exactly once in `.text`.
//!
//! Output: `crates/f4x-engine-sys/src/og_1_10_163_addresses.rs` and
//! `provenance/addresses.jsonl`.

use std::fmt::Write as _;
use std::path::Path;

use f4x_pe::{Image, PeFile};
use iced_x86::{Decoder, DecoderOptions, FlowControl};

pub struct Wanted {
    pub name: String,
    /// A CommonLibF4 `REL::ID` (resolved through the Address Library hint) ...
    pub id: u64,
    /// ... or an RVA we found ourselves (`rva:0x...` in the list).
    pub rva: Option<u32>,
    pub source: String,
    pub description: String,
}

pub fn parse_wanted(text: &str) -> Vec<Wanted> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let name = parts.next()?.to_owned();
            let id_field = parts.next()?;
            let (id, rva) = match id_field.strip_prefix("rva:") {
                Some(hex) => (0, Some(u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok()?)),
                None => (id_field.parse().ok()?, None),
            };
            let source = parts.next()?.to_owned();
            let description = parts.collect::<Vec<_>>().join(" ");
            Some(Wanted { name, id, rva, source, description })
        })
        .collect()
}

/// The Address Library bin: u64 count, then sorted (u64 id, u64 rva) pairs
/// (CommonLibF4 `REL::IDDatabase`).
pub fn bin_lookup(bin: &[u8], id: u64) -> Option<u64> {
    let count = u64::from_le_bytes(bin.get(..8)?.try_into().ok()?) as usize;
    let pairs = bin.get(8..8 + count * 16)?;
    let (mut lo, mut hi) = (0usize, count);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let entry = &pairs[mid * 16..mid * 16 + 16];
        let key = u64::from_le_bytes(entry[..8].try_into().ok()?);
        match key.cmp(&id) {
            std::cmp::Ordering::Equal => return Some(u64::from_le_bytes(entry[8..].try_into().ok()?)),
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    None
}

pub struct Signature {
    pub bytes: Vec<u8>,
    pub mask: Vec<u8>,
}

/// Masked bytes of the instructions starting at `rva`, grown until unique in `.text`.
pub fn signature(pe: &PeFile, rva: u32) -> Result<Signature, String> {
    let text = pe.section(".text").ok_or("no .text")?.clone();
    let text_bytes = pe.section_bytes(&text);
    let start = (rva - text.rva) as usize;
    let code = text_bytes.get(start..).ok_or("rva outside .text")?;
    let mut decoder = Decoder::with_ip(64, &code[..code.len().min(512)], pe.image_base() + rva as u64, DecoderOptions::NONE);
    let (mut bytes, mut mask) = (Vec::new(), Vec::new());
    for instruction in &mut decoder {
        if instruction.is_invalid() {
            break;
        }
        let at = (instruction.ip() - pe.image_base() - rva as u64) as usize;
        let len = instruction.len();
        let raw = &code[at..at + len];
        let mut keep = vec![1u8; len];
        let offsets = decoder_offsets(&instruction, raw);
        for (offset, size) in offsets {
            for k in offset..offset + size {
                keep[k] = 0;
            }
        }
        bytes.extend_from_slice(raw);
        mask.extend_from_slice(&keep);
        if bytes.len() >= 12 && count_matches(text_bytes, &bytes, &mask, 2) == 1 {
            return Ok(Signature { bytes, mask });
        }
        if bytes.len() >= 96 || matches!(instruction.flow_control(), FlowControl::Return) {
            break;
        }
    }
    Err(format!("no unique signature within {} bytes", bytes.len()))
}

/// Byte ranges of an instruction that move when code or data is relocated: relative
/// branch targets and RIP-relative displacements.
fn decoder_offsets(instruction: &iced_x86::Instruction, raw: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut decoder = Decoder::new(64, raw, DecoderOptions::NONE);
    let decoded = decoder.decode();
    let offsets = decoder.get_constant_offsets(&decoded);
    if instruction.is_ip_rel_memory_operand() && offsets.has_displacement() {
        out.push((offsets.displacement_offset(), offsets.displacement_size()));
    }
    let relative_branch = matches!(
        instruction.flow_control(),
        FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch
    ) && instruction.near_branch_target() != 0;
    if relative_branch && offsets.has_immediate() {
        out.push((offsets.immediate_offset(), offsets.immediate_size()));
    } else if relative_branch && raw.len() >= 5 && matches!(raw[0], 0xE8 | 0xE9) {
        out.push((1, 4));
    } else if relative_branch && raw.len() >= 6 && raw[0] == 0x0F && (0x80..=0x8F).contains(&raw[1]) {
        out.push((2, 4));
    }
    out
}

/// How many times the masked pattern occurs in `haystack` (stops counting at `limit`).
pub fn count_matches(haystack: &[u8], bytes: &[u8], mask: &[u8], limit: usize) -> usize {
    let anchor = mask.iter().position(|&m| m != 0).unwrap_or(0);
    let first = bytes[anchor];
    let mut count = 0;
    let mut i = 0;
    while i + bytes.len() <= haystack.len() {
        if haystack[i + anchor] == first
            && bytes.iter().zip(mask).enumerate().all(|(k, (&b, &m))| m == 0 || haystack[i + k] == b)
        {
            count += 1;
            if count >= limit {
                return count;
            }
        }
        i += 1;
    }
    count
}

pub fn run(bin_path: &Path, exe_path: &Path, wanted_path: &Path, out_rs: &Path, out_provenance: &Path) -> Result<(), String> {
    let bin = std::fs::read(bin_path).map_err(|e| format!("{}: {e}", bin_path.display()))?;
    let exe = std::fs::read(exe_path).map_err(|e| format!("{}: {e}", exe_path.display()))?;
    let pe = PeFile::parse(&exe).map_err(|e| e.to_string())?;
    let ranges = pe.function_ranges();
    let wanted = parse_wanted(&std::fs::read_to_string(wanted_path).map_err(|e| e.to_string())?);
    let mut rs = String::from(
        "//! Generated by `f4x-codegen addresses` (see `crates/f4x-engine-sys/wanted.txt`). Do not edit.\n\
         //! Fallout 4 1.10.163: RVAs plus masked signatures unique in `.text`.\n\
         #![allow(clippy::all)]\n\nuse crate::Address;\n\n",
    );
    let mut provenance = String::new();
    let mut failures = Vec::new();
    for w in &wanted {
        let Some(rva) = w.rva.or_else(|| bin_lookup(&bin, w.id).map(|r| r as u32)) else {
            failures.push(format!("{}: id {} not in the bin", w.name, w.id));
            continue;
        };
        if !pe.is_function_start(&ranges, rva) {
            failures.push(format!("{}: {rva:#x} is not a function start", w.name));
            continue;
        }
        let sig = match signature(&pe, rva) {
            Ok(sig) => sig,
            Err(e) => {
                failures.push(format!("{}: {e}", w.name));
                continue;
            }
        };
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(", ");
        if w.rva.is_some() {
            let _ = writeln!(rs, "/// {} ({})", w.description, w.source);
        } else {
            let _ = writeln!(rs, "/// {} (CommonLibF4 `REL::ID({})`, {})", w.description, w.id, w.source);
        }
        let _ = writeln!(
            rs,
            "pub const {}: Address = Address {{ name: {:?}, rva: {rva:#x}, signature: &[{}], mask: &[{}] }};\n",
            w.name.to_ascii_uppercase(),
            w.name,
            hex(&sig.bytes),
            sig.mask.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
        );
        let pattern: String = sig.bytes.iter().zip(&sig.mask).map(|(b, m)| if *m == 0 { "??".into() } else { format!("{b:02X}") }).collect::<Vec<_>>().join(" ");
        let _ = writeln!(
            provenance,
            "{}",
            serde_json::json!({
                "id": format!("og/fn/{}", w.name), "build": "og-1.10.163", "value": format!("{rva:#x}"),
                "recipe": format!("signature: {pattern}"),
                "sources": if w.rva.is_some() { serde_json::json!([
                    {"kind": "bb", "notes": format!("{} (return addresses on the stack when F4SE 0.6.23 sends the message; docs/formats/cosave.md)", w.source)},
                    {"kind": "re", "tool": "f4x-codegen addresses", "notes": format!("{}; function start; signature unique in .text", w.description)}
                ]) } else { serde_json::json!([
                    {"kind": "clf4", "ref": format!("REL::ID({})", w.id), "file": w.source, "symbol": w.description},
                    {"kind": "al-hint", "notes": "located with the user's local Address Library bin; not committed"},
                    {"kind": "re", "tool": "f4x-codegen addresses", "notes": "function start; signature unique in .text"}
                ]) }
            })
        );
        println!("{:<32} {rva:#010x}  {} bytes  {}", w.name, sig.bytes.len(), pattern);
    }
    std::fs::write(out_rs, rs).map_err(|e| e.to_string())?;
    std::fs::write(out_provenance, provenance).map_err(|e| e.to_string())?;
    for f in &failures {
        eprintln!("failed: {f}");
    }
    if failures.is_empty() { Ok(()) } else { Err(format!("{} address(es) failed", failures.len())) }
}
