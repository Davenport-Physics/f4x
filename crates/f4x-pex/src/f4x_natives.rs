//! Script declarations for F4SE's script API, generated from the signature inventory.
//!
//! Mods compiled against F4SE call functions such as `EncounterZone.IsWorkshop()`. The VM
//! resolves those by name in the script's own type, so the script must declare them. F4SE
//! ships its own edited copies of the vanilla scripts; f4x does not use or redistribute
//! them. Instead:
//! - [`patch`] adds what F4SE adds (structs, functions, constant properties) to the user's
//!   own vanilla script;
//! - [`create`] writes the scripts F4SE adds (`UI`, `InstanceData`, ...) from scratch.
//!
//! Function and struct signatures come from `data/f4se-0.6.23-signatures.json` (names
//! and types only, extracted from F4SE's distributed `.pex`; provenance tag `f4se-pex`).
//! F4SE's scripted functions are declared native: f4x implements them in Rust. Constant
//! property values come from `data/constants/<Script>.json`, each value with its own
//! clean source (CommonLibF4 enums or the engine); properties without one are left out.
//! Struct member defaults come from `data/struct-defaults/<Script>.json` (observed
//! black-box: what a default-constructed struct holds under F4SE); members without one
//! default to the type's zero value.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value as Json;

use crate::opcode::Opcode;
use crate::{
    Function, Header, Instruction, NamedFunction, Object, Pex, PexString, Property, State, StringTable, Struct,
    StructMember, StructOrder, UserFlag, Value,
};

/// The inventory, as shipped in the crate: F4SE 0.6.23 on 1.10.163.
pub const INVENTORY: &str = include_str!("../data/f4se-0.6.23-signatures.json");

/// The next-gen inventory: F4SE 0.7.9 on 1.11.240 (a superset of 0.6.23's functions:
/// adds `Form.GetEditorID` and the gamepad-button registrations and events).
pub const INVENTORY_NG: &str = include_str!("../data/f4se-0.7.9-signatures.json");

/// One function F4SE adds to a script.
#[derive(Clone, Debug)]
pub struct FunctionDecl {
    pub name: String,
    pub returns: String,
    /// `(type, name)`.
    pub params: Vec<(String, String)>,
    pub global: bool,
    /// `native`, `stub` (empty event) or `scripted` (declared native here).
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: String,
    /// `(name, type)`.
    pub members: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct ScriptDecl {
    pub script: String,
    pub extends: String,
    /// Not in the vanilla game: written from scratch.
    pub new_script: bool,
    pub structs: Vec<StructDecl>,
    pub functions: Vec<FunctionDecl>,
    /// Read-only `Int` properties: names only (values from the constants files).
    pub properties: Vec<String>,
}

fn strs(v: &Json) -> Vec<(String, String)> {
    v.as_array()
        .map(|a| a.iter().filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_str()?.to_owned()))).collect())
        .unwrap_or_default()
}

/// Every script F4SE adds to or changes (1.10.163).
pub fn inventory() -> Vec<ScriptDecl> {
    inventory_for(false)
}

/// Every script F4SE adds to or changes, for 1.10.163 or next-gen (`ng`).
pub fn inventory_for(ng: bool) -> Vec<ScriptDecl> {
    let doc: Json = serde_json::from_str(if ng { INVENTORY_NG } else { INVENTORY }).unwrap_or_default();
    let Some(scripts) = doc["scripts"].as_array() else { return Vec::new() };
    scripts
        .iter()
        .map(|s| ScriptDecl {
            script: s["script"].as_str().unwrap_or_default().to_owned(),
            extends: s["extends"].as_str().unwrap_or_default().to_owned(),
            new_script: s["new_script"].as_bool().unwrap_or(false),
            structs: s["structs"]
                .as_array()
                .map(|a| a.iter().map(|st| StructDecl { name: st["name"].as_str().unwrap_or_default().to_owned(), members: strs(&st["members"]) }).collect())
                .unwrap_or_default(),
            functions: s["functions"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|f| FunctionDecl {
                            name: f["name"].as_str().unwrap_or_default().to_owned(),
                            returns: f["returns"].as_str().unwrap_or("None").to_owned(),
                            params: strs(&f["params"]),
                            global: f["global"].as_bool().unwrap_or(false),
                            kind: f["kind"].as_str().unwrap_or("native").to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            properties: s["properties"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| Some(p["name"].as_str()?.to_owned())).collect())
                .unwrap_or_default(),
        })
        .collect()
}

/// Constant property values: script (lowercase) -> property -> value.
pub type Constants = BTreeMap<String, BTreeMap<String, i32>>;

/// Read `<dir>/<Script>.json` files (`{"Prop": {"value": n, "source": "..."}}`).
pub fn load_constants(dir: &Path) -> Result<Constants, String> {
    let mut out = Constants::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(out) };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let script = path.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let map = out.entry(script).or_default();
        for (name, v) in doc.as_object().into_iter().flatten() {
            let value = v.get("value").and_then(Json::as_i64).or_else(|| v.as_i64());
            if let Some(value) = value {
                map.insert(name.clone(), value as i32);
            }
        }
    }
    Ok(out)
}

/// Read `<dir>/<Script>.json` struct default files (`{"Struct": {"member": {"value": n,
/// "source": "..."}}}`) into the [`Constants`] shape, keyed `Struct.member`, so they can be
/// merged with the constant properties (property names never contain a dot).
pub fn load_struct_defaults(dir: &Path) -> Result<Constants, String> {
    let mut out = Constants::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(out) };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let script = path.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let map = out.entry(script).or_default();
        for (st, members) in doc.as_object().into_iter().flatten() {
            for (member, v) in members.as_object().into_iter().flatten() {
                if let Some(value) = v.get("value").and_then(Json::as_i64).or_else(|| v.as_i64()) {
                    map.insert(format!("{st}.{member}"), value as i32);
                }
            }
        }
    }
    Ok(out)
}

/// The default of `structure.member` in a script's [`Constants`] (case-insensitive).
fn struct_default(values: Option<&BTreeMap<String, i32>>, structure: &str, member: &str) -> Option<i32> {
    let key = format!("{structure}.{member}");
    values?.iter().find(|(k, _)| k.eq_ignore_ascii_case(&key)).map(|(_, v)| *v)
}

fn function(pex: &mut Pex, d: &FunctionDecl) -> Function {
    let params: Vec<(&str, &str)> = d.params.iter().map(|(t, n)| (t.as_str(), n.as_str())).collect();
    let mut flags = if d.kind == "stub" { 0 } else { Function::FLAG_NATIVE };
    if d.global {
        flags |= Function::FLAG_GLOBAL;
    }
    pex.declare_function(&d.returns, &params, flags)
}

fn constant_property(pex: &mut Pex, name: &str, value: i32) -> Property {
    let mut getter = pex.declare_function("Int", &[], 0);
    getter.instructions.push(Instruction { op: Opcode::Return, args: vec![Value::Integer(value)], varargs: Vec::new() });
    Property {
        name: pex.strings.intern_ignore_case(name),
        type_name: pex.strings.intern_ignore_case("Int"),
        doc_string: pex.strings.intern_ignore_case(""),
        user_flags: 0,
        flags: Property::FLAG_READ,
        auto_var_name: None,
        read_handler: Some(getter),
        write_handler: None,
    }
}

fn structure(pex: &mut Pex, d: &StructDecl, values: Option<&BTreeMap<String, i32>>) -> Struct {
    let members = d
        .members
        .iter()
        .map(|(name, ty)| StructMember {
            name: pex.strings.intern_ignore_case(name),
            type_name: pex.strings.intern_ignore_case(ty),
            user_flags: 0,
            default: struct_default(values, &d.name, name).map_or(Value::None, Value::Integer),
            is_const: false,
            doc_string: pex.strings.intern_ignore_case(""),
        })
        .collect();
    Struct { name: pex.strings.intern_ignore_case(&d.name), members }
}

/// Add what `decl` lists and `pex` lacks. Returns the names added.
pub fn patch(pex: &mut Pex, decl: &ScriptDecl, constants: &Constants) -> Result<Vec<String>, String> {
    let script = decl.script.as_str();
    let oi = pex
        .objects
        .iter()
        .position(|o| pex.str(o.name).eq_ignore_ascii_case(script))
        .ok_or_else(|| format!("{script}.pex has no object {script}"))?;
    let si = pex.objects[oi].states.iter().position(|s| pex.str(s.name).is_empty()).ok_or_else(|| format!("{script} has no default state"))?;
    let lower = |pex: &Pex, ids: Vec<crate::StringId>| ids.into_iter().map(|i| pex.str(i).to_ascii_lowercase()).collect::<Vec<_>>();
    let have_fns = lower(pex, pex.objects[oi].states[si].functions.iter().map(|f| f.name).collect());
    let have_props = lower(pex, pex.objects[oi].properties.iter().map(|p| p.name).collect());
    let mut added = Vec::new();
    let values = constants.get(&script.to_ascii_lowercase());

    for d in &decl.structs {
        if let Some(sti) = pex.objects[oi].structs.iter().position(|s| pex.str(s.name).eq_ignore_ascii_case(&d.name)) {
            // Present (an earlier f4x run's, or F4SE's own): only fill defaults that are
            // missing, so a script that already has them is left unchanged.
            let names: Vec<String> = pex.objects[oi].structs[sti].members.iter().map(|m| pex.str(m.name).into_owned()).collect();
            for (mi, member) in names.iter().enumerate() {
                let m = &mut pex.objects[oi].structs[sti].members[mi];
                if m.default == Value::None
                    && let Some(v) = struct_default(values, &d.name, member)
                {
                    m.default = Value::Integer(v);
                    added.push(format!("struct default {}.{member}", d.name));
                }
            }
            continue;
        }
        let st = structure(pex, d, values);
        if let Some(debug) = pex.debug_info.as_mut() {
            debug.struct_orders.push(StructOrder { object_name: pex.objects[oi].name, struct_name: st.name, members: st.members.iter().map(|m| m.name).collect() });
        }
        pex.objects[oi].structs.push(st);
        added.push(format!("struct {}", d.name));
    }
    for d in &decl.functions {
        if have_fns.contains(&d.name.to_ascii_lowercase()) {
            continue;
        }
        let f = function(pex, d);
        let name = pex.strings.intern_ignore_case(&d.name);
        pex.objects[oi].states[si].functions.push(NamedFunction { name, function: f });
        added.push(d.name.clone());
    }
    for name in &decl.properties {
        if have_props.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        let Some(&value) = values.and_then(|v| v.get(name)) else { continue };
        let p = constant_property(pex, name, value);
        pex.objects[oi].properties.push(p);
        added.push(format!("property {name}"));
    }
    Ok(added)
}

/// A new script for `decl` (one F4SE adds), with the header and user-flag table of
/// `template` (a vanilla script: its `hidden` flag is set on the new object).
pub fn create(decl: &ScriptDecl, template: &Pex, constants: &Constants) -> Result<Pex, String> {
    let mut pex = Pex {
        header: Header {
            major_version: template.header.major_version,
            minor_version: template.header.minor_version,
            game_id: template.header.game_id,
            compilation_time: 0,
            source_file_name: PexString::new(&format!("{}.psc", decl.script)),
            user_name: PexString::new("f4x"),
            machine_name: PexString::new("f4x"),
        },
        strings: StringTable { strings: Vec::new() },
        debug_info: None,
        user_flags: Vec::new(),
        objects: Vec::new(),
    };
    let mut hidden = 0u32;
    for flag in &template.user_flags {
        let name = template.str(flag.name).into_owned();
        if name.eq_ignore_ascii_case("hidden") {
            hidden = 1 << flag.bit;
        }
        let id = pex.strings.intern_ignore_case(&name);
        pex.user_flags.push(UserFlag { name: id, bit: flag.bit });
    }
    let empty = pex.strings.intern_ignore_case("");
    let object = Object {
        name: pex.strings.intern_ignore_case(&decl.script),
        parent_class_name: pex.strings.intern_ignore_case(&decl.extends),
        doc_string: empty,
        is_const: false,
        user_flags: hidden,
        auto_state_name: empty,
        structs: Vec::new(),
        variables: Vec::new(),
        properties: Vec::new(),
        states: vec![State { name: empty, functions: Vec::new() }],
        size_excludes_field: false,
    };
    pex.objects.push(object);
    patch(&mut pex, decl, constants)?;
    Ok(pex)
}

/// Compare the declarations with F4SE's scripts in `dir`: every function's return type,
/// parameter types and global flag. Returns mismatches.
pub fn verify(dir: &Path) -> Result<Vec<String>, String> {
    verify_for(dir, false)
}

/// [`verify`] against the 1.10.163 or the next-gen (`ng`) inventory.
pub fn verify_for(dir: &Path, ng: bool) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    for decl in inventory_for(ng) {
        let path = dir.join(format!("{}.pex", decl.script));
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let pex = Pex::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        for d in &decl.functions {
            let found = pex.functions().into_iter().find(|f| pex.str(f.name).eq_ignore_ascii_case(&d.name) && pex.str(f.object.name).eq_ignore_ascii_case(&decl.script));
            let Some(f) = found else {
                problems.push(format!("{}.{}: not in F4SE's script", decl.script, d.name));
                continue;
            };
            let theirs: Vec<String> = f.function.params.iter().map(|p| pex.str(p.type_name).to_ascii_lowercase()).collect();
            let ours: Vec<String> = d.params.iter().map(|(t, _)| t.to_ascii_lowercase()).collect();
            let ret = pex.str(f.function.return_type).to_ascii_lowercase();
            if theirs != ours || ret != d.returns.to_ascii_lowercase() || f.function.is_global() != d.global {
                problems.push(format!("{}.{}: F4SE {ret}({}), f4x {}({})", decl.script, d.name, theirs.join(", "), d.returns, ours.join(", ")));
            }
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_parses() {
        let inv = inventory();
        assert!(inv.len() >= 29);
        let natives: usize = inv.iter().map(|s| s.functions.iter().filter(|f| f.kind == "native").count()).sum();
        assert!(natives >= 220, "{natives}");
        assert!(inv.iter().any(|s| s.script == "UI" && s.new_script));
    }

    #[test]
    fn created_script_round_trips() {
        let template = Pex {
            header: Header {
                major_version: 3,
                minor_version: 9,
                game_id: 2,
                compilation_time: 0,
                source_file_name: PexString::new("x.psc"),
                user_name: PexString::new(""),
                machine_name: PexString::new(""),
            },
            strings: StringTable { strings: vec![PexString::new("hidden")] },
            debug_info: None,
            user_flags: vec![UserFlag { name: crate::StringId(0), bit: 0 }],
            objects: Vec::new(),
        };
        let mut constants = Constants::new();
        constants.entry("instancedata".into()).or_default().insert("Flag_MinorCrime".into(), 7);
        for decl in inventory().iter().filter(|d| d.new_script) {
            let pex = create(decl, &template, &constants).unwrap();
            let bytes = pex.to_bytes().unwrap();
            let back = Pex::parse(&bytes).unwrap();
            assert_eq!(back, pex, "{}", decl.script);
            let funcs = back.functions().len();
            assert_eq!(funcs, decl.functions.len() + back.objects[0].properties.len(), "{}", decl.script);
        }
    }

    #[test]
    fn struct_defaults_are_written_and_filled() {
        let template = Pex {
            header: Header {
                major_version: 3,
                minor_version: 9,
                game_id: 2,
                compilation_time: 0,
                source_file_name: PexString::new("x.psc"),
                user_name: PexString::new(""),
                machine_name: PexString::new(""),
            },
            strings: StringTable { strings: Vec::new() },
            debug_info: None,
            user_flags: Vec::new(),
            objects: Vec::new(),
        };
        let decl = inventory().into_iter().find(|d| d.script == "UI").unwrap();
        let default_of = |pex: &Pex, member: &str| {
            let st = &pex.objects[0].structs[0];
            st.members.iter().find(|m| pex.str(m.name) == member).map(|m| m.default.clone())
        };
        // Without defaults: every member is None (the type's zero).
        let mut pex = create(&decl, &template, &Constants::new()).unwrap();
        assert_eq!(default_of(&pex, "depth"), Some(Value::None));
        // An existing script gets the missing defaults filled, once.
        let mut constants = Constants::new();
        constants.entry("ui".into()).or_default().insert("MenuData.depth".into(), 6);
        let added = patch(&mut pex, &decl, &constants).unwrap();
        assert_eq!(added, vec!["struct default MenuData.depth".to_owned()]);
        assert_eq!(default_of(&pex, "depth"), Some(Value::Integer(6)));
        assert!(patch(&mut pex, &decl, &constants).unwrap().is_empty());
        // A new script has them from the start, and they survive a round trip.
        let pex = create(&decl, &template, &constants).unwrap();
        assert_eq!(default_of(&pex, "depth"), Some(Value::Integer(6)));
        assert_eq!(default_of(&pex, "menuFlags"), Some(Value::None));
        assert_eq!(Pex::parse(&pex.to_bytes().unwrap()).unwrap(), pex);
    }

    #[test]
    fn shipped_struct_defaults_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/struct-defaults");
        let defaults = load_struct_defaults(&dir).unwrap();
        for (script, values) in &defaults {
            let decl = inventory().into_iter().find(|d| d.script.eq_ignore_ascii_case(script)).expect("a known script");
            for key in values.keys() {
                let (st, member) = key.split_once('.').unwrap();
                let s = decl.structs.iter().find(|s| s.name == st).expect("a known struct");
                assert!(s.members.iter().any(|(m, _)| m == member), "{script}: {key}");
            }
        }
    }
}
