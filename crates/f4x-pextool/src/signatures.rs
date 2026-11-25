//! `pex f4se-signatures`: what F4SE adds to the game's scripts, as signatures only.
//!
//! Each F4SE script is compared with the game's vanilla script of the same name (from
//! `Fallout4 - Misc.ba2`). What F4SE adds is written out as names and types: structs and
//! their members, functions (native, empty event stub, or scripted), and properties.
//! Bodies, constant values, default values and comments are never copied (clean-room
//! rule: F4SE's `.pex` files may be read for signatures only; provenance tag `f4se-pex`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use f4x_pex::{FunctionKind, Pex};
use serde_json::{Value as Json, json};

fn vanilla_scripts(data: &std::path::Path) -> BTreeMap<String, Pex> {
    let mut out = BTreeMap::new();
    let Ok(archive) = f4x_ba2::Archive::open(&data.join("Fallout4 - Misc.ba2")) else { return out };
    for entry in archive.entries() {
        let name = entry.name.replace('\\', "/").to_ascii_lowercase();
        let Some(file) = name.strip_prefix("scripts/").and_then(|n| n.strip_suffix(".pex")) else { continue };
        if file.contains('/') {
            continue;
        }
        if let Ok(bytes) = archive.extract(entry)
            && let Ok(pex) = Pex::parse(&bytes)
        {
            out.insert(file.to_owned(), pex);
        }
    }
    out
}

pub fn run(args: &[&str]) -> Result<ExitCode, String> {
    let value = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let game = value("--game").ok_or("--game <Fallout 4 dir> is required")?;
    let f4se = value("--f4se-scripts").ok_or("--f4se-scripts <dir with F4SE's .pex> is required")?;
    let out_path = value("--out").ok_or("--out <file.json> is required")?;
    // Labels for the "about" text (defaults: the 1.10.163 inventory).
    let label = |flag: &str, default: &str| {
        args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).map_or_else(|| default.to_owned(), |v| (*v).to_owned())
    };
    let (f4se_version, game_version) = (label("--f4se-version", "0.6.23"), label("--game-version", "1.10.163"));
    let vanilla = vanilla_scripts(&game.join("Data"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(&f4se)
        .map_err(|e| format!("{}: {e}", f4se.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pex")))
        .collect();
    files.sort();

    let mut scripts = Vec::new();
    let (mut natives, mut stubs, mut scripted, mut props) = (0, 0, 0, 0);
    for path in files {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let pex = Pex::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(object) = pex.objects.first() else { continue };
        let name = pex.str(object.name).into_owned();
        let base = vanilla.get(&name.to_ascii_lowercase());
        let base_obj = base.and_then(|b| b.objects.first());
        let known_fn = |f: &str| {
            base.is_some_and(|b| b.functions().iter().any(|r| r.kind == FunctionKind::State && b.str(r.name).eq_ignore_ascii_case(f)))
        };
        let known_struct =
            |s: &str| base.zip(base_obj).is_some_and(|(b, o)| o.structs.iter().any(|x| b.str(x.name).eq_ignore_ascii_case(s)));
        let known_prop =
            |p: &str| base.zip(base_obj).is_some_and(|(b, o)| o.properties.iter().any(|x| b.str(x.name).eq_ignore_ascii_case(p)));

        let structs: Vec<Json> = object
            .structs
            .iter()
            .filter(|s| !known_struct(&pex.str(s.name)))
            .map(|s| {
                json!({
                    "name": pex.str(s.name),
                    "members": s.members.iter().map(|m| json!([pex.str(m.name), pex.str(m.type_name)])).collect::<Vec<_>>(),
                })
            })
            .collect();
        let mut functions = Vec::new();
        for f in pex.functions() {
            if f.kind != FunctionKind::State || !pex.str(f.owner).is_empty() {
                continue;
            }
            let fname = pex.str(f.name).into_owned();
            if known_fn(&fname) {
                continue;
            }
            let sig = pex.signature(&f);
            let kind = if f.function.is_native() {
                natives += 1;
                "native"
            } else if f.function.instructions.is_empty() {
                stubs += 1;
                "stub"
            } else {
                scripted += 1;
                "scripted"
            };
            functions.push(json!({
                "name": fname,
                "returns": sig.return_type,
                "params": sig.params.iter().map(|p| json!([p.type_name, p.name])).collect::<Vec<_>>(),
                "global": sig.is_global,
                "kind": kind,
            }));
        }
        let properties: Vec<Json> = object
            .properties
            .iter()
            .filter(|p| !known_prop(&pex.str(p.name)))
            .map(|p| {
                props += 1;
                json!({
                    "name": pex.str(p.name),
                    "type": pex.str(p.type_name),
                    "readable": p.flags & 1 != 0,
                    "writable": p.flags & 2 != 0,
                    "auto": p.is_auto(),
                })
            })
            .collect();
        if structs.is_empty() && functions.is_empty() && properties.is_empty() {
            continue;
        }
        scripts.push(json!({
            "script": name,
            "extends": pex.str(object.parent_class_name),
            "new_script": base.is_none(),
            "structs": structs,
            "functions": functions,
            "properties": properties,
        }));
    }
    let doc = json!({
        "about": format!("Signatures (names and types only) of what F4SE {f4se_version} adds to Fallout 4 {game_version} scripts. Source: F4SE's distributed .pex files, compared with vanilla Fallout4 - Misc.ba2. Generated by `f4x-pextool pex f4se-signatures`; provenance tag f4se-pex."),
        "scripts": scripts,
    });
    std::fs::write(&out_path, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n").map_err(|e| e.to_string())?;
    println!("{} scripts: {natives} native, {stubs} stub, {scripted} scripted functions, {props} properties -> {}", doc["scripts"].as_array().map_or(0, Vec::len), out_path.display());
    Ok(ExitCode::SUCCESS)
}
