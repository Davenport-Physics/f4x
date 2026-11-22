//! The F4SE-compatible functions f4x implements, as script declarations.
//!
//! Mods compiled against F4SE call functions such as `EncounterZone.IsWorkshop()`. The
//! VM resolves those by name in the script's own type, so the script must declare them
//! native, and must declare the event stubs (`OnControlDown`) that registrations call.
//! F4SE ships its own edited copies of the vanilla scripts. f4x does not use or
//! redistribute them: [`patch`] adds these declarations to the user's own vanilla
//! scripts.
//!
//! Signatures come from F4SE's distributed `.pex` (names and types only; provenance tag
//! `f4se-pex`), and [`verify`] checks them against those files. The list must match the
//! natives f4x binds (`f4x-runtime`'s `f4se::natives`).

use std::path::Path;

use crate::{Function, NamedFunction, Pex};

/// One function to declare: native, or an empty event stub.
pub struct Declaration {
    pub script: &'static str,
    pub name: &'static str,
    /// `None` for no return value.
    pub returns: &'static str,
    /// `(type, name)`.
    pub params: &'static [(&'static str, &'static str)],
    pub native: bool,
}

const fn native(script: &'static str, returns: &'static str, name: &'static str, params: &'static [(&'static str, &'static str)]) -> Declaration {
    Declaration { script, name, returns, params, native: true }
}

const fn event(script: &'static str, name: &'static str, params: &'static [(&'static str, &'static str)]) -> Declaration {
    Declaration { script, name, returns: "None", params, native: false }
}

pub const DECLARATIONS: &[Declaration] = &[
    native("EncounterZone", "None", "SetWorkshop", &[("Bool", "ws")]),
    native("EncounterZone", "Bool", "IsWorkshop", &[]),
    native("EncounterZone", "None", "SetNeverResetable", &[("Bool", "resetable")]),
    native("EncounterZone", "Bool", "IsNeverResetable", &[]),
    native("Location", "EncounterZone", "GetEncounterZone", &[("Bool", "recursive")]),
    native("ScriptObject", "None", "RegisterForControl", &[("String", "control")]),
    native("ScriptObject", "None", "UnregisterForControl", &[("String", "control")]),
    event("ScriptObject", "OnControlDown", &[("String", "control")]),
    event("ScriptObject", "OnControlUp", &[("String", "control"), ("Float", "time")]),
];

/// Add the declarations for `script` that it lacks. Returns the names added.
pub fn patch(pex: &mut Pex, script: &str) -> Result<Vec<&'static str>, String> {
    let wanted: Vec<&Declaration> = DECLARATIONS.iter().filter(|d| d.script.eq_ignore_ascii_case(script)).collect();
    let object_index = pex
        .objects
        .iter()
        .position(|o| pex.str(o.name).eq_ignore_ascii_case(script))
        .ok_or_else(|| format!("{script}.pex has no object {script}"))?;
    let state_index = pex.objects[object_index]
        .states
        .iter()
        .position(|s| pex.str(s.name).is_empty())
        .ok_or_else(|| format!("{script} has no default state"))?;
    let existing: Vec<String> = pex.objects[object_index].states[state_index]
        .functions
        .iter()
        .map(|f| pex.str(f.name).to_ascii_lowercase())
        .collect();
    let mut added = Vec::new();
    for d in wanted {
        if existing.contains(&d.name.to_ascii_lowercase()) {
            continue;
        }
        let function = pex.declare_function(d.returns, d.params, if d.native { Function::FLAG_NATIVE } else { 0 });
        let name = pex.strings.intern_ignore_case(d.name);
        pex.objects[object_index].states[state_index].functions.push(NamedFunction { name, function });
        added.push(d.name);
    }
    Ok(added)
}

pub fn scripts() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = DECLARATIONS.iter().map(|d| d.script).collect();
    out.dedup();
    out
}

/// Compare every declaration with the same function in F4SE's scripts (`dir`): return
/// type, parameter types (case-insensitive) and the native flag. Returns mismatches.
pub fn verify(dir: &Path) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    for script in scripts() {
        let path = dir.join(format!("{script}.pex"));
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let pex = Pex::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        for d in DECLARATIONS.iter().filter(|d| d.script == script) {
            let found = pex.functions().into_iter().find(|f| pex.str(f.name).eq_ignore_ascii_case(d.name) && pex.str(f.object.name).eq_ignore_ascii_case(script));
            let Some(f) = found else {
                problems.push(format!("{script}.{}: not in F4SE's script", d.name));
                continue;
            };
            let theirs: Vec<String> = f.function.params.iter().map(|p| pex.str(p.type_name).to_ascii_lowercase()).collect();
            let ours: Vec<String> = d.params.iter().map(|(t, _)| t.to_ascii_lowercase()).collect();
            let ret = pex.str(f.function.return_type).to_ascii_lowercase();
            if theirs != ours || ret != d.returns.to_ascii_lowercase() || f.function.is_native() != d.native {
                problems.push(format!(
                    "{script}.{}: F4SE {ret}({}) native={}, f4x {}({}) native={}",
                    d.name,
                    theirs.join(", "),
                    f.function.is_native(),
                    d.returns,
                    ours.join(", "),
                    d.native
                ));
            }
        }
    }
    Ok(problems)
}

