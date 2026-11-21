//! Parser for clang's `-fdump-record-layouts` text output (Microsoft ABI flavour).
//!
//! A record looks like this (offsets are absolute bytes from the start of the
//! outermost record; nesting is two spaces per level after the `| `):
//!
//! ```text
//! *** Dumping AST Record Layout
//!          0 | class RE::TESForm
//!          0 |   class RE::BaseFormComponent (primary base)
//!          0 |     (BaseFormComponent vftable pointer)
//!          8 |   struct RE::TESFileContainer sourceFiles
//!          8 |     const class RE::TESFileArray * array
//!         16 |   unsigned int formFlags
//!      9:0-3 |   unsigned char bits
//!            | [sizeof=32, align=8,
//!            |  nvsize=32, nvalign=8]
//! ```
//!
//! Only direct members (depth 1) become `fields`/`bases`. Deeper lines describe
//! sub-objects that have their own entry, except vftable/vbtable pointers, which
//! are collected when they sit in this record's own base-class chain.

use serde::{Deserialize, Serialize};

const HEADER: &str = "*** Dumping AST Record Layout";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordLayout {
    /// Fully qualified name as clang prints it, e.g. `RE::BSTArray<class RE::TESForm *>`.
    pub name: String,
    /// `class`, `struct` or `union`.
    pub kind: String,
    pub size: u64,
    pub align: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvsize: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvalign: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub empty: bool,
    pub fields: Vec<Field>,
    pub bases: Vec<Base>,
    /// Offset of the primary vftable pointer (the lowest one in this record's own or
    /// inherited base chain). `None` for non-polymorphic records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfptr: Option<u64>,
    /// Every vftable pointer in the base chain, with the class that introduced it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vfptrs: Vec<PtrSlot>,
    /// Offset of the first vbtable pointer (records with virtual bases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vbptr: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vtordisps: Vec<PtrSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// Empty for anonymous unions/structs and unnamed bit-fields.
    pub name: String,
    pub offset: u64,
    /// The type as clang prints it. Record-typed members are prefixed with their tag
    /// (`struct RE::TESFileContainer`).
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_width: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base {
    pub name: String,
    pub offset: u64,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtrSlot {
    pub offset: u64,
    /// The class named in the dump line, e.g. `BaseFormComponent` (unqualified).
    pub class: String,
}

impl RecordLayout {
    pub fn is_polymorphic(&self) -> bool {
        !self.vfptrs.is_empty()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub records: Vec<RecordLayout>,
    /// Records that could not be parsed, as `line N: reason`. The parser skips them
    /// instead of failing the whole file.
    pub warnings: Vec<String>,
}

/// Parses every `*** Dumping AST Record Layout` section in `text`. Other sections
/// (e.g. `*** Dumping IRgen Record Layout`) are ignored.
pub fn parse(text: &str) -> Parsed {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Parsed::default();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with(HEADER) {
            let start = i + 1;
            match parse_record(&lines, start) {
                Ok((record, next)) => {
                    out.records.push(record);
                    i = next;
                }
                Err((at, reason)) => {
                    out.warnings.push(format!("line {}: {reason}", at + 1));
                    i = at.max(start);
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    Base,
    Field,
    Pointer,
}

type Failure = (usize, String);

/// Parses one record starting at `lines[start]` (the line after the section header).
/// Returns the record and the index of the first line after it.
fn parse_record(lines: &[&str], start: usize) -> Result<(RecordLayout, usize), Failure> {
    let mut i = start;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let header_at = i;
    let header = lines.get(i).ok_or((i, "missing record header".to_string()))?;
    let (offset, content) = split_line(header).ok_or((i, format!("bad header `{header}`")))?;
    if offset.trim() != "0" || content.starts_with(' ') {
        return Err((i, format!("bad header `{header}`")));
    }
    let (content, empty) = strip_suffix_tag(content, "(empty)");
    let (kind, name) = content.split_once(' ').ok_or((i, format!("bad header `{header}`")))?;
    let mut record = RecordLayout {
        name: name.to_string(),
        kind: kind.to_string(),
        size: 0,
        align: 0,
        nvsize: None,
        nvalign: None,
        empty,
        fields: Vec::new(),
        bases: Vec::new(),
        vfptr: None,
        vfptrs: Vec::new(),
        vbptr: None,
        vtordisps: Vec::new(),
    };

    // stack[d - 1] is the kind of the innermost entry seen at depth d.
    let mut stack: Vec<Entry> = Vec::new();
    let mut footer = String::new();
    i += 1;
    loop {
        let Some(line) = lines.get(i) else {
            return Err((header_at, format!("record `{}` has no [sizeof=...] footer", record.name)));
        };
        if line.starts_with("***") || line.trim().is_empty() {
            return Err((i, format!("record `{}` ended before its footer", record.name)));
        }
        let (offset, content) = split_line(line).ok_or((i, format!("unrecognized line `{line}`")))?;
        if offset.trim().is_empty() {
            footer.push_str(content);
            footer.push(' ');
            if content.contains(']') {
                i += 1;
                break;
            }
            i += 1;
            continue;
        }
        let (byte, bits) = parse_offset(offset.trim()).ok_or((i, format!("bad offset in `{line}`")))?;
        let indent = content.len() - content.trim_start_matches(' ').len();
        let depth = indent / 2;
        if depth == 0 {
            return Err((i, format!("second depth-0 line `{line}`")));
        }
        let body = &content[indent..];
        stack.truncate(depth - 1);
        let in_base_chain = stack.iter().all(|e| *e == Entry::Base);
        let entry = classify(body);
        match entry {
            Line::VfPtr(class) => {
                if in_base_chain {
                    record.vfptrs.push(PtrSlot { offset: byte, class: class.to_string() });
                }
                stack.push(Entry::Pointer);
            }
            Line::VbPtr => {
                if in_base_chain && record.vbptr.is_none() {
                    record.vbptr = Some(byte);
                }
                stack.push(Entry::Pointer);
            }
            Line::VtorDisp(class) => {
                if depth == 1 {
                    record.vtordisps.push(PtrSlot { offset: byte, class: class.to_string() });
                }
                stack.push(Entry::Pointer);
            }
            Line::Base { name, is_virtual, primary, empty } => {
                if depth == 1 {
                    record.bases.push(Base {
                        name: name.to_string(),
                        offset: byte,
                        is_virtual,
                        primary,
                        empty,
                    });
                }
                stack.push(Entry::Base);
            }
            Line::Field { ty, name, empty } => {
                if depth == 1 {
                    let (bit_offset, bit_width) = match bits {
                        Some((b, w)) => (Some(b), Some(w)),
                        None => (None, None),
                    };
                    record.fields.push(Field {
                        name: name.to_string(),
                        offset: byte,
                        ty: ty.to_string(),
                        bit_offset,
                        bit_width,
                        empty,
                    });
                }
                stack.push(Entry::Field);
            }
        }
        i += 1;
    }

    let footer = footer.trim();
    let inner = footer
        .strip_prefix('[')
        .and_then(|f| f.trim_end().strip_suffix(']'))
        .ok_or((i - 1, format!("bad footer `{footer}`")))?;
    let mut have_size = false;
    let mut have_align = false;
    for pair in inner.split(',') {
        let Some((key, value)) = pair.trim().split_once('=') else { continue };
        let Ok(value) = value.trim().parse::<u64>() else { continue };
        match key.trim() {
            "sizeof" => {
                record.size = value;
                have_size = true;
            }
            "align" => {
                record.align = value;
                have_align = true;
            }
            "nvsize" => record.nvsize = Some(value),
            "nvalign" => record.nvalign = Some(value),
            _ => {}
        }
    }
    if !(have_size && have_align) {
        return Err((i - 1, format!("footer of `{}` lacks sizeof/align", record.name)));
    }
    record.vfptr = record.vfptrs.iter().map(|p| p.offset).min();
    Ok((record, i))
}

/// Splits `"        16 |   unsigned int formFlags"` into the offset column and the
/// content after `| ` (indentation and trailing spaces preserved: an unnamed member
/// shows up as a trailing space after its type).
fn split_line(line: &str) -> Option<(&str, &str)> {
    let at = line.find(" | ")?;
    let content = &line[at + 3..];
    // Footer lines read `           | [sizeof=...`; clang prints the bar in column 11.
    Some((&line[..at], content))
}

/// `"16"` → (16, None); `"9:2-7"` → (9, Some((2, 6))) meaning bits 2..=7 (width 6).
fn parse_offset(s: &str) -> Option<(u64, Option<(u32, u32)>)> {
    match s.split_once(':') {
        None => Some((s.parse().ok()?, None)),
        Some((byte, bits)) => {
            let byte = byte.parse().ok()?;
            let (first, last) = bits.split_once('-')?;
            let first: u32 = first.parse().ok()?;
            // A zero-width bit-field prints an end below its start (or nothing).
            let width = match last.parse::<u32>() {
                Ok(last) if last + 1 >= first => last + 1 - first,
                _ => 0,
            };
            Some((byte, Some((first, width))))
        }
    }
}

enum Line<'a> {
    VfPtr(&'a str),
    VbPtr,
    VtorDisp(&'a str),
    Base { name: &'a str, is_virtual: bool, primary: bool, empty: bool },
    Field { ty: &'a str, name: &'a str, empty: bool },
}

fn classify(body: &str) -> Line<'_> {
    if let Some(inner) = body.strip_prefix('(').and_then(|b| b.strip_suffix(')')) {
        if let Some(class) = inner.strip_suffix(" vftable pointer") {
            return Line::VfPtr(class);
        }
        if inner.ends_with(" vbtable pointer") {
            return Line::VbPtr;
        }
        if let Some(class) = inner.strip_prefix("vtordisp for vbase ") {
            return Line::VtorDisp(class);
        }
    }
    let (rest, empty) = strip_suffix_tag(body, "(empty)");
    for (tag, is_virtual, primary) in
        [("(primary base)", false, true), ("(virtual base)", true, false), ("(base)", false, false)]
    {
        let (base, matched) = strip_suffix_tag(rest, tag);
        if matched {
            let name = base.split_once(' ').map_or(base, |(_kind, name)| name);
            return Line::Base { name, is_virtual, primary, empty };
        }
    }
    // The member name is the last space-separated token; types may contain spaces
    // (and even paths with spaces, for anonymous records) but names never do.
    match rest.rfind(' ') {
        Some(at) => Line::Field { ty: &rest[..at], name: &rest[at + 1..], empty },
        None => Line::Field { ty: rest, name: "", empty },
    }
}

/// Strips a trailing `" " + tag`, reporting whether it was present.
fn strip_suffix_tag<'a>(s: &'a str, tag: &str) -> (&'a str, bool) {
    match s.strip_suffix(tag).and_then(|r| r.strip_suffix(' ')) {
        Some(rest) => (rest, true),
        None => (s, false),
    }
}

/// Counts for the `layouts` summary.
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub records: usize,
    pub unique_names: usize,
    pub re_records: usize,
    pub re_non_template: usize,
    pub re_polymorphic: usize,
}

pub fn summarize(records: &[RecordLayout]) -> Summary {
    let mut names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let re: Vec<&RecordLayout> = records.iter().filter(|r| r.name.starts_with("RE::")).collect();
    Summary {
        records: records.len(),
        unique_names: names.len(),
        re_records: re.len(),
        re_non_template: re.iter().filter(|r| !r.name.contains('<')).count(),
        re_polymorphic: re.iter().filter(|r| r.is_polymorphic()).count(),
    }
}

/// Writes `records` as a JSON array with one record per line.
pub fn write_json(records: &[RecordLayout], out: &mut impl std::io::Write) -> std::io::Result<()> {
    out.write_all(b"[\n")?;
    for (n, record) in records.iter().enumerate() {
        serde_json::to_writer(&mut *out, record)?;
        out.write_all(if n + 1 == records.len() { b"\n" } else { b",\n" })?;
    }
    out.write_all(b"]\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
\n*** Dumping AST Record Layout
         0 | class RE::TESForm
         0 |   class RE::BaseFormComponent (primary base)
         0 |     (BaseFormComponent vftable pointer)
         8 |   struct RE::TESFileContainer sourceFiles
         8 |     const class RE::TESFileArray * array
        16 |   unsigned int formFlags
        20 |   unsigned int formID
        24 |   unsigned short inGameFormFlags
        26 |   class F4SE::stl::enumeration<enum RE::ENUM_FORM_ID, unsigned char> formType
        26 |     unsigned char _impl
           | [sizeof=32, align=8,
           |  nvsize=32, nvalign=8]

*** Dumping AST Record Layout
         0 | struct std::nothrow_t (empty)
           | [sizeof=1, align=1,
           |  nvsize=0, nvalign=1]

*** Dumping AST Record Layout
         0 | struct Bits
     0:0-59 |   size_t _Index
     7:4-7 |   unsigned char flags
         8 |   union Bits::(anonymous at /path with spaces/x.h:3:5)\x20
         8 |     int a
        16 |   struct RE::Empty tag (empty)
           | [sizeof=24, align=8,
           |  nvsize=24, nvalign=8]

*** Dumping AST Record Layout
         0 | class Derived
         0 |   (Derived vftable pointer)
         8 |   (Derived vbtable pointer)
        16 |   struct Member m
        16 |     (Member vftable pointer)
        24 |   class Mixin (base) (empty)
        28 |   (vtordisp for vbase VBase)
        32 |   class VBase (virtual base)
        32 |     (VBase vftable pointer)
           | [sizeof=40, align=8,
           |  nvsize=24, nvalign=8]
";

    #[test]
    fn parses_sample() {
        let parsed = parse(SAMPLE);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.records.len(), 4);

        let form = &parsed.records[0];
        assert_eq!(form.name, "RE::TESForm");
        assert_eq!(form.kind, "class");
        assert_eq!((form.size, form.align, form.nvsize), (32, 8, Some(32)));
        assert_eq!(form.vfptr, Some(0));
        assert_eq!(form.bases.len(), 1);
        assert_eq!(form.bases[0].name, "RE::BaseFormComponent");
        assert!(form.bases[0].primary && !form.bases[0].is_virtual);
        let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["sourceFiles", "formFlags", "formID", "inGameFormFlags", "formType"]);
        assert_eq!(form.fields[2].offset, 20);
        assert_eq!(form.fields[4].ty, "class F4SE::stl::enumeration<enum RE::ENUM_FORM_ID, unsigned char>");

        let empty = &parsed.records[1];
        assert!(empty.empty);
        assert_eq!((empty.name.as_str(), empty.size, empty.vfptr), ("std::nothrow_t", 1, None));

        let bits = &parsed.records[2];
        assert_eq!(bits.fields[0].bit_offset, Some(0));
        assert_eq!(bits.fields[0].bit_width, Some(60));
        assert_eq!(
            (bits.fields[1].offset, bits.fields[1].bit_offset, bits.fields[1].bit_width),
            (7, Some(4), Some(4))
        );
        assert_eq!(bits.fields[2].name, "");
        assert_eq!(bits.fields[2].ty, "union Bits::(anonymous at /path with spaces/x.h:3:5)");
        assert_eq!((bits.fields[3].name.as_str(), bits.fields[3].empty), ("tag", true));
        assert_eq!(bits.fields.len(), 4);

        let derived = &parsed.records[3];
        // The vfptr inside member `m` is not ours; the one inside the virtual base is.
        let slots: Vec<(u64, &str)> = derived.vfptrs.iter().map(|p| (p.offset, p.class.as_str())).collect();
        assert_eq!(slots, [(0, "Derived"), (32, "VBase")]);
        assert_eq!(derived.vbptr, Some(8));
        assert_eq!(derived.vtordisps, [PtrSlot { offset: 28, class: "VBase".into() }]);
        assert_eq!(derived.bases.len(), 2);
        assert!(derived.bases[0].empty && !derived.bases[0].primary);
        assert!(derived.bases[1].is_virtual);
    }

    #[test]
    fn bad_record_is_skipped_not_fatal() {
        let text = "*** Dumping AST Record Layout\n         0 | struct A\n         0 |   int x\n\n\
                    *** Dumping AST Record Layout\n         0 | struct B\n           | [sizeof=1, align=1]\n";
        let parsed = parse(text);
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0].name, "B");
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn json_round_trip() {
        let parsed = parse(SAMPLE);
        let mut buf = Vec::new();
        write_json(&parsed.records, &mut buf).unwrap();
        let back: Vec<RecordLayout> = serde_json::from_slice(&buf).unwrap();
        assert_eq!(back, parsed.records);
    }
}
