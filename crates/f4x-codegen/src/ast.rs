//! clang `-ast-dump=json` handling.
//!
//! - [`split_stream`] turns clang's output (with `-ast-dump-filter`, a stream of
//!   concatenated pretty-printed JSON values) into minified top-level values.
//! - [`Node`] is a typed view of the few node attributes the generator needs; serde
//!   skips the rest.
//! - [`LocTracker`] undoes clang's location compression: a `loc`/`range` object only
//!   carries `file` (and `line`) when it differs from the previously *printed*
//!   location, so positions can only be recovered by visiting nodes in document
//!   order (`loc`, then `range.begin`, `range.end`, then `inner`).
//! - [`Walker`] runs the S2 analyses: `RE::` record definitions, `static_assert`
//!   sites, and `REL::ID` uses with the `REL::Relocation<T>` type they feed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Stream splitting

/// Incremental splitter for a stream of concatenated JSON objects/arrays. It drops
/// all whitespace outside strings, so each emitted value is minified JSON.
#[derive(Default)]
pub struct Splitter {
    buf: Vec<u8>,
    depth: u32,
    in_string: bool,
    escape: bool,
    offset: u64,
}

impl Splitter {
    pub fn feed(
        &mut self,
        chunk: &[u8],
        emit: &mut dyn FnMut(Vec<u8>) -> Result<(), String>,
    ) -> Result<(), String> {
        for &b in chunk {
            self.offset += 1;
            if self.in_string {
                self.buf.push(b);
                if self.escape {
                    self.escape = false;
                } else if b == b'\\' {
                    self.escape = true;
                } else if b == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            if matches!(b, b' ' | b'\n' | b'\r' | b'\t') {
                continue;
            }
            if self.depth == 0 && b != b'{' && b != b'[' {
                return Err(format!(
                    "unexpected byte {:?} between top-level JSON values at offset {}",
                    b as char,
                    self.offset - 1
                ));
            }
            self.buf.push(b);
            match b {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        emit(std::mem::take(&mut self.buf))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.depth != 0 || self.in_string {
            return Err(format!("JSON stream truncated after {} bytes", self.offset));
        }
        Ok(())
    }
}

/// Reads `r` to the end, calling `emit` with each minified top-level value. Returns
/// the number of bytes read.
pub fn split_stream(
    mut r: impl Read,
    mut emit: impl FnMut(Vec<u8>) -> Result<(), String>,
) -> Result<u64, String> {
    let mut splitter = Splitter::default();
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("reading clang output: {e}")),
        };
        splitter.feed(&chunk[..n], &mut emit)?;
    }
    splitter.finish()?;
    Ok(splitter.offset)
}

/// Writes `{"kind":"TranslationUnitDecl","inner":[v1,v2,...]}` from minified values.
pub struct AstWriter<W: Write> {
    out: W,
    count: usize,
}

impl<W: Write> AstWriter<W> {
    pub fn new(mut out: W) -> io::Result<Self> {
        out.write_all(br#"{"kind":"TranslationUnitDecl","inner":["#)?;
        Ok(Self { out, count: 0 })
    }

    pub fn write_value(&mut self, value: &[u8]) -> io::Result<()> {
        if self.count > 0 {
            self.out.write_all(b",")?;
        }
        self.out.write_all(value)?;
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<usize> {
        self.out.write_all(b"]}\n")?;
        self.out.flush()?;
        Ok(self.count)
    }
}

// ---------------------------------------------------------------------------
// Node model

/// One AST node. Deserialized by hand because clang repeats keys on some nodes
/// (`UsingType` carries two `type` attributes); the last occurrence wins.
#[derive(Debug, Default)]
pub struct Node {
    pub kind: String,
    pub name: Option<String>,
    pub loc: Option<Loc>,
    pub range: Option<Range>,
    pub ty: Option<TypeInfo>,
    /// Literal value: a string for `IntegerLiteral`, a number or bool for others.
    pub value: Option<serde_json::Value>,
    pub tag_used: Option<String>,
    pub complete_definition: bool,
    pub is_implicit: bool,
    pub storage_class: Option<String>,
    pub inner: Vec<Node>,
}

enum NodeKey {
    Kind,
    Name,
    Loc,
    Range,
    Type,
    Value,
    TagUsed,
    CompleteDefinition,
    IsImplicit,
    StorageClass,
    Inner,
    Other,
}

impl<'de> Deserialize<'de> for NodeKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = NodeKey;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an AST node attribute name")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<NodeKey, E> {
                Ok(match s {
                    "kind" => NodeKey::Kind,
                    "name" => NodeKey::Name,
                    "loc" => NodeKey::Loc,
                    "range" => NodeKey::Range,
                    "type" => NodeKey::Type,
                    "value" => NodeKey::Value,
                    "tagUsed" => NodeKey::TagUsed,
                    "completeDefinition" => NodeKey::CompleteDefinition,
                    "isImplicit" => NodeKey::IsImplicit,
                    "storageClass" => NodeKey::StorageClass,
                    "inner" => NodeKey::Inner,
                    _ => NodeKey::Other,
                })
            }
        }
        d.deserialize_identifier(V)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Node;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an AST node object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut n = Node::default();
                while let Some(key) = map.next_key::<NodeKey>()? {
                    match key {
                        NodeKey::Kind => n.kind = map.next_value()?,
                        NodeKey::Name => n.name = map.next_value()?,
                        NodeKey::Loc => n.loc = map.next_value()?,
                        NodeKey::Range => n.range = map.next_value()?,
                        NodeKey::Type => n.ty = map.next_value()?,
                        NodeKey::Value => n.value = map.next_value()?,
                        NodeKey::TagUsed => n.tag_used = map.next_value()?,
                        NodeKey::CompleteDefinition => n.complete_definition = map.next_value()?,
                        NodeKey::IsImplicit => n.is_implicit = map.next_value()?,
                        NodeKey::StorageClass => n.storage_class = map.next_value()?,
                        NodeKey::Inner => n.inner = map.next_value()?,
                        NodeKey::Other => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(n)
            }
        }
        d.deserialize_map(V)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TypeInfo {
    pub qual_type: String,
    pub desugared_qual_type: Option<String>,
}

impl TypeInfo {
    /// The desugared spelling when clang printed one, else the written one.
    pub fn canonical(&self) -> &str {
        self.desugared_qual_type.as_deref().unwrap_or(&self.qual_type)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BareLoc {
    pub offset: Option<u64>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Loc {
    pub offset: Option<u64>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub spelling_loc: Option<BareLoc>,
    pub expansion_loc: Option<BareLoc>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Range {
    pub begin: Loc,
    pub end: Loc,
}

/// Parses one minified top-level value. Deep trees need a large stack: call this
/// (and drop the result) on a thread with a generous stack size.
pub fn parse_node(buf: &[u8]) -> Result<Node, String> {
    let mut de = serde_json::Deserializer::from_slice(buf);
    de.disable_recursion_limit();
    let node = Node::deserialize(&mut de).map_err(|e| format!("AST JSON: {e}"))?;
    de.end().map_err(|e| format!("AST JSON: {e}"))?;
    Ok(node)
}

// ---------------------------------------------------------------------------
// Locations

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pos {
    pub file: Rc<str>,
    pub line: u32,
}

/// Replays clang's `LastLocFilename`/`LastLocLine` state. One tracker per
/// independently dumped top-level value (clang starts each with a fresh dumper).
#[derive(Default)]
pub struct LocTracker {
    file: Option<Rc<str>>,
    line: u32,
}

impl LocTracker {
    fn bare(&mut self, offset: Option<u64>, file: Option<&String>, line: Option<u32>) -> Option<Pos> {
        // An invalid location prints as `{}` and does not touch the state.
        offset?;
        if let Some(file) = file {
            self.file = Some(Rc::from(file.as_str()));
        }
        if let Some(line) = line {
            self.line = line;
        }
        Some(Pos { file: self.file.clone()?, line: self.line })
    }

    /// Feeds one `loc`-shaped object and returns its expansion position.
    pub fn track(&mut self, loc: &Loc) -> Option<Pos> {
        if loc.spelling_loc.is_some() || loc.expansion_loc.is_some() {
            // clang writes the spelling location first, then the expansion location.
            if let Some(s) = &loc.spelling_loc {
                self.bare(s.offset, s.file.as_ref(), s.line);
            }
            return loc.expansion_loc.as_ref().and_then(|e| self.bare(e.offset, e.file.as_ref(), e.line));
        }
        self.bare(loc.offset, loc.file.as_ref(), loc.line)
    }
}

// ---------------------------------------------------------------------------
// Analyses

#[derive(Debug, Default, Clone, Serialize)]
pub struct AssertSite {
    /// Seen outside an uninstantiated template, so clang evaluated it.
    pub evaluated: bool,
    pub sizeof: bool,
    pub offsetof: bool,
    pub occurrences: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelocKind {
    /// `REL::ID(n)` feeding a `REL::Relocation<T>`.
    Relocation,
    /// A bare `REL::ID` (the RTTI/VTABLE ID tables, `REL::ID` members, ...).
    Id,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reloc {
    pub id: u64,
    pub kind: RelocKind,
    /// `T` of the `REL::Relocation<T>`, desugared, e.g. `bool (RE::TESDataHandler::*)(bool)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    /// Qualified name of the enclosing function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Its type as written (`bool (bool)`; no `this`, no class).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_type: Option<String>,
    /// Qualified name of the enclosing variable or field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
    /// Path relative to the CommonLibF4 checkout.
    pub file: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_template: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default)]
pub struct Analysis {
    /// `"NamespaceDecl RE"` → number of top-level blocks kept.
    pub top_level: BTreeMap<String, usize>,
    pub nodes: u64,
    /// Complete, non-template, non-local record definitions under `RE::`.
    pub re_records: BTreeSet<String>,
    /// `file:line` (relative to the CommonLibF4 checkout) → site.
    pub static_asserts: BTreeMap<String, AssertSite>,
    pub relocs: Vec<Reloc>,
    /// `REL::ID` constructions whose argument is not an integer literal.
    pub non_literal_ids: usize,
}

impl Analysis {
    pub fn merge(&mut self, other: Analysis) {
        for (k, v) in other.top_level {
            *self.top_level.entry(k).or_default() += v;
        }
        self.nodes += other.nodes;
        self.re_records.extend(other.re_records);
        for (k, v) in other.static_asserts {
            let site = self.static_asserts.entry(k).or_default();
            site.evaluated |= v.evaluated;
            site.sizeof |= v.sizeof;
            site.offsetof |= v.offsetof;
            site.occurrences += v.occurrences;
        }
        self.relocs.extend(other.relocs);
        self.non_literal_ids += other.non_literal_ids;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tmpl {
    /// Ordinary code.
    None,
    /// Inside an instantiation (evaluated, but a copy of a pattern).
    Spec,
    /// Inside an uninstantiated template pattern (dependent, not evaluated).
    Pattern,
}

#[derive(Clone, Copy)]
struct Ctx {
    tmpl: Tmpl,
    in_function: bool,
    /// Under a `UsingShadowDecl`: clang re-dumps the target declaration of
    /// `using X::Y;` in full there. Walk it (for location state) but record nothing.
    alias: bool,
}

struct FnFrame {
    name: String,
    ty: String,
}

struct IdFrame {
    found: bool,
}

/// Walks top-level values and accumulates an [`Analysis`].
pub struct Walker<'a> {
    out: &'a mut Analysis,
    /// Absolute path prefix of the CommonLibF4 checkout, with a trailing `/`.
    clf4_root: &'a str,
    loc: LocTracker,
    scopes: Vec<String>,
    functions: Vec<FnFrame>,
    vars: Vec<String>,
    reloc_types: Vec<String>,
    ids: Vec<IdFrame>,
}

const FUNCTION_KINDS: &[&str] =
    &["FunctionDecl", "CXXMethodDecl", "CXXConstructorDecl", "CXXDestructorDecl", "CXXConversionDecl"];

const TEMPLATE_KINDS: &[&str] = &[
    "ClassTemplateDecl",
    "FunctionTemplateDecl",
    "VarTemplateDecl",
    "TypeAliasTemplateDecl",
    "ConceptDecl",
    "TemplateTemplateParmDecl",
];

const RELOCATION_PREFIX: &str = "REL::Relocation<";

impl<'a> Walker<'a> {
    pub fn new(out: &'a mut Analysis, clf4_root: &'a str) -> Self {
        Self {
            out,
            clf4_root,
            loc: LocTracker::default(),
            scopes: Vec::new(),
            functions: Vec::new(),
            vars: Vec::new(),
            reloc_types: Vec::new(),
            ids: Vec::new(),
        }
    }

    /// Analyses one independently dumped top-level value.
    pub fn top_level(&mut self, node: &Node) {
        let key = format!("{} {}", node.kind, node.name.as_deref().unwrap_or(""));
        *self.out.top_level.entry(key).or_default() += 1;
        self.loc = LocTracker::default();
        self.visit(node, Ctx { tmpl: Tmpl::None, in_function: false, alias: false });
    }

    fn rel_path(&self, pos: &Pos) -> Option<String> {
        pos.file.strip_prefix(self.clf4_root).map(str::to_string)
    }

    fn qualify(&self, name: &str) -> String {
        let mut q = self.scopes.join("::");
        if !q.is_empty() {
            q.push_str("::");
        }
        q.push_str(name);
        q
    }

    fn visit(&mut self, n: &Node, ctx: Ctx) {
        self.out.nodes += 1;
        let at_loc = n.loc.as_ref().and_then(|l| self.loc.track(l));
        let at_begin = n.range.as_ref().and_then(|r| self.loc.track(&r.begin));
        if let Some(r) = &n.range {
            self.loc.track(&r.end);
        }
        let pos = at_loc.or(at_begin);
        let kind = n.kind.as_str();
        let name = n.name.as_deref().unwrap_or("");

        let mut scope_pushed = false;
        let mut fn_pushed = false;
        let mut var_pushed = false;
        let mut reloc_pushed = false;
        let mut id_pushed = false;
        let mut child_ctx = ctx;

        match kind {
            "NamespaceDecl" => {
                self.scopes.push(if name.is_empty() { "(anonymous namespace)".into() } else { name.into() });
                scope_pushed = true;
            }
            "CXXRecordDecl"
            | "ClassTemplateSpecializationDecl"
            | "ClassTemplatePartialSpecializationDecl"
                if n.complete_definition && !n.is_implicit =>
            {
                if kind == "CXXRecordDecl"
                    && !name.is_empty()
                    && ctx.tmpl == Tmpl::None
                    && !ctx.in_function
                    && !ctx.alias
                    && self.scopes.first().is_some_and(|s| s == "RE")
                {
                    let q = self.qualify(name);
                    self.out.re_records.insert(q);
                }
                self.scopes.push(if name.is_empty() { "(anonymous)".into() } else { name.into() });
                scope_pushed = true;
            }
            k if FUNCTION_KINDS.contains(&k) => {
                let ty = n.ty.as_ref().map(|t| t.qual_type.clone()).unwrap_or_default();
                self.functions.push(FnFrame { name: self.qualify(name), ty });
                fn_pushed = true;
                self.scopes.push(name.into());
                scope_pushed = true;
                child_ctx.in_function = true;
            }
            "UsingShadowDecl" | "ConstructorUsingShadowDecl" => child_ctx.alias = true,
            "VarDecl" | "FieldDecl" => {
                self.vars.push(self.qualify(name));
                var_pushed = true;
            }
            "StaticAssertDecl" => {
                if !ctx.alias
                    && let Some(rel) = pos.as_ref().and_then(|p| self.rel_path(p).map(|r| (r, p.line)))
                {
                    let site = self.out.static_asserts.entry(format!("{}:{}", rel.0, rel.1)).or_default();
                    site.occurrences += 1;
                    site.evaluated |= ctx.tmpl != Tmpl::Pattern;
                    if let Some(cond) = n.inner.first() {
                        site.sizeof |= contains(cond, &|c| {
                            c.kind == "UnaryExprOrTypeTraitExpr" && c.name.as_deref() == Some("sizeof")
                        });
                        site.offsetof |= contains(cond, &|c| c.kind == "OffsetOfExpr");
                    }
                }
            }
            "IntegerLiteral" => {
                if let Some(frame) = self.ids.last_mut()
                    && !frame.found
                {
                    frame.found = true;
                    let id = n.value.as_ref().and_then(|v| match v {
                        serde_json::Value::String(s) => s.parse::<u64>().ok(),
                        serde_json::Value::Number(x) => x.as_u64(),
                        _ => None,
                    });
                    if let (Some(id), Some(pos)) = (id, pos.as_ref()) {
                        self.record_id(id, pos, ctx);
                    }
                }
            }
            _ => {}
        }

        if let Some(ty) = &n.ty {
            let canonical = strip_const(ty.canonical());
            if let Some(arg) = canonical.strip_prefix(RELOCATION_PREFIX).and_then(|a| a.strip_suffix('>')) {
                self.reloc_types.push(arg.trim().to_string());
                reloc_pushed = true;
            }
            if kind == "CXXConstructExpr"
                && (canonical == "REL::ID" || strip_const(&ty.qual_type) == "REL::ID")
            {
                self.ids.push(IdFrame { found: false });
                id_pushed = true;
            }
        }

        let template_decl = TEMPLATE_KINDS.contains(&kind);
        let mut seen_pattern = false;
        for child in &n.inner {
            let mut c = child_ctx;
            c.tmpl = if ctx.tmpl == Tmpl::Pattern || kind.ends_with("PartialSpecializationDecl") {
                Tmpl::Pattern
            } else if matches!(kind, "ClassTemplateSpecializationDecl" | "VarTemplateSpecializationDecl") {
                // Also explicit instantiations (`extern template class X<1>;`), which
                // clang dumps at namespace scope rather than under the template.
                Tmpl::Spec
            } else if template_decl {
                match child.kind.as_str() {
                    "ClassTemplateSpecializationDecl" | "VarTemplateSpecializationDecl" => Tmpl::Spec,
                    k if kind == "FunctionTemplateDecl" && FUNCTION_KINDS.contains(&k) => {
                        // The first function child is the pattern, later ones are
                        // instantiations.
                        if seen_pattern {
                            Tmpl::Spec
                        } else {
                            seen_pattern = true;
                            Tmpl::Pattern
                        }
                    }
                    _ => Tmpl::Pattern,
                }
            } else {
                ctx.tmpl
            };
            self.visit(child, c);
        }

        if id_pushed
            && let Some(frame) = self.ids.pop()
            && !frame.found
            && !ctx.alias
        {
            self.out.non_literal_ids += 1;
        }
        if reloc_pushed {
            self.reloc_types.pop();
        }
        if var_pushed {
            self.vars.pop();
        }
        if fn_pushed {
            self.functions.pop();
        }
        if scope_pushed {
            self.scopes.pop();
        }
    }

    fn record_id(&mut self, id: u64, pos: &Pos, ctx: Ctx) {
        if ctx.alias {
            return;
        }
        let Some(file) = self.rel_path(pos) else { return };
        let target_type = self.reloc_types.last().cloned();
        let function = self.functions.last();
        self.out.relocs.push(Reloc {
            id,
            kind: if target_type.is_some() { RelocKind::Relocation } else { RelocKind::Id },
            target_type,
            function: function.map(|f| f.name.clone()),
            function_type: function.map(|f| f.ty.clone()),
            var: self.vars.last().cloned(),
            file,
            line: pos.line,
            in_template: ctx.tmpl != Tmpl::None,
        });
    }
}

fn strip_const(s: &str) -> &str {
    s.strip_prefix("const ").unwrap_or(s)
}

fn contains(n: &Node, pred: &dyn Fn(&Node) -> bool) -> bool {
    pred(n) || n.inner.iter().any(|c| contains(c, pred))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_minifies_and_splits() {
        let input = b"{\n  \"a\": \"x y \\\" }\",\n  \"b\": [1, 2]\n}\n{\n  \"c\": {}\n}\n";
        let mut values = Vec::new();
        let n = split_stream(&input[..], |v| {
            values.push(String::from_utf8(v).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(n, input.len() as u64);
        assert_eq!(values, [r#"{"a":"x y \" }","b":[1,2]}"#, r#"{"c":{}}"#]);
    }

    #[test]
    fn splitter_rejects_garbage_and_truncation() {
        assert!(split_stream(&b"{} exit=0"[..], |_| Ok(())).is_err());
        assert!(split_stream(&b"{\"a\": [1"[..], |_| Ok(())).is_err());
    }

    #[test]
    fn node_tolerates_duplicate_keys() {
        let json = br#"{"kind":"UsingType","type":{"qualType":"std::uintptr_t"},
            "type":{"desugaredQualType":"unsigned long long","qualType":"::uintptr_t"},"extra":[{"x":1}]}"#;
        let node = parse_node(json).unwrap();
        assert_eq!(node.kind, "UsingType");
        assert_eq!(node.ty.unwrap().canonical(), "unsigned long long");
    }

    #[test]
    fn writer_wraps_values() {
        let mut buf = Vec::new();
        let mut w = AstWriter::new(&mut buf).unwrap();
        w.write_value(br#"{"kind":"NamespaceDecl"}"#).unwrap();
        w.write_value(br#"{"kind":"NamespaceDecl"}"#).unwrap();
        assert_eq!(w.finish().unwrap(), 2);
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["inner"].as_array().unwrap().len(), 2);
    }

    /// A trimmed-down version of what clang prints for
    /// `static TESDataHandler* GetSingleton() { REL::Relocation<TESDataHandler**> singleton{ REL::ID(711558) }; ... }`
    /// plus a file switch and a static_assert in a class template.
    const SAMPLE: &str = r#"{
 "kind": "NamespaceDecl", "loc": {"offset": 1, "file": "/clf4/CommonLibF4/include/RE/A.h", "line": 3}, "name": "RE",
 "inner": [
  {"kind": "CXXRecordDecl", "loc": {"offset": 10, "line": 5}, "name": "TESDataHandler", "tagUsed": "class",
   "completeDefinition": true,
   "inner": [
    {"kind": "CXXRecordDecl", "loc": {"offset": 11}, "name": "TESDataHandler", "isImplicit": true},
    {"kind": "CXXMethodDecl", "loc": {"offset": 20, "line": 7}, "name": "GetSingleton",
     "type": {"qualType": "TESDataHandler *()"}, "storageClass": "static",
     "inner": [{"kind": "CompoundStmt", "range": {"begin": {"offset": 30}, "end": {"offset": 90, "line": 10}},
      "inner": [{"kind": "DeclStmt", "range": {"begin": {"offset": 31, "line": 8}, "end": {"offset": 80}},
       "inner": [{"kind": "VarDecl", "loc": {"offset": 40}, "name": "singleton",
        "type": {"desugaredQualType": "REL::Relocation<RE::TESDataHandler **>", "qualType": "REL::Relocation<TESDataHandler **>"},
        "inner": [{"kind": "CXXConstructExpr", "range": {"begin": {"offset": 41}, "end": {"offset": 79}},
         "type": {"desugaredQualType": "REL::Relocation<RE::TESDataHandler **>", "qualType": "REL::Relocation<TESDataHandler **>"},
         "inner": [{"kind": "CXXFunctionalCastExpr", "range": {"begin": {"offset": 50}, "end": {"offset": 70}}, "type": {"qualType": "REL::ID"},
          "inner": [{"kind": "CXXConstructExpr", "range": {"begin": {"offset": 50}, "end": {"offset": 70}}, "type": {"qualType": "REL::ID"},
           "inner": [{"kind": "ImplicitCastExpr", "range": {"begin": {"offset": 60}, "end": {"offset": 60}}, "type": {"qualType": "std::uint64_t"},
            "inner": [{"kind": "IntegerLiteral", "range": {"begin": {"offset": 60}, "end": {"offset": 60}}, "type": {"qualType": "int"}, "value": "711558"}]
           }]
          }]
         }]
        }]
       }]
      }]
     }]
    }
   ]
  },
  {"kind": "StaticAssertDecl", "loc": {"offset": 100, "file": "/clf4/CommonLibF4/include/RE/B.h", "line": 12},
   "inner": [{"kind": "BinaryOperator", "inner": [{"kind": "UnaryExprOrTypeTraitExpr", "name": "sizeof"}]}]},
  {"kind": "ClassTemplateDecl", "loc": {"offset": 200, "line": 20}, "name": "BSTArray",
   "inner": [{"kind": "CXXRecordDecl", "loc": {"offset": 201}, "name": "BSTArray", "completeDefinition": true,
     "inner": [{"kind": "StaticAssertDecl", "loc": {"offset": 210, "line": 22}, "inner": [{"kind": "CXXBoolLiteralExpr", "value": true}]}]}]
  },
  {"kind": "NamespaceDecl", "loc": {"offset": 250}, "name": "GFx",
   "inner": [{"kind": "UsingShadowDecl", "loc": {"offset": 251},
     "inner": [{"kind": "CXXRecordDecl", "loc": {"offset": 252}, "name": "Color", "completeDefinition": true,
       "inner": [{"kind": "StaticAssertDecl", "loc": {"offset": 253, "line": 25}}]}]}]},
  {"kind": "ClassTemplateSpecializationDecl", "loc": {"offset": 260}, "name": "BSTDataBuffer", "completeDefinition": true,
   "inner": [{"kind": "CXXRecordDecl", "loc": {"offset": 261}, "name": "Block", "completeDefinition": true}]},
  {"kind": "VarDecl", "loc": {"spellingLoc": {"offset": 5, "file": "/clf4/CommonLibF4/include/RE/M.h", "line": 1},
                              "expansionLoc": {"offset": 300, "file": "/clf4/CommonLibF4/include/RE/B.h", "line": 30}},
   "name": "RTTI_TESForm", "type": {"qualType": "const REL::ID"},
   "inner": [{"kind": "CXXConstructExpr", "range": {"begin": {"offset": 301}, "end": {"offset": 305}}, "type": {"qualType": "const REL::ID"},
     "inner": [{"kind": "IntegerLiteral", "range": {"begin": {"offset": 302}, "end": {"offset": 303}}, "value": "12345"}]}]}
 ]
}"#;

    #[test]
    fn walker_finds_ids_records_and_asserts() {
        let mut values = Vec::new();
        split_stream(SAMPLE.as_bytes(), |v| {
            values.push(v);
            Ok(())
        })
        .unwrap();
        assert_eq!(values.len(), 1);
        let node = parse_node(&values[0]).unwrap();
        let mut analysis = Analysis::default();
        Walker::new(&mut analysis, "/clf4/").top_level(&node);

        assert_eq!(analysis.re_records.iter().collect::<Vec<_>>(), ["RE::TESDataHandler"]);
        assert_eq!(analysis.relocs.len(), 2);
        let r = &analysis.relocs[0];
        assert_eq!(r.id, 711558);
        assert_eq!(r.kind, RelocKind::Relocation);
        assert_eq!(r.target_type.as_deref(), Some("RE::TESDataHandler **"));
        assert_eq!(r.function.as_deref(), Some("RE::TESDataHandler::GetSingleton"));
        assert_eq!(r.function_type.as_deref(), Some("TESDataHandler *()"));
        assert_eq!(r.var.as_deref(), Some("RE::TESDataHandler::GetSingleton::singleton"));
        assert_eq!((r.file.as_str(), r.line), ("CommonLibF4/include/RE/A.h", 8));
        let t = &analysis.relocs[1];
        assert_eq!((t.id, &t.kind, t.var.as_deref()), (12345, &RelocKind::Id, Some("RE::RTTI_TESForm")));
        assert_eq!((t.file.as_str(), t.line), ("CommonLibF4/include/RE/B.h", 30));

        let sites: Vec<(&str, bool, bool)> =
            analysis.static_asserts.iter().map(|(k, s)| (k.as_str(), s.evaluated, s.sizeof)).collect();
        assert_eq!(
            sites,
            [("CommonLibF4/include/RE/B.h:12", true, true), ("CommonLibF4/include/RE/B.h:22", false, false)]
        );
    }
}
