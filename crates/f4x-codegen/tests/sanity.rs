//! Sanity checks against a real dump. Each test is skipped (passes with a note) when
//! `target/clangdump/` has not been produced yet:
//!
//! ```sh
//! cargo run --release -p f4x-codegen -- dump
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use f4x_codegen::ast::{Reloc, RelocKind};
use f4x_codegen::default_out_dir;
use f4x_codegen::records::{self, RecordLayout};

/// Sizes CommonLibF4 asserts with `static_assert(sizeof(...))`.
const KNOWN_SIZES: &[(&str, u64)] = &[
    ("RE::TESForm", 0x20),
    ("RE::TESObjectREFR", 0x110),
    ("RE::Actor", 0x490),
    ("RE::PlayerCharacter", 0xE10),
    ("RE::TESDataHandler", 0x1020),
    ("RE::BSScript::Stack", 0xA8),
    ("RE::Scaleform::GFx::Value", 0x20),
    ("RE::UI", 0x250),
];

fn dump_file(name: &str) -> Option<PathBuf> {
    let path = default_out_dir().join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!(
            "skipping: {} not found (run `cargo run --release -p f4x-codegen -- dump`)",
            path.display()
        );
        None
    }
}

fn by_name(records: &[RecordLayout]) -> HashMap<&str, &RecordLayout> {
    let mut map = HashMap::new();
    for r in records {
        map.entry(r.name.as_str()).or_insert(r);
    }
    map
}

#[test]
fn records_txt_has_known_sizes() {
    let Some(path) = dump_file("records.txt") else { return };
    let parsed = records::parse(&fs::read_to_string(path).unwrap());
    assert!(
        parsed.warnings.is_empty(),
        "parse warnings: {:?}",
        &parsed.warnings[..parsed.warnings.len().min(5)]
    );
    assert!(parsed.records.len() > 10_000, "only {} records", parsed.records.len());
    let map = by_name(&parsed.records);
    for &(name, size) in KNOWN_SIZES {
        let record = map.get(name).unwrap_or_else(|| panic!("{name} has no layout"));
        assert_eq!(record.size, size, "sizeof({name})");
    }

    // A few structural facts that the binding generator will rely on.
    let form = map["RE::TESForm"];
    assert_eq!(form.vfptr, Some(0));
    assert_eq!(form.bases[0].name, "RE::BaseFormComponent");
    let form_id = form.fields.iter().find(|f| f.name == "formID").expect("TESForm::formID");
    assert_eq!(form_id.offset, 0x14);
    let player = map["RE::PlayerCharacter"];
    assert_eq!((player.bases[0].name.as_str(), player.bases[0].offset), ("RE::Actor", 0));
    assert!(player.bases[0].primary);
    assert_eq!(map["RE::BSScript::Stack"].vfptr, None);
}

#[test]
fn layouts_json_matches_records_txt() {
    let (Some(json), Some(txt)) = (dump_file("layouts.json"), dump_file("records.txt")) else { return };
    let from_json: Vec<RecordLayout> = serde_json::from_slice(&fs::read(json).unwrap()).unwrap();
    let from_txt = records::parse(&fs::read_to_string(txt).unwrap()).records;
    assert_eq!(from_json.len(), from_txt.len());
    let map = by_name(&from_json);
    for &(name, size) in KNOWN_SIZES {
        assert_eq!(map[name].size, size, "sizeof({name}) in layouts.json");
    }
}

#[test]
fn vtables_txt_has_player_character() {
    let Some(path) = dump_file("vtables.txt") else { return };
    let text = fs::read_to_string(path).unwrap();
    // TESForm's vftable as seen from PlayerCharacter: the BaseFormComponent vfptr at 0.
    assert!(
        text.lines().any(|l| l.starts_with("VFTable for 'RE::BaseFormComponent' in 'RE::TESForm' in")
            && l.contains("in 'RE::PlayerCharacter' (")),
        "no PlayerCharacter vftable in vtables.txt"
    );
    assert!(text.contains("VFTable indices for 'RE::TESForm'"));
}

#[test]
fn relocs_json_has_known_ids() {
    let Some(path) = dump_file("relocs.json") else { return };
    let relocs: Vec<Reloc> = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let find = |id: u64| {
        relocs
            .iter()
            .find(|r| r.id == id && r.kind == RelocKind::Relocation)
            .unwrap_or_else(|| panic!("REL::ID({id}) not extracted"))
    };
    let singleton = find(711558);
    assert_eq!(singleton.function.as_deref(), Some("RE::TESDataHandler::GetSingleton"));
    assert_eq!(singleton.target_type.as_deref(), Some("RE::TESDataHandler **"));
    let check = find(1432894);
    assert_eq!(check.function.as_deref(), Some("RE::TESDataHandler::CheckModsLoaded"));
    assert_eq!(check.target_type.as_deref(), Some("bool (RE::TESDataHandler::*)(bool)"));
    assert_eq!(find(901511).target_type.as_deref(), Some("RE::SCRIPT_FUNCTION (*)[522]"));
    assert_eq!(find(75173).target_type.as_deref(), Some("RE::SCRIPT_FUNCTION (*)[819]"));
}
