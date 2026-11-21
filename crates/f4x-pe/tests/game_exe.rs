//! Checks against a real `Fallout4.exe`. Skipped unless `F4X_GAME_EXE` points at one.

use f4x_pe::rtti::{RttiIndex, class_info, mangle, vtable_len};
use f4x_pe::{Image, PeFile};

fn exe() -> Option<Vec<u8>> {
    let path = std::env::var_os("F4X_GAME_EXE")?;
    Some(std::fs::read(path).expect("F4X_GAME_EXE unreadable"))
}

#[test]
fn rtti_index_on_the_game() {
    let Some(bytes) = exe() else { return };
    let pe = PeFile::parse(&bytes).unwrap();
    let started = std::time::Instant::now();
    let index = RttiIndex::scan(&pe);
    eprintln!(
        "{} type descriptors, {} vtables in {:?}",
        index.type_descriptors.len(),
        index.vtables.len(),
        started.elapsed()
    );
    // CommonLibF4's RTTI_IDs.h lists 11,888 type descriptors for 1.10.163.
    assert!(index.type_descriptors.len() > 10_000);

    for class in ["TESForm", "TESObjectREFR", "Actor", "PlayerCharacter", "BSScript::Internal::VirtualMachine", "GameVM"] {
        let mangled = mangle(class, false);
        let vtable = index.primary_vtable(&mangled).unwrap_or_else(|| panic!("no primary vtable for {class}"));
        let info = class_info(&pe, vtable).unwrap();
        assert_eq!(info.name(), class);
        eprintln!("{class}: vtable rva {vtable:#x}, {} slots, {} bases", vtable_len(&pe, vtable), info.bases.len());
    }

    let starts = pe.function_starts();
    assert!(starts.len() > 50_000);
    assert!(pe.imports().iter().any(|i| i.dll.eq_ignore_ascii_case("USER32.dll") && i.name.as_deref() == Some("PeekMessageA")));
    assert!(!pe.relocations().is_empty());
    assert!(pe.section(".text").is_some());
}
