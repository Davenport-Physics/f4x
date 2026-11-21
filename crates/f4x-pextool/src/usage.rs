//! `pex f4se-usage`: which F4SE script functions does an installed mod set use?
//!
//! 1. F4SE's script surface = what its `.pex` files declare beyond the vanilla scripts
//!    (signatures only): new natives, new event stubs, new scripts.
//! 2. Every mod script (loose `Data/Scripts/**` and `.pex` inside non-vanilla BA2s) is
//!    scanned: `callmethod`/`callstatic`/`callparent` are resolved to the class that
//!    declares the function (receiver type from params, locals, variables, properties;
//!    walking the parent chain), and calls landing on an F4SE addition are reported.
//!    Event stubs a script overrides (e.g. `OnKeyDown`) are reported too.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use f4x_pex::{Opcode, Pex, Value};

#[derive(Default)]
struct Class {
    parent: String,
    functions: BTreeSet<String>,
}

#[derive(Default)]
struct World {
    /// lowercase class -> declaration info
    classes: HashMap<String, Class>,
}

impl World {
    fn add(&mut self, pex: &Pex, only_new: bool) {
        for object in &pex.objects {
            let name = pex.str(object.name).to_ascii_lowercase();
            let entry = self.classes.entry(name).or_default();
            if !only_new || entry.parent.is_empty() {
                entry.parent = pex.str(object.parent_class_name).to_ascii_lowercase();
            }
            for f in pex.functions().iter().filter(|f| std::ptr::eq(f.object, object)) {
                entry.functions.insert(pex.str(f.name).to_ascii_lowercase());
            }
        }
    }

    /// The class (walking parents from `class`) that declares `function`.
    fn declaring(&self, class: &str, function: &str) -> Option<String> {
        let mut current = class.to_ascii_lowercase();
        for _ in 0..32 {
            let entry = self.classes.get(&current)?;
            if entry.functions.contains(function) {
                return Some(current);
            }
            if entry.parent.is_empty() {
                return None;
            }
            current = entry.parent.clone();
        }
        None
    }
}

fn vanilla_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("fallout4 - ") || lower.starts_with("dlc") || lower.starts_with("cc")
}

fn read_ba2_scripts(path: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(archive) = f4x_ba2::Archive::open(path) else { return Vec::new() };
    archive
        .entries()
        .iter()
        .filter(|e| e.name.to_ascii_lowercase().ends_with(".pex"))
        .filter_map(|e| Some((e.name.clone(), archive.extract(e).ok()?)))
        .collect()
}

fn walk_pex(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_pex(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pex")) {
            out.push(path);
        }
    }
}

pub fn run(args: &[&str]) -> Result<std::process::ExitCode, String> {
    let value = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).map(|s| PathBuf::from(s));
    let game = value("--game").ok_or("--game <Fallout 4 dir> is required")?;
    let f4se = value("--f4se-scripts").ok_or("--f4se-scripts <dir with F4SE's .pex> is required")?;
    let json_out = value("--json");
    let data = game.join("Data");

    // Vanilla: Misc + official DLC/CC script archives.
    let mut vanilla = World::default();
    let mut archives: Vec<PathBuf> = std::fs::read_dir(&data)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ba2")))
        .collect();
    archives.sort();
    let mut vanilla_count = 0;
    for archive in archives.iter().filter(|p| vanilla_archive(&p.file_name().unwrap_or_default().to_string_lossy())) {
        for (_, bytes) in read_ba2_scripts(archive) {
            if let Ok(pex) = Pex::parse(&bytes) {
                vanilla.add(&pex, false);
                vanilla_count += 1;
            }
        }
    }

    // F4SE additions.
    let mut f4se_files = Vec::new();
    walk_pex(&f4se, &mut f4se_files);
    let f4se_names: BTreeSet<String> =
        f4se_files.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase()).collect();
    let mut additions: BTreeMap<(String, String), &'static str> = BTreeMap::new();
    let mut world = World::default();
    world.classes = std::mem::take(&mut vanilla.classes);
    for path in &f4se_files {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let pex = Pex::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        for f in pex.functions() {
            let class = pex.str(f.object.name).to_ascii_lowercase();
            let name = pex.str(f.name).to_ascii_lowercase();
            let known = world.classes.get(&class).is_some_and(|c| c.functions.contains(&name));
            if !known {
                additions.insert((class, name), if f.function.is_native() { "native" } else { "event" });
            }
        }
        world.add(&pex, true);
    }

    // Mod scripts.
    let mut mod_scripts: Vec<(String, String, Vec<u8>)> = Vec::new(); // (source, script, bytes)
    let mut loose = Vec::new();
    walk_pex(&data.join("Scripts"), &mut loose);
    for path in loose {
        let file = path.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
        if f4se_names.contains(&file) && path.parent().is_some_and(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("Scripts"))) {
            continue; // F4SE's own scripts
        }
        if let Ok(bytes) = std::fs::read(&path) {
            mod_scripts.push(("loose".into(), path.strip_prefix(&data).unwrap_or(&path).display().to_string(), bytes));
        }
    }
    for archive in archives.iter().filter(|p| !vanilla_archive(&p.file_name().unwrap_or_default().to_string_lossy())) {
        let source = archive.file_name().unwrap_or_default().to_string_lossy().into_owned();
        for (name, bytes) in read_ba2_scripts(archive) {
            mod_scripts.push((source.clone(), name, bytes));
        }
    }
    let mut parsed = Vec::new();
    for (source, name, bytes) in mod_scripts {
        match Pex::parse(&bytes) {
            Ok(pex) => {
                world.add(&pex, false);
                parsed.push((source, name, pex));
            }
            Err(e) => eprintln!("warning: {name}: {e}"),
        }
    }

    // Scan calls.
    let mut by_source: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut scripts_using: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let (mut calls_seen, mut calls_resolved) = (0usize, 0usize);
    let mut unresolved_samples: Vec<String> = Vec::new();
    for (source, script, pex) in &parsed {
        for object in &pex.objects {
            let class = pex.str(object.name).to_ascii_lowercase();
            let parent = pex.str(object.parent_class_name).to_ascii_lowercase();
            let mut fields: HashMap<String, String> = HashMap::new();
            for v in &object.variables {
                fields.insert(pex.str(v.name).to_ascii_lowercase(), pex.str(v.type_name).to_ascii_lowercase());
            }
            for p in &object.properties {
                fields.insert(pex.str(p.name).to_ascii_lowercase(), pex.str(p.type_name).to_ascii_lowercase());
            }
            for f in pex.functions().iter().filter(|f| std::ptr::eq(f.object, object)) {
                let fname = pex.str(f.name).to_ascii_lowercase();
                // Overridden F4SE event stubs.
                if let Some(declaring) = world.declaring(&parent, &fname)
                    && additions.get(&(declaring.clone(), fname.clone())) == Some(&"event")
                {
                    *by_source.entry(source.clone()).or_default().entry(format!("{declaring}.{fname} (event)")).or_default() += 1;
                    scripts_using.entry(source.clone()).or_default().insert(script.clone());
                }
                let mut types = fields.clone();
                for t in f.function.params.iter().chain(&f.function.locals) {
                    types.insert(pex.str(t.name).to_ascii_lowercase(), pex.str(t.type_name).to_ascii_lowercase());
                }
                types.insert("self".into(), class.clone());
                for instruction in &f.function.instructions {
                    let ident = |v: &Value| match v {
                        Value::Identifier(id) | Value::String(id) => Some(pex.str(*id).to_ascii_lowercase()),
                        _ => None,
                    };
                    let target = match instruction.op {
                        Opcode::CallMethod => {
                            let (Some(method), Some(receiver)) = (instruction.args.first().and_then(ident), instruction.args.get(1).and_then(ident)) else { continue };
                            let Some(receiver_type) = types.get(&receiver).cloned() else { continue };
                            world.declaring(receiver_type.trim_end_matches("[]"), &method).map(|c| (c, method))
                        }
                        Opcode::CallStatic => {
                            let (Some(object_name), Some(method)) = (instruction.args.first().and_then(ident), instruction.args.get(1).and_then(ident)) else { continue };
                            world.declaring(&object_name, &method).map(|c| (c, method))
                        }
                        Opcode::CallParent => {
                            let Some(method) = instruction.args.first().and_then(ident) else { continue };
                            world.declaring(&parent, &method).map(|c| (c, method))
                        }
                        _ => None,
                    };
                    if matches!(instruction.op, Opcode::CallMethod | Opcode::CallStatic | Opcode::CallParent) {
                        calls_seen += 1;
                        if target.is_some() {
                            calls_resolved += 1;
                        } else if unresolved_samples.len() < 8 {
                            unresolved_samples.push(format!("{script}: {:?} {:?}", instruction.op, instruction.args.iter().take(2).map(|a| match a { Value::Identifier(i) | Value::String(i) => pex.str(*i).into_owned(), other => format!("{other:?}") }).collect::<Vec<_>>()));
                        }
                    }
                    if let Some((declaring, method)) = target
                        && additions.get(&(declaring.clone(), method.clone())) == Some(&"native")
                    {
                        *by_source.entry(source.clone()).or_default().entry(format!("{declaring}.{method}")).or_default() += 1;
                        scripts_using.entry(source.clone()).or_default().insert(script.clone());
                    }
                }
            }
        }
    }

    let natives = additions.values().filter(|k| **k == "native").count();
    println!("calls: {calls_seen} seen, {calls_resolved} resolved to a declaring class");
    for sample in &unresolved_samples {
        println!("  unresolved: {sample}");
    }
    println!(
        "vanilla scripts: {vanilla_count}; F4SE additions: {natives} natives, {} event stubs; mod scripts scanned: {}",
        additions.len() - natives,
        parsed.len()
    );
    let mut union: BTreeMap<String, usize> = BTreeMap::new();
    for (source, calls) in &by_source {
        println!("\n{source}: {} F4SE functions in {} scripts", calls.len(), scripts_using[source].len());
        for (call, count) in calls {
            println!("  {call:<55} x{count}");
            *union.entry(call.clone()).or_default() += count;
        }
    }
    println!("\nunion: {} distinct F4SE functions", union.len());
    if let Some(path) = json_out {
        let report = serde_json::json!({
            "f4se_additions": additions.iter().map(|((c, f), k)| serde_json::json!({"class": c, "function": f, "kind": k})).collect::<Vec<_>>(),
            "by_source": by_source,
            "scripts": scripts_using,
            "union": union,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default()).map_err(|e| e.to_string())?;
        println!("wrote {}", path.display());
    }
    Ok(std::process::ExitCode::SUCCESS)
}
