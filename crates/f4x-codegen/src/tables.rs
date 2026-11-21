//! `tables`: emit per-build engine offset tables for `f4x-engine-sys` from the clang dumps.
//!
//! For every non-template `RE::` record CommonLibF4 defines:
//! - `SIZE`, `ALIGN`, the mangled RTTI name (`RTTI`), and one `const` per field offset
//!   (documented with the C++ type);
//! - `bases::<Base>` offsets;
//! - `vtbl::<method>` slot indices of the primary vtable plus `vtbl::LEN`.
//!
//! Plus `form_type`: CommonLibF4's `ENUM_FORM_ID` (value, record signature, class).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

/// One class's primary vtable: slot names in order (without the RTTI entry).
pub type Vtables = HashMap<String, Vec<String>>;

pub struct Inputs {
    pub layouts: Vec<Value>,
    pub vtables: Vtables,
    pub form_types: Vec<FormType>,
}

pub struct FormType {
    pub name: String,
    pub value: u8,
    pub signature: String,
    pub class: String,
}

pub fn load(dump: &Path, clf4: &Path) -> Result<Inputs, String> {
    let layouts: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(dump.join("layouts.json")).map_err(|e| format!("layouts.json: {e} (run `dump` first)"))?,
    )
    .map_err(|e| e.to_string())?;
    let vtables_text = std::fs::read_to_string(dump.join("vtables.txt")).map_err(|e| format!("vtables.txt: {e}"))?;
    let primary_bases = primary_vfptr_bases(&layouts);
    let vtables = parse_vtables(&vtables_text, &primary_bases);
    let forms_h = clf4.join("CommonLibF4/include/RE/Bethesda/TESForms.h");
    let form_types = parse_form_types(&std::fs::read_to_string(&forms_h).map_err(|e| format!("{}: {e}", forms_h.display()))?);
    Ok(Inputs { layouts, vtables, form_types })
}

/// For each class, the (unqualified) name of the base that owns the vfptr at offset 0.
fn primary_vfptr_bases(layouts: &[Value]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for layout in layouts {
        let Some(name) = layout["name"].as_str() else { continue };
        if let Some(vfptrs) = layout["vfptrs"].as_array()
            && let Some(first) = vfptrs.iter().find(|v| v["offset"].as_u64() == Some(0))
            && let Some(class) = first["class"].as_str()
        {
            out.insert(name.to_owned(), class.to_owned());
        }
    }
    out
}

/// Parse `-fdump-vtable-layouts` and keep each class's primary (offset-0) vtable.
pub fn parse_vtables(text: &str, primary_bases: &HashMap<String, String>) -> Vtables {
    let mut out: Vtables = HashMap::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("VFTable for '") else { continue };
        // "A' (N entries)." or "A' in 'B' in 'C' (N entries)."
        let names: Vec<&str> = rest.split('\'').step_by(2).take_while(|s| !s.starts_with(" (")).collect();
        let quoted: Vec<&str> = rest.split('\'').enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, s)| s).collect();
        let _ = names;
        let path: Vec<&str> = quoted.iter().copied().filter(|s| !s.trim().is_empty() && !s.starts_with(" (")).collect();
        let (Some(&first), Some(&class)) = (path.first(), path.last()) else { continue };
        let primary = match primary_bases.get(class) {
            Some(base) => last_segment(first) == base.as_str() || first == class,
            None => path.len() == 1,
        };
        let mut slots = Vec::new();
        while let Some(entry) = lines.peek() {
            let Some((index, signature)) = entry.trim_start().split_once(" | ") else { break };
            if index.trim().parse::<usize>().is_err() {
                break;
            }
            slots.push(method_name(signature));
            lines.next();
        }
        if primary && !slots.is_empty() && !out.contains_key(class) {
            // Entry 0 is the RTTI pointer.
            slots.remove(0);
            out.insert(class.to_owned(), slots);
        }
    }
    out
}

fn last_segment(qualified: &str) -> &str {
    qualified.rsplit("::").next().unwrap_or(qualified)
}

/// `const char *RE::TESForm::GetFormEditorID() const` → `GetFormEditorID`;
/// destructors → `dtor`.
fn method_name(signature: &str) -> String {
    let Some(paren) = find_param_list(signature) else { return "unknown".into() };
    let head = &signature[..paren];
    let name = head.rsplit("::").next().unwrap_or(head).trim();
    let name = name.rsplit(' ').next().unwrap_or(name).trim_start_matches(['*', '&']);
    if name.starts_with('~') {
        "dtor".into()
    } else if name.starts_with("operator") {
        "op".into()
    } else {
        name.to_owned()
    }
}

/// Byte index of the '(' that opens the parameter list (the first one outside `<>`).
fn find_param_list(signature: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in signature.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

pub fn parse_form_types(header: &str) -> Vec<FormType> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in header.lines() {
        let t = line.trim();
        if t.starts_with("enum class ENUM_FORM_ID") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if t.starts_with("};") || t.starts_with("kTotal") {
            break;
        }
        // kWEAP,  // 2B WEAP X TESObjectWEAP
        let Some((name, comment)) = t.split_once(',') else { continue };
        let Some(name) = name.trim().strip_prefix('k') else { continue };
        let words: Vec<&str> = comment.trim().trim_start_matches("//").split_whitespace().collect();
        let Some(value) = words.first().and_then(|v| u8::from_str_radix(v, 16).ok()) else { continue };
        out.push(FormType {
            name: name.to_owned(),
            value,
            signature: words.get(1).copied().unwrap_or("").to_owned(),
            class: words.last().copied().unwrap_or("").to_owned(),
        });
    }
    out
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static", "struct", "super", "trait",
    "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
    "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

fn ident(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    let cleaned = if cleaned.chars().next().is_none_or(|c| c.is_ascii_digit()) { format!("_{cleaned}") } else { cleaned };
    if matches!(cleaned.as_str(), "self" | "Self" | "super" | "crate") {
        format!("{cleaned}_")
    } else if KEYWORDS.contains(&cleaned.as_str()) {
        format!("r#{cleaned}")
    } else {
        cleaned
    }
}

/// Mangled MSVC RTTI name of a game class (`RE::` dropped: the game's classes live in
/// the global namespace or their own namespaces).
pub fn rtti_name(qualified: &str, is_struct: bool) -> String {
    let unqualified = qualified.strip_prefix("RE::").unwrap_or(qualified);
    let mut parts: Vec<&str> = unqualified.split("::").collect();
    parts.reverse();
    format!("{}{}@@", if is_struct { ".?AU" } else { ".?AV" }, parts.join("@"))
}

#[derive(Default)]
struct ModuleTree {
    children: BTreeMap<String, ModuleTree>,
    body: String,
}

impl ModuleTree {
    fn at(&mut self, path: &[&str]) -> &mut ModuleTree {
        path.iter().fold(self, |node, segment| node.children.entry(ident(segment)).or_default())
    }

    fn render(&self, out: &mut String, depth: usize) {
        let indent = "    ".repeat(depth);
        for line in self.body.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "{indent}{line}");
            }
        }
        for (name, child) in &self.children {
            let _ = writeln!(out, "{indent}pub mod {name} {{");
            child.render(out, depth + 1);
            let _ = writeln!(out, "{indent}}}");
        }
    }
}

pub struct Generated {
    pub source: String,
    pub classes: usize,
    pub vtables: usize,
}

pub fn generate(inputs: &Inputs, header: &str) -> Generated {
    let mut tree = ModuleTree::default();
    let mut classes = 0;
    let mut vtables = 0;
    let mut seen = HashSet::new();
    for layout in &inputs.layouts {
        let Some(name) = layout["name"].as_str() else { continue };
        if !name.starts_with("RE::") || name.contains('<') || name.contains('(') || !seen.insert(name.to_owned()) {
            continue;
        }
        let path: Vec<&str> = name.split("::").collect();
        let is_struct = layout["kind"].as_str() == Some("struct");
        let mut body = String::new();
        let _ = writeln!(body, "//! `{name}`");
        let _ = writeln!(body, "pub const SIZE: usize = {:#x};", layout["size"].as_u64().unwrap_or(0));
        let _ = writeln!(body, "pub const ALIGN: usize = {};", layout["align"].as_u64().unwrap_or(1));
        let _ = writeln!(body, "pub const RTTI: &str = {:?};", rtti_name(name, is_struct));
        let mut field_names = HashSet::new();
        for field in layout["fields"].as_array().into_iter().flatten() {
            let (Some(fname), Some(offset)) = (field["name"].as_str(), field["offset"].as_u64()) else { continue };
            if fname.is_empty() {
                continue;
            }
            let mut id = ident(fname);
            if matches!(id.as_str(), "SIZE" | "ALIGN" | "RTTI" | "bases" | "vtbl") {
                id = format!("{id}_");
            }
            if !field_names.insert(id.clone()) {
                continue;
            }
            let _ = writeln!(body, "/// `{}`", field["type"].as_str().unwrap_or("?").replace('`', "'"));
            let _ = writeln!(body, "pub const {id}: usize = {offset:#x};");
        }
        let bases: Vec<&Value> = layout["bases"].as_array().into_iter().flatten().filter(|b| b["virtual"] != true).collect();
        if !bases.is_empty() {
            let _ = writeln!(body, "pub mod bases {{");
            let mut base_names = HashSet::new();
            for base in bases {
                let (Some(bname), Some(offset)) = (base["name"].as_str(), base["offset"].as_u64()) else { continue };
                let mut id = ident(last_segment(bname.split('<').next().unwrap_or(bname)));
                while !base_names.insert(id.clone()) {
                    id.push('_');
                }
                let _ = writeln!(body, "    /// `{bname}`");
                let _ = writeln!(body, "    pub const {id}: usize = {offset:#x};");
            }
            let _ = writeln!(body, "}}");
        }
        if let Some(slots) = inputs.vtables.get(name) {
            vtables += 1;
            let _ = writeln!(body, "pub mod vtbl {{");
            let _ = writeln!(body, "    pub const LEN: usize = {};", slots.len());
            let mut counts: HashMap<String, usize> = HashMap::new();
            for (slot, method) in slots.iter().enumerate() {
                let n = counts.entry(method.clone()).or_default();
                *n += 1;
                let mut id = ident(method);
                if *n > 1 {
                    id = format!("{}_{}", id.trim_start_matches("r#"), n);
                }
                if id == "LEN" {
                    id = "LEN_".into();
                }
                let _ = writeln!(body, "    pub const {id}: usize = {slot:#x};");
            }
            let _ = writeln!(body, "}}");
        }
        tree.at(&path).body.push_str(&body);
        classes += 1;
    }

    let mut source = String::from(header);
    tree.render(&mut source, 0);

    let _ = writeln!(source, "/// CommonLibF4 `ENUM_FORM_ID`: `(value, record signature, class)`.");
    let _ = writeln!(source, "pub mod form_type {{");
    for ft in &inputs.form_types {
        let _ = writeln!(source, "    /// `{}` ({})", ft.signature, ft.class);
        let _ = writeln!(source, "    pub const {}: u8 = {:#04x};", ident(&ft.name), ft.value);
    }
    let _ = writeln!(source, "    pub const TABLE: &[(u8, &str, &str)] = &[");
    for ft in &inputs.form_types {
        let _ = writeln!(source, "        ({:#04x}, {:?}, {:?}),", ft.value, ft.signature, ft.class);
    }
    let _ = writeln!(source, "    ];\n}}");
    Generated { source, classes, vtables }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_mangling() {
        assert_eq!(method_name("const char *RE::TESForm::GetFormEditorID() const"), "GetFormEditorID");
        assert_eq!(method_name("RE::TESForm::~TESForm() [vector deleting]"), "dtor");
        assert_eq!(method_name("void RE::BaseFormComponent::CopyComponent(BaseFormComponent *, TESForm *) [pure]"), "CopyComponent");
        assert_eq!(method_name("BSTArray<int> RE::Foo::Bar(int)"), "Bar");
        assert_eq!(rtti_name("RE::BSScript::Internal::VirtualMachine", false), ".?AVVirtualMachine@Internal@BSScript@@");
        assert_eq!(rtti_name("RE::NiPoint3", true), ".?AUNiPoint3@@");
        assert_eq!(ident("type"), "r#type");
        assert_eq!(ident("self"), "self_");
        assert_eq!(ident("3d"), "_3d");
    }

    #[test]
    fn parses_primary_vtables() {
        let text = "VFTable for 'RE::BaseFormComponent' in 'RE::TESForm' (3 entries).\n   0 | RE::TESForm RTTI\n   1 | RE::TESForm::~TESForm() [vector deleting]\n   2 | const char *RE::TESForm::GetFormEditorID() const\n\nVFTable for 'RE::Other' in 'RE::TESForm' (2 entries).\n   0 | RE::TESForm RTTI\n   1 | void RE::Other::X()\n";
        let mut primary = HashMap::new();
        primary.insert("RE::TESForm".to_owned(), "BaseFormComponent".to_owned());
        let v = parse_vtables(text, &primary);
        assert_eq!(v["RE::TESForm"], ["dtor", "GetFormEditorID"]);
    }

    #[test]
    fn parses_form_types() {
        let h = "enum class ENUM_FORM_ID\n{\n  kNONE,  // 00 NONE X TESForm\n  kWEAP,  // 2B WEAP X TESObjectWEAP\n  kTotal\n};";
        let f = parse_form_types(h);
        assert_eq!(f.len(), 2);
        assert_eq!((f[1].name.as_str(), f[1].value, f[1].class.as_str()), ("WEAP", 0x2B, "TESObjectWEAP"));
    }
}
