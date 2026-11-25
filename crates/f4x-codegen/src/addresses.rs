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
//!
//! Next-gen (`--flavor ng`, [`run_ng`]): there is no Address Library. Step 1 is replaced
//! by `crates/f4x-engine-sys/wanted-ng_1_11_240.txt` (our own RVAs, one per `wanted.txt`
//! entry, with the evidence); steps 2-3 are the same. Output:
//! `crates/f4x-engine-sys/src/ng_1_11_240_addresses.rs` and
//! `provenance/addresses-ng_1_11_240.jsonl`.

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
    /// A global (data), not a function (`global:<id>` in the list).
    pub global: bool,
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
            let mut id_field = parts.next()?;
            let global = id_field.starts_with("global:");
            if global {
                id_field = &id_field["global:".len()..];
            }
            let (id, rva) = match id_field.strip_prefix("rva:") {
                Some(hex) => (0, Some(u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok()?)),
                None => (id_field.parse().ok()?, None),
            };
            let source = parts.next()?.to_owned();
            let description = parts.collect::<Vec<_>>().join(" ");
            Some(Wanted { name, id, rva, global, source, description })
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
/// A signature for a global: an instruction in `.text` that references `target`
/// RIP-relatively, grown until unique. Returns (site RVA, signature, displacement offset
/// within the instruction, instruction length).
pub fn global_signature(pe: &PeFile, target: u32) -> Result<(u32, Signature, u8, u8), String> {
    let text = pe.section(".text").ok_or("no .text")?.clone();
    let bytes = pe.section_bytes(&text);
    let base = pe.image_base();
    // Candidate sites: any 4-byte window whose value would be a RIP-relative disp to target.
    let mut tried = 0;
    for i in 0..bytes.len().saturating_sub(8) {
        // Decode instructions that could contain a disp32 at i: try instruction starts
        // a few bytes before the displacement.
        for back in 2..=4usize {
            let Some(start) = i.checked_sub(back) else { continue };
            let mut decoder = Decoder::with_ip(64, &bytes[start..(start + 15).min(bytes.len())], base + (text.rva as usize + start) as u64, DecoderOptions::NONE);
            let instruction = decoder.decode();
            if instruction.is_invalid() || !instruction.is_ip_rel_memory_operand() {
                continue;
            }
            if instruction.ip_rel_memory_address() != base + target as u64 {
                continue;
            }
            let offsets = decoder.get_constant_offsets(&instruction);
            if offsets.displacement_offset() != back {
                continue;
            }
            tried += 1;
            let site = text.rva + start as u32;
            if let Ok(sig) = signature(pe, site) {
                return Ok((site, sig, back as u8, instruction.len() as u8));
            }
            if tried > 512 {
                return Err("no unique signature at any of 512 referencing instructions".into());
            }
        }
    }
    Err("no RIP-relative reference found".into())
}

/// A signature for a function whose own bytes are not unique (e.g. identical copies):
/// a direct `call rel32` to it, grown until unique. Returns (site RVA, signature); the
/// target is `site + 5 + rel32` (the same resolution as a [`global_signature`]).
pub fn call_signature(pe: &PeFile, target: u32) -> Result<(u32, Signature), String> {
    let text = pe.section(".text").ok_or("no .text")?.clone();
    let bytes = pe.section_bytes(&text);
    let mut tried = 0;
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] != 0xE8 {
            continue;
        }
        let rel = i32::from_le_bytes(bytes[i + 1..i + 5].try_into().unwrap());
        let site = text.rva + i as u32;
        if (site as i64 + 5 + rel as i64) != target as i64 {
            continue;
        }
        tried += 1;
        if let Ok(sig) = signature(pe, site) {
            return Ok((site, sig));
        }
        if tried > 64 {
            break;
        }
    }
    Err(format!("no unique signature at the function or at any of {tried} call sites"))
}

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
         #![allow(clippy::all)]\n\nuse crate::{Address, GlobalRef};\n\n",
    );
    let mut provenance = String::new();
    let mut failures = Vec::new();
    for w in &wanted {
        let Some(rva) = w.rva.or_else(|| bin_lookup(&bin, w.id).map(|r| r as u32)) else {
            failures.push(format!("{}: id {} not in the bin", w.name, w.id));
            continue;
        };
        if w.global {
            match global_signature(&pe, rva) {
                Ok((site, sig, disp_offset, insn_len)) => {
                    let hex = |v: &[u8]| v.iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(", ");
                    let _ = writeln!(rs, "/// {} (global; CommonLibF4 `REL::ID({})`, {})", w.description, w.id, w.source);
                    let _ = writeln!(
                        rs,
                        "pub const {}: Address = Address {{ name: {:?}, rva: {rva:#x}, signature: &[{}], mask: &[{}], global: Some(GlobalRef {{ site_rva: {site:#x}, disp_offset: {disp_offset}, insn_len: {insn_len} }}) }};\n",
                        w.name.to_ascii_uppercase(),
                        w.name,
                        hex(&sig.bytes),
                        sig.mask.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
                    );
                    let pattern: String = sig.bytes.iter().zip(&sig.mask).map(|(b, m)| if *m == 0 { "??".into() } else { format!("{b:02X}") }).collect::<Vec<_>>().join(" ");
                    let _ = writeln!(provenance, "{}", serde_json::json!({
                        "id": format!("og/global/{}", w.name), "build": "og-1.10.163", "value": format!("{rva:#x}"),
                        "recipe": format!("rip-relative reference at signature: {pattern} (disp at +{disp_offset}, insn len {insn_len})"),
                        "sources": [
                            {"kind": "clf4", "ref": format!("REL::ID({})", w.id), "file": w.source, "symbol": w.description},
                            {"kind": "al-hint", "notes": "located with the user's local Address Library bin; not committed"},
                            {"kind": "re", "tool": "f4x-codegen addresses", "notes": format!("referenced from {site:#x}; signature unique in .text")}
                        ]
                    }));
                    println!("{:<32} {rva:#010x}  global, ref at {site:#x}  {pattern}", w.name);
                }
                Err(e) => failures.push(format!("{}: {e}", w.name)),
            }
            continue;
        }
        if !pe.is_function_start(&ranges, rva) {
            failures.push(format!("{}: {rva:#x} is not a function start", w.name));
            continue;
        }
        let sig = match signature(&pe, rva) {
            Ok(sig) => sig,
            Err(_) => match call_signature(&pe, rva) {
                Ok((site, sig)) => {
                    let hex = |v: &[u8]| v.iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(", ");
                    let _ = writeln!(rs, "/// {} (via a call site; CommonLibF4 `REL::ID({})`, {})", w.description, w.id, w.source);
                    let _ = writeln!(
                        rs,
                        "pub const {}: Address = Address {{ name: {:?}, rva: {rva:#x}, signature: &[{}], mask: &[{}], global: Some(GlobalRef {{ site_rva: {site:#x}, disp_offset: 1, insn_len: 5 }}) }};\n",
                        w.name.to_ascii_uppercase(),
                        w.name,
                        hex(&sig.bytes),
                        sig.mask.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
                    );
                    let pattern: String = sig.bytes.iter().zip(&sig.mask).map(|(b, m)| if *m == 0 { "??".into() } else { format!("{b:02X}") }).collect::<Vec<_>>().join(" ");
                    let _ = writeln!(provenance, "{}", serde_json::json!({
                        "id": format!("og/fn/{}", w.name), "build": "og-1.10.163", "value": format!("{rva:#x}"),
                        "recipe": format!("target of the call at signature: {pattern} (function bytes not unique in .text)"),
                        "sources": [
                            {"kind": "clf4", "ref": format!("REL::ID({})", w.id), "file": w.source, "symbol": w.description},
                            {"kind": "al-hint", "notes": "located with the user's local Address Library bin; not committed"},
                            {"kind": "re", "tool": "f4x-codegen addresses", "notes": format!("called from {site:#x}; signature unique in .text")}
                        ]
                    }));
                    println!("{:<32} {rva:#010x}  via call at {site:#x}  {pattern}", w.name);
                    continue;
                }
                Err(e) => {
                    failures.push(format!("{}: {e}", w.name));
                    continue;
                }
            },
        };
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(", ");
        if w.rva.is_some() {
            let _ = writeln!(rs, "/// {} ({})", w.description, w.source);
        } else {
            let _ = writeln!(rs, "/// {} (CommonLibF4 `REL::ID({})`, {})", w.description, w.id, w.source);
        }
        let _ = writeln!(
            rs,
            "pub const {}: Address = Address {{ name: {:?}, rva: {rva:#x}, signature: &[{}], mask: &[{}], global: None }};\n",
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

/// One line of the next-gen list (`wanted-ng_1_11_240.txt`): `name rva kind evidence`.
pub struct NgEntry {
    pub name: String,
    /// `None` for `none` (not found in this build).
    pub rva: Option<u32>,
    /// `fn`, `call`, `global` or `none`.
    pub kind: String,
    pub evidence: String,
}

pub fn parse_ng_list(text: &str) -> Result<Vec<NgEntry>, String> {
    let mut out = Vec::new();
    for (number, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name), Some(rva), Some(kind)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("line {number}: expected `name rva kind evidence`"));
        };
        let rva = match (kind, rva) {
            ("none", "-") => None,
            ("fn" | "call" | "global", hex) => Some(
                u32::from_str_radix(hex.trim_start_matches("0x"), 16).map_err(|e| format!("line {number}: rva `{hex}`: {e}"))?,
            ),
            _ => return Err(format!("line {number}: kind `{kind}` with rva `{rva}` (fn|call|global with an rva, or none with -)")),
        };
        let evidence = parts.collect::<Vec<_>>().join(" ");
        if evidence.is_empty() {
            return Err(format!("line {number}: {name} has no evidence"));
        }
        out.push(NgEntry { name: name.to_owned(), rva, kind: kind.to_owned(), evidence });
    }
    Ok(out)
}

/// A derived recipe: the referencing site (RVA, displacement offset, instruction length)
/// for a global or call-site recipe, `None` for a function-start one; the signature; how
/// it reads in provenance.
type Found = (Option<(u32, u8, u8)>, Signature, String);

/// `addresses --flavor ng`: no Address Library. The RVAs come from our own analysis of the
/// next-gen exe (`wanted-ng_1_11_240.txt`, same names and order as `wanted.txt`); this
/// derives the same masked recipes as the old-gen mode, unique in the next-gen `.text`.
/// Entries marked `none` get an empty recipe (they resolve to `None` at runtime).
pub fn run_ng(exe_path: &Path, wanted_path: &Path, list_path: &Path, out_rs: &Path, out_provenance: &Path) -> Result<(), String> {
    let exe = std::fs::read(exe_path).map_err(|e| format!("{}: {e}", exe_path.display()))?;
    let pe = PeFile::parse(&exe).map_err(|e| e.to_string())?;
    let ranges = pe.function_ranges();
    let wanted = parse_wanted(&std::fs::read_to_string(wanted_path).map_err(|e| e.to_string())?);
    let list = parse_ng_list(&std::fs::read_to_string(list_path).map_err(|e| format!("{}: {e}", list_path.display()))?)?;
    let names = |v: &mut dyn Iterator<Item = &str>| v.collect::<Vec<_>>().join(" ");
    if names(&mut wanted.iter().map(|w| w.name.as_str())) != names(&mut list.iter().map(|e| e.name.as_str())) {
        return Err(format!("{} must list the entries of {} (same names, same order)", list_path.display(), wanted_path.display()));
    }
    let hex = |v: &[u8]| v.iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(", ");
    let pattern = |s: &Signature| s.bytes.iter().zip(&s.mask).map(|(b, m)| if *m == 0 { "??".into() } else { format!("{b:02X}") }).collect::<Vec<String>>().join(" ");
    let mut rs = String::from(
        "//! Generated by `f4x-codegen addresses --flavor ng` from `crates/f4x-engine-sys/wanted-ng_1_11_240.txt`\n\
         //! (our own analysis of the exe; no Address Library). Do not edit.\n\
         //! Fallout 4 1.11.240 (next-gen, Steam): RVAs plus masked signatures unique in `.text`.\n\
         //! Same names as `og_1_10_163_addresses`; an entry with an empty signature was not found.\n\
         #![allow(clippy::all)]\n\nuse crate::{Address, GlobalRef};\n\n",
    );
    let mut provenance = String::new();
    let mut failures = Vec::new();
    let mut unresolved = 0;
    for (w, e) in wanted.iter().zip(&list) {
        let konst = w.name.to_ascii_uppercase();
        let identity = if w.rva.is_some() {
            serde_json::json!({"kind": "re", "notes": format!("1.10.163 entry: {} ({})", w.description, w.source)})
        } else {
            serde_json::json!({"kind": "clf4", "ref": format!("REL::ID({}) (the 1.10.163 entry; identity only)", w.id), "file": w.source, "symbol": w.description})
        };
        let analysis = serde_json::json!({"kind": "re", "tool": "f4x-re compare/func/disasm/xrefs", "notes": e.evidence});
        let Some(rva) = e.rva else {
            unresolved += 1;
            let _ = writeln!(rs, "/// {} (not found in 1.11.240: {})", w.description, e.evidence);
            let _ = writeln!(rs, "pub const {konst}: Address = Address {{ name: {:?}, rva: 0, signature: &[], mask: &[], global: None }};\n", w.name);
            let _ = writeln!(provenance, "{}", serde_json::json!({
                "id": format!("ng/none/{}", w.name), "build": "ng-1.11.240", "value": null,
                "recipe": "none (empty signature: resolves to None)", "sources": [identity, analysis]
            }));
            println!("{:<32} unresolved", w.name);
            continue;
        };
        // (site, signature, disp_offset, insn_len) for references; None for a function start.
        let found: Result<Found, String> = match e.kind.as_str() {
            "global" => global_signature(&pe, rva).map(|(site, sig, d, l)| {
                let p = pattern(&sig);
                (Some((site, d, l)), sig, format!("rip-relative reference at signature: {p} (disp at +{d}, insn len {l})"))
            }),
            "call" => call_signature(&pe, rva).map(|(site, sig)| {
                let p = pattern(&sig);
                (Some((site, 1, 5)), sig, format!("target of the call at signature: {p} (function bytes not unique in .text)"))
            }),
            _ if !pe.is_function_start(&ranges, rva) => Err(format!("{rva:#x} is not a function start")),
            _ => match signature(&pe, rva) {
                Ok(sig) => {
                    let p = pattern(&sig);
                    Ok((None, sig, format!("signature: {p}")))
                }
                Err(_) => call_signature(&pe, rva).map(|(site, sig)| {
                    let p = pattern(&sig);
                    (Some((site, 1, 5)), sig, format!("target of the call at signature: {p} (function bytes not unique in .text)"))
                }),
            },
        };
        let (site, sig, recipe) = match found {
            Ok(f) => f,
            Err(err) => {
                failures.push(format!("{}: {err}", w.name));
                continue;
            }
        };
        let (kind, global) = match site {
            None => ("fn", "None".to_owned()),
            Some((s, d, l)) => (
                if e.kind == "global" { "global" } else { "fn" },
                format!("Some(GlobalRef {{ site_rva: {s:#x}, disp_offset: {d}, insn_len: {l} }})"),
            ),
        };
        let how = match (e.kind.as_str(), site) {
            ("global", _) => "global; ",
            (_, Some(_)) => "via a call site; ",
            _ => "",
        };
        let _ = writeln!(rs, "/// {} ({how}1.11.240: {})", w.description, e.evidence);
        let _ = writeln!(
            rs,
            "pub const {konst}: Address = Address {{ name: {:?}, rva: {rva:#x}, signature: &[{}], mask: &[{}], global: {global} }};\n",
            w.name,
            hex(&sig.bytes),
            sig.mask.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
        );
        let notes = match site {
            Some((s, _, _)) => format!("referenced from {s:#x}; signature unique in .text"),
            None => "function start; signature unique in .text".to_owned(),
        };
        let _ = writeln!(provenance, "{}", serde_json::json!({
            "id": format!("ng/{kind}/{}", w.name), "build": "ng-1.11.240", "value": format!("{rva:#x}"), "recipe": recipe,
            "sources": [identity, analysis, {"kind": "re", "tool": "f4x-codegen addresses --flavor ng", "notes": notes}]
        }));
        println!("{:<32} {rva:#010x}  {}  {}", w.name, e.kind, recipe);
    }
    for f in &failures {
        eprintln!("failed: {f}");
    }
    if !failures.is_empty() {
        return Err(format!("{} address(es) failed; nothing written", failures.len()));
    }
    std::fs::write(out_rs, rs).map_err(|e| e.to_string())?;
    std::fs::write(out_provenance, provenance).map_err(|e| e.to_string())?;
    println!("{} entries, {unresolved} unresolved -> {}", list.len(), out_rs.display());
    Ok(())
}
