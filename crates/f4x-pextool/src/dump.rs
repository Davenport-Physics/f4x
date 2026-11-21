//! `pex dump`: a readable, Papyrus-like listing of a script's declarations.

use std::fmt::Write as _;
use std::process::ExitCode;

use f4x_pex::{Function, FunctionKind, FunctionRef, Instruction, Pex, Value};

use crate::{CliResult, emit, open_archive};

pub fn run(args: &[&str]) -> CliResult {
    let (asm, rest) = match args {
        ["--asm", rest @ ..] => (true, rest),
        rest => (false, rest),
    };
    let (label, bytes) = match rest {
        ["--ba2", archive, entry] => {
            let archive = open_archive(archive)?;
            let found = archive.find(entry).ok_or_else(|| format!("{entry}: not in the archive"))?;
            (found.name.clone(), archive.extract(found).map_err(|e| format!("{entry}: {e}"))?)
        }
        [file] => (file.to_string(), std::fs::read(file).map_err(|e| format!("{file}: {e}"))?),
        _ => {
            return Err(
                "usage: pex dump [--asm] <file.pex> | pex dump [--asm] --ba2 <archive> <entry>".into()
            );
        }
    };
    let pex = Pex::parse(&bytes).map_err(|e| format!("{label}: {e}"))?;
    emit(&render(&pex, asm))?;
    Ok(ExitCode::SUCCESS)
}

fn flags_suffix(pex: &Pex, user_flags: u32) -> String {
    pex.user_flag_names(user_flags).iter().map(|n| format!(" {n}")).collect()
}

fn value(pex: &Pex, v: &Value) -> String {
    match v {
        Value::None => "None".into(),
        Value::Identifier(id) => pex.str(*id).into_owned(),
        Value::String(id) => format!("{:?}", pex.str(*id)),
        Value::Integer(n) => n.to_string(),
        Value::Float(x) => format!("{x:?}"),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
    }
}

/// Renders the whole script.
pub fn render(pex: &Pex, asm: bool) -> String {
    let mut out = String::new();
    let h = &pex.header;
    let _ = writeln!(
        out,
        "; {} (pex {}.{}, game {}, compiled {}, {} strings{})",
        h.source_file_name,
        h.major_version,
        h.minor_version,
        h.game_id,
        h.compilation_time,
        pex.strings.len(),
        if pex.debug_info.is_some() { ", debug info" } else { "" }
    );
    for object in &pex.objects {
        let parent = pex.str(object.parent_class_name);
        let _ = write!(out, "ScriptName {}", pex.str(object.name));
        if !parent.is_empty() {
            let _ = write!(out, " extends {parent}");
        }
        if object.is_const {
            out.push_str(" Const");
        }
        let _ = writeln!(out, "{}", flags_suffix(pex, object.user_flags));
        doc(&mut out, pex, object.doc_string, "");

        for s in &object.structs {
            let _ = writeln!(out, "\nStruct {}", pex.str(s.name));
            for m in &s.members {
                let _ = write!(
                    out,
                    "  {} {} = {}",
                    pex.str(m.type_name),
                    pex.str(m.name),
                    value(pex, &m.default)
                );
                if m.is_const {
                    out.push_str(" Const");
                }
                let _ = writeln!(out, "{}", flags_suffix(pex, m.user_flags));
                doc(&mut out, pex, m.doc_string, "  ");
            }
            out.push_str("EndStruct\n");
        }

        if !object.variables.is_empty() {
            out.push('\n');
        }
        for v in &object.variables {
            let _ = write!(out, "{} {} = {}", pex.str(v.type_name), pex.str(v.name), value(pex, &v.default));
            if v.is_const {
                out.push_str(" Const");
            }
            let _ = writeln!(out, "{}", flags_suffix(pex, v.user_flags));
        }

        if !object.properties.is_empty() {
            out.push('\n');
        }
        // Functions of this object only (a file can, in principle, hold several).
        let functions: Vec<_> =
            pex.functions().into_iter().filter(|f| std::ptr::eq(f.object, object)).collect();
        for p in &object.properties {
            let _ = write!(out, "{} Property {}", pex.str(p.type_name), pex.str(p.name));
            if let Some(var) = p.auto_var_name {
                let backing_const = object.variables.iter().any(|v| v.name == var && v.is_const);
                out.push_str(if backing_const { " Auto Const" } else { " Auto" });
                let _ = writeln!(out, "{} ; var {}", flags_suffix(pex, p.user_flags), pex.str(var));
                doc(&mut out, pex, p.doc_string, "  ");
                continue;
            }
            let _ = writeln!(out, "{}", flags_suffix(pex, p.user_flags));
            doc(&mut out, pex, p.doc_string, "  ");
            for f in functions.iter().filter(|f| f.kind != FunctionKind::State && f.owner == p.name) {
                function(&mut out, pex, f, asm, "  ");
            }
            out.push_str("EndProperty\n");
        }

        if let Some(debug) = &pex.debug_info {
            for g in debug.property_groups.iter().filter(|g| g.object_name == object.name) {
                let names: Vec<_> = g.properties.iter().map(|&id| pex.str(id)).collect();
                let _ = writeln!(
                    out,
                    "; Group {:?}{}: {}",
                    pex.str(g.group_name),
                    flags_suffix(pex, g.user_flags),
                    names.join(", ")
                );
            }
            for o in debug.struct_orders.iter().filter(|o| o.object_name == object.name) {
                let names: Vec<_> = o.members.iter().map(|&id| pex.str(id)).collect();
                let _ = writeln!(out, "; Struct order {}: {}", pex.str(o.struct_name), names.join(", "));
            }
        }

        let auto_state = pex.str(object.auto_state_name);
        for state in &object.states {
            let name = pex.str(state.name);
            if name.is_empty() {
                out.push_str("\n; default state\n");
            } else {
                let auto = if name.eq_ignore_ascii_case(&auto_state) { "Auto " } else { "" };
                let _ = writeln!(out, "\n{auto}State {name}");
            }
            for f in functions.iter().filter(|f| f.kind == FunctionKind::State && f.owner == state.name) {
                function(&mut out, pex, f, asm, "  ");
            }
            if !name.is_empty() {
                out.push_str("EndState\n");
            }
        }
    }
    out
}

fn doc(out: &mut String, pex: &Pex, id: f4x_pex::StringId, indent: &str) {
    let text = pex.str(id);
    if !text.is_empty() {
        let _ = writeln!(out, "{indent}{{ {} }}", text.replace('\n', " "));
    }
}

fn function(out: &mut String, pex: &Pex, f: &FunctionRef<'_>, asm: bool, indent: &str) {
    let sig = pex.signature(f);
    let _ = writeln!(out, "{indent}{sig}{}", flags_suffix(pex, f.function.user_flags));
    if asm {
        instructions(out, pex, f.function, &format!("{indent}    "));
    }
}

fn instructions(out: &mut String, pex: &Pex, f: &Function, indent: &str) {
    for local in &f.locals {
        let _ = writeln!(out, "{indent}.local {} {}", pex.str(local.type_name), pex.str(local.name));
    }
    for (i, Instruction { op, args, varargs }) in f.instructions.iter().enumerate() {
        let mut line: Vec<String> = args.iter().map(|v| value(pex, v)).collect();
        if op.has_varargs() {
            line.push(format!("[{}]", varargs.iter().map(|v| value(pex, v)).collect::<Vec<_>>().join(", ")));
        }
        let _ = writeln!(out, "{indent}{i:>4}: {op} {}", line.join(", "));
    }
}
