//! `f4x-re`: analysis commands over `Fallout4.exe` builds.
//!
//! ```text
//! f4x-re match <old.exe> <new.exe> [--seeds FILE] --out match.json
//! f4x-re lookup <match.json> <old-rva>...
//! f4x-re func <exe> <rva>            features of the function containing rva
//! f4x-re disasm <exe> <rva> [count]
//! f4x-re xrefs <exe> <rva>           instructions referencing rva
//! f4x-re strings <exe> <substring>   functions referencing matching strings
//! f4x-re suggest <old.exe> <new.exe> <match.json> <old-rva>...
//!                                    rank new-build candidates for unmatched functions
//! f4x-re compare <old.exe> <new.exe> <match.json> <pairs-file>
//!                                    evidence that each `name fn|global old new` pair is
//!                                    the same function (or global) in both builds
//! f4x-re query <exe> <term>...       functions having every feature: imm=X data=X
//!                                    const=X call=X string=S vtable=S size<=X size>=X
//! ```
//! Seeds: lines `old_rva new_rva how...` (hex), e.g. verified signature hits.

use std::process::ExitCode;

use f4x_re::compare;
use f4x_re::index::Binary;
use f4x_re::matcher::Matcher;

fn hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("{s}: {e}"))
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: f4x-re match|lookup|func|disasm|xrefs|strings ... (see the source)";
    match args.first().map(String::as_str) {
        Some("match") => {
            let (old, new) = (read(args.get(1).ok_or(usage)?)?, read(args.get(2).ok_or(usage)?)?);
            let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).ok_or("--out FILE")?;
            let t = std::time::Instant::now();
            let (old, new) = (Binary::load(&old)?, Binary::load(&new)?);
            eprintln!("indexed {} + {} functions in {:?}", old.funcs.len(), new.funcs.len(), t.elapsed());
            let mut m = Matcher::new(&old, &new);
            if let Some(seeds) = args.iter().position(|a| a == "--seeds").and_then(|i| args.get(i + 1)) {
                let text = std::fs::read_to_string(seeds).map_err(|e| format!("{seeds}: {e}"))?;
                for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
                    let mut parts = line.split_whitespace();
                    let (o, n) = (hex(parts.next().unwrap_or(""))?, hex(parts.next().unwrap_or(""))?);
                    let how = parts.collect::<Vec<_>>().join(" ");
                    if !m.seed(o, n, &format!("seed: {how}")) {
                        eprintln!("seed {o:#x} -> {n:#x} not applied (not function starts, or conflicting)");
                    }
                }
            }
            m.anchors();
            eprintln!("anchors: {} matches ({} conflicts)", m.matches().len(), m.conflicts);
            m.propagate();
            eprintln!("after propagation: {} matches, {} globals ({} conflicts)", m.matches().len(), m.globals.len(), m.conflicts);
            let mut globals: Vec<_> = m.globals.iter().map(|(&o, (n, how))| serde_json::json!({"old": o, "new": n, "how": how})).collect();
            globals.sort_by_key(|g| g["old"].as_u64());
            let json = serde_json::json!({"functions": m.matches(), "globals": globals});
            std::fs::write(out, serde_json::to_string(&json).map_err(|e| e.to_string())?).map_err(|e| format!("{out}: {e}"))?;
            Ok(())
        }
        Some("lookup") => {
            let text = std::fs::read_to_string(args.get(1).ok_or(usage)?).map_err(|e| e.to_string())?;
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            for rva in &args[2..] {
                let rva = hex(rva)? as u64;
                let hit = json["functions"].as_array().into_iter().flatten().chain(json["globals"].as_array().into_iter().flatten()).find(|m| m["old"].as_u64() == Some(rva));
                match hit {
                    Some(m) => println!("{rva:#x} -> {:#x}  ({})", m["new"].as_u64().unwrap_or(0), m["how"].as_str().unwrap_or("")),
                    None => println!("{rva:#x} -> ?"),
                }
            }
            Ok(())
        }
        Some("suggest") => {
            let (old, new) = (read(args.get(1).ok_or(usage)?)?, read(args.get(2).ok_or(usage)?)?);
            let (old, new) = (Binary::load(&old)?, Binary::load(&new)?);
            let text = std::fs::read_to_string(args.get(3).ok_or(usage)?).map_err(|e| e.to_string())?;
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let map: std::collections::HashMap<u32, u32> = json["functions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((m["old"].as_u64()? as u32, m["new"].as_u64()? as u32)))
                .collect();
            let mapped: std::collections::HashSet<u32> = map.values().copied().collect();
            for rva in &args[4..] {
                let rva = hex(rva)?;
                let Some(o) = old.index_of(rva) else {
                    println!("{rva:#x}: not a function start in the old build");
                    continue;
                };
                let f = &old.funcs[o];
                let mut score: std::collections::HashMap<usize, (f64, Vec<String>)> = std::collections::HashMap::new();
                // Each piece of evidence spreads one vote over the unmatched candidates it allows.
                let mut spread = |cands: Vec<usize>, why: String| {
                    let cands: Vec<usize> = cands.into_iter().filter(|&n| !mapped.contains(&new.funcs[n].start)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                    let w = 1.0 / cands.len().max(1) as f64;
                    for n in cands {
                        let e = score.entry(n).or_default();
                        e.0 += w;
                        e.1.push(why.clone());
                    }
                };
                for &caller in &old.callers[o] {
                    let cs = old.funcs[caller].start;
                    if let Some(nc) = map.get(&cs).and_then(|&s| new.index_of(s)) {
                        spread(new.funcs[nc].calls.iter().filter_map(|&c| new.index_of(c)).collect(), format!("called by {cs:#x}"));
                    }
                }
                for &callee in &f.calls {
                    if let Some(nc) = map.get(&callee).and_then(|&s| new.index_of(s)) {
                        spread(new.callers[nc].clone(), format!("calls {callee:#x}"));
                    }
                }
                for s in f.strings.iter().collect::<std::collections::HashSet<_>>() {
                    spread(new.functions_with_string(s), format!("string {s:?}"));
                }
                let mut ranked: Vec<_> = score.into_iter().collect();
                ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
                println!("{rva:#x} (size {:#x}, {} calls, {} callers, strings {:?})", f.size, f.calls.len(), old.callers[o].len(), f.strings);
                for (n, (votes, why)) in ranked.into_iter().take(6) {
                    let g = &new.funcs[n];
                    let taken = if mapped.contains(&g.start) { " [already matched]" } else { "" };
                    let mut why = why;
                    why.dedup();
                    println!("   {:#x} score {votes:.2} size {:#x}{taken}: {}", g.start, g.size, why.iter().take(4).cloned().collect::<Vec<_>>().join(", "));
                }
            }
            Ok(())
        }
        Some("compare") => {
            let (old, new) = (read(args.get(1).ok_or(usage)?)?, read(args.get(2).ok_or(usage)?)?);
            let (old, new) = (Binary::load(&old)?, Binary::load(&new)?);
            let text = std::fs::read_to_string(args.get(3).ok_or(usage)?).map_err(|e| e.to_string())?;
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let pairs_of = |key: &str| -> std::collections::HashMap<u32, u32> {
                json[key].as_array().into_iter().flatten().filter_map(|m| Some((m["old"].as_u64()? as u32, m["new"].as_u64()? as u32))).collect()
            };
            let (fmap, gmap) = (pairs_of("functions"), pairs_of("globals"));
            let pairs = std::fs::read_to_string(args.get(4).ok_or(usage)?).map_err(|e| e.to_string())?;
            for line in pairs.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                let p: Vec<&str> = line.split_whitespace().collect();
                let [name, kind, o, n, ..] = p[..] else { continue };
                let (o, n) = (hex(o)?, hex(n)?);
                println!("{}", compare::report(&old, &new, &fmap, &gmap, name, kind, o, n));
            }
            Ok(())
        }
        Some(cmd @ ("func" | "disasm" | "xrefs" | "strings" | "query")) => {
            let bytes = read(args.get(1).ok_or(usage)?)?;
            let b = Binary::load(&bytes)?;
            match cmd {
                "query" => {
                    // Functions having every listed feature: imm=, data=, const=, call=,
                    // string= (substring), vtable= (substring), size<= / size>=.
                    let mut hits = 0;
                    for f in &b.funcs {
                        let ok = args[2..].iter().all(|term| {
                            let (k, v) = term.split_once(['=', '<', '>']).unwrap_or((term, ""));
                            let v = v.trim_start_matches('=');
                            let num = || hex(v).unwrap_or(u32::MAX);
                            match k {
                                "imm" => f.imms.contains(&(num() as u64)),
                                "data" => f.data.contains(&num()),
                                "const" => f.consts.contains(&num()),
                                "call" => f.calls.contains(&num()),
                                "string" => f.strings.iter().any(|s| s.contains(v)),
                                "vtable" => f.vtables.iter().any(|s| s.contains(v)),
                                "size" if term.contains('<') => f.size <= num(),
                                "size" => f.size >= num(),
                                _ => false,
                            }
                        });
                        if ok {
                            hits += 1;
                            if hits <= 400 {
                                println!("{:#x} size {:#x} insns {} calls {} data {}", f.start, f.size, f.insns, f.calls.len(), f.data.len());
                            }
                        }
                    }
                    println!("{hits} function(s)");
                }
                "func" => {
                    for rva in &args[2..] {
                        let rva = hex(rva)?;
                        let Some(i) = b.owner(rva).or_else(|| b.index_of(rva)) else {
                            println!("{rva:#x}: not in any function");
                            continue;
                        };
                        let f = &b.funcs[i];
                        println!("{rva:#x} in function {:#x} (size {:#x}, {} insns, chunks {:x?})", f.start, f.size, f.insns, f.chunks);
                        println!("  callers: {:x?}", b.callers[i].iter().map(|&c| b.funcs[c].start).collect::<Vec<_>>());
                        println!("  calls: {:x?}", f.calls);
                        println!("  strings: {:?}", f.strings);
                        println!("  vtables: {:?}", f.vtables);
                        println!("  data: {:x?}", f.data);
                        println!("  consts: {:x?}", f.consts);
                        println!("  imms: {:x?}", f.imms);
                    }
                }
                "disasm" => {
                    let rva = hex(args.get(2).ok_or(usage)?)?;
                    let count = args.get(3).and_then(|c| c.parse().ok()).unwrap_or(40);
                    for line in b.disasm(rva, count) {
                        println!("{line}");
                    }
                }
                "xrefs" => {
                    for rva in &args[2..] {
                        let rva = hex(rva)?;
                        for site in b.xrefs(rva) {
                            let owner = b.owner(site).map(|i| b.funcs[i].start).unwrap_or(0);
                            println!("{rva:#x} <- {site:#x} (in {owner:#x})");
                        }
                    }
                }
                _ => {
                    let needle = args.get(2).ok_or(usage)?;
                    for (s, users) in b.string_users() {
                        if s.contains(needle.as_str()) {
                            let starts: Vec<String> = users.iter().map(|&u| format!("{:#x}", b.funcs[u].start)).collect();
                            println!("{s:?}: {}", starts.join(" "));
                        }
                    }
                }
            }
            Ok(())
        }
        _ => Err(usage.into()),
    }
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
