//! Spike S3 exit check: a native catalog (`natives.jsonl` from a session) against the
//! game on disk.
//!
//! 1. Every callback RVA must be a function start in `Fallout4.exe`'s `.pdata`.
//! 2. The set of (script, function) natives the VM bound must match the native
//!    declarations in the vanilla scripts (`Fallout4 - Misc.ba2`).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use f4x_pe::PeFile;
use serde_json::Value;

use crate::env::GameEnv;
use crate::workspace_root;

pub fn run(args: &[String]) -> Result<(), String> {
    let natives = match args.iter().position(|a| a == "--natives").and_then(|i| args.get(i + 1)) {
        Some(path) => PathBuf::from(path),
        None => latest_catalog()?,
    };
    let env = GameEnv::from_env();
    let text = std::fs::read_to_string(&natives).map_err(|e| format!("{}: {e}", natives.display()))?;
    let records: Vec<Value> =
        text.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    println!("catalog: {} ({} bindings)", natives.display(), records.len());

    // 1. Callbacks are function starts.
    let exe = std::fs::read(env.game_dir.join("Fallout4.exe")).map_err(|e| e.to_string())?;
    let pe = PeFile::parse(&exe).map_err(|e| e.to_string())?;
    let starts: HashSet<u32> = pe.function_starts().into_iter().collect();
    let ranges = pe.function_ranges();
    let mut leaf = 0;
    let mut bad_callbacks = Vec::new();
    for record in &records {
        let rva = record["callback_rva"].as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        if rva.is_some_and(|r| !starts.contains(&r) && pe.is_function_start(&ranges, r)) {
            leaf += 1;
        } else if !rva.is_some_and(|r| starts.contains(&r)) {
            bad_callbacks.push(format!("{}.{} -> {:?}", record["object"], record["name"], record["callback_rva"]));
        }
    }
    println!(
        "callbacks: {} at .pdata starts, {leaf} leaf functions (aligned, outside every unwind range), {} bad",
        records.len() - bad_callbacks.len() - leaf,
        bad_callbacks.len()
    );
    for bad in bad_callbacks.iter().take(10) {
        println!("  not a function start: {bad}");
    }

    // 2. Bound natives vs declared natives.
    let key = |object: &str, name: &str| (object.to_ascii_lowercase(), name.to_ascii_lowercase());
    let bound: BTreeSet<_> = records
        .iter()
        .map(|r| key(r["object"].as_str().unwrap_or(""), r["name"].as_str().unwrap_or("")))
        .collect();
    let archive_path = env.game_dir.join("Data/Fallout4 - Misc.ba2");
    let archive = f4x_ba2::Archive::open(&archive_path).map_err(|e| format!("{}: {e}", archive_path.display()))?;
    let mut declared = BTreeSet::new();
    let mut scripts = 0;
    for entry in archive.entries() {
        if !entry.name.to_ascii_lowercase().ends_with(".pex") {
            continue;
        }
        let bytes = archive.extract(entry).map_err(|e| e.to_string())?;
        let pex = f4x_pex::Pex::parse(&bytes).map_err(|e| format!("{}: {e}", entry.name))?;
        scripts += 1;
        for f in pex.native_functions() {
            declared.insert(key(&f.object, &f.name));
        }
    }
    let only_bound: Vec<_> = bound.difference(&declared).collect();
    let only_declared: Vec<_> = declared.difference(&bound).collect();
    println!(
        "natives: {} bound, {} declared in {scripts} vanilla scripts; {} bound-only, {} declared-only",
        bound.len(),
        declared.len(),
        only_bound.len(),
        only_declared.len()
    );
    for (object, name) in only_bound.iter().take(15) {
        println!("  bound but not declared: {object}.{name}");
    }
    for (object, name) in only_declared.iter().take(15) {
        println!("  declared but not bound: {object}.{name}");
    }

    // Declared-only natives are allowed (the release exe leaves some dev/legacy natives
    // unbound); a bound native that no vanilla script declares is not.
    if bad_callbacks.is_empty() && only_bound.is_empty() {
        Ok(())
    } else {
        Err("catalog does not match the game".into())
    }
}

fn latest_catalog() -> Result<PathBuf, String> {
    let sessions = workspace_root().join("target/f4x-sessions");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .map_err(|e| format!("{}: {e}", sessions.display()))?
        .flatten()
        .map(|e| e.path().join("natives.jsonl"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().ok_or_else(|| "no session has a natives.jsonl yet; run `cargo xtask spike-launch`".into())
}
