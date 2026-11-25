//! `f4x-vtcmp`: compare a class's vtables slot by slot across two `Fallout4.exe` builds.
//!
//! ```text
//! f4x-vtcmp <old.exe> <new.exe> <match.json> <class-substring>... [--all]
//! ```
//! For every RTTI vtable (any subobject) whose mangled or demangled class name contains a
//! substring, prints each slot's old and new function and how it relates via `match.json`:
//! `=` (the new slot holds the matched function), `-> slot N` (the matched function sits in
//! another new slot: the slots shifted), `!=` (matched elsewhere, not in this vtable) or `?`
//! (old function unmatched), plus `new - old`. The header line says whether the slots'
//! address order is the same in both builds (`order same`): code linked in the same order
//! keeps its relative order, so an unchanged vtable whose functions are unmatched still
//! shows a smooth delta and the same order. `--all` prints every slot, otherwise only the
//! first 0x80 and any that are not `=`. Read-only: evidence for hand-written slot indices.

use std::collections::HashMap;
use std::process::ExitCode;

use f4x_pe::rtti::{RttiIndex, demangle, vtable_len};
use f4x_pe::{Image, PeFile};

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn slots(pe: &PeFile<'_>, vtable: u32) -> Vec<u32> {
    (0..vtable_len(pe, vtable)).filter_map(|i| pe.u64_at(vtable + 8 * i as u32).and_then(|p| pe.rva_of(p))).collect()
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: f4x-vtcmp <old.exe> <new.exe> <match.json> <class-substring>... [--all]";
    let all = args.iter().any(|a| a == "--all");
    let args: Vec<&String> = args.iter().filter(|a| *a != "--all").collect();
    if args.len() < 4 {
        return Err(usage.into());
    }
    let (ob, nb) = (read(args[0])?, read(args[1])?);
    let (old, new) = (PeFile::parse(&ob).map_err(|e| e.0)?, PeFile::parse(&nb).map_err(|e| e.0)?);
    let text = std::fs::read_to_string(args[2]).map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let map: HashMap<u32, u32> = json["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["old"].as_u64()? as u32, m["new"].as_u64()? as u32)))
        .collect();
    let (ro, rn) = (RttiIndex::scan(&old), RttiIndex::scan(&new));
    for needle in &args[3..] {
        for v in ro.vtables.iter().filter(|v| v.class.contains(needle.as_str()) || demangle(&v.class).contains(needle.as_str())) {
            let name = demangle(&v.class);
            let Some(nv) = rn.vtables.iter().find(|n| n.class == v.class && n.offset == v.offset) else {
                println!("{name} +{:#x}: old {:#x}, not in new", v.offset, v.vtable_rva);
                continue;
            };
            let (os, ns) = (slots(&old, v.vtable_rva), slots(&new, nv.vtable_rva));
            let (mut same, mut other) = (0, 0);
            let mut lines = Vec::new();
            for i in 0..os.len().max(ns.len()) {
                let (o, n) = (os.get(i).copied(), ns.get(i).copied());
                let mark = match (o, n) {
                    (Some(o), n) => match map.get(&o) {
                        Some(&m) if Some(m) == n => "=".to_owned(),
                        Some(&m) => match ns.iter().position(|&x| x == m) {
                            Some(j) => format!("-> slot {j:#x}"),
                            None => format!("!= (match {m:#x})"),
                        },
                        None => "?".to_owned(),
                    },
                    (None, _) => "(new only)".to_owned(),
                };
                if mark == "=" {
                    same += 1;
                } else {
                    other += 1;
                }
                if all || i < 0x80 || mark != "=" {
                    let delta = match (o, n) {
                        (Some(o), Some(n)) if n >= o => format!("+{:#x}", n - o),
                        (Some(o), Some(n)) => format!("-{:#x}", o - n),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        "  [{i:#04x}] {:>10} {:>10} {delta:>10}  {mark}",
                        o.map_or("-".into(), |o| format!("{o:#x}")),
                        n.map_or("-".into(), |n| format!("{n:#x}"))
                    ));
                }
            }
            let rank = |s: &[u32]| {
                let mut idx: Vec<usize> = (0..s.len()).collect();
                idx.sort_by_key(|&i| (s[i], i));
                idx
            };
            let order = if os.len() == ns.len() && rank(&os) == rank(&ns) { "order same" } else { "order differs" };
            println!(
                "{name} +{:#x}: old {:#x} ({} slots), new {:#x} ({} slots): {same} =, {other} other, {order}",
                v.offset,
                v.vtable_rva,
                os.len(),
                nv.vtable_rva,
                ns.len()
            );
            for l in lines {
                println!("{l}");
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
