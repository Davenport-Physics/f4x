//! MSVC x64 run-time type information.
//!
//! Layout (every `rva` field is relative to the image base):
//!
//! ```text
//! vtable[-1] ──> CompleteObjectLocator { signature = 1, offset, cd_offset,
//!                                         type_descriptor_rva, hierarchy_rva, self_rva }
//! TypeDescriptor      { vftable: u64, spare: u64, name: ".?AVTESForm@@\0" }
//! ClassHierarchy      { signature, attributes, base_count, base_array_rva }
//! BaseClassDescriptor { type_descriptor_rva, contained_bases, mdisp, pdisp, vdisp, attributes, .. }
//! ```
//!
//! [`RttiIndex::scan`] walks the image once and maps every class name to its vtables,
//! so code can find e.g. the Papyrus VM's vtable on any build without addresses.

use std::collections::{HashMap, HashSet};

use crate::image::Image;

/// A base class in a class hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseClass {
    /// Mangled name, e.g. `.?AVTESFullName@@`.
    pub mangled_name: String,
    /// Offset of the base inside the complete object.
    pub offset: i32,
    /// True for a virtual base, whose offset is only known at run time.
    pub is_virtual: bool,
}

/// What RTTI says about the class behind one vtable.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    /// Mangled name of the complete (most-derived) class.
    pub mangled_name: String,
    /// Offset of the subobject using this vtable inside the complete object.
    pub vtable_offset: u32,
    /// Every class in the hierarchy, starting with the class itself.
    pub bases: Vec<BaseClass>,
}

impl ClassInfo {
    /// The class name in C++ form, e.g. `TESObjectWEAP`.
    #[must_use]
    pub fn name(&self) -> String {
        demangle(&self.mangled_name)
    }

    /// Offset of base `mangled_name` relative to the subobject that owns this vtable,
    /// or `None` if it isn't a non-virtual base.
    #[must_use]
    pub fn base_offset(&self, mangled_name: &str) -> Option<isize> {
        let base = self.bases.iter().find(|b| b.mangled_name == mangled_name && !b.is_virtual)?;
        Some(base.offset as isize - self.vtable_offset as isize)
    }
}

const COL_SIGNATURE: u32 = 1;
const MAX_NAME: usize = 1024;

/// Decode the complete object locator at `col_rva`.
pub fn locator(image: &impl Image, col_rva: u32) -> Option<ClassInfo> {
    if image.u32_at(col_rva)? != COL_SIGNATURE || image.u32_at(col_rva + 0x14)? != col_rva {
        return None;
    }
    let vtable_offset = image.u32_at(col_rva + 4)?;
    let type_descriptor = image.u32_at(col_rva + 0xC)?;
    let hierarchy = image.u32_at(col_rva + 0x10)?;
    let mangled_name = image.c_str_at(type_descriptor + 0x10, MAX_NAME)?.to_owned();

    let count = image.u32_at(hierarchy + 8)?;
    if count > 1024 {
        return None;
    }
    let array = image.u32_at(hierarchy + 0xC)?;
    let mut bases = Vec::with_capacity(count as usize);
    for i in 0..count {
        let descriptor = image.u32_at(array + 4 * i)?;
        let name = image.c_str_at(image.u32_at(descriptor)? + 0x10, MAX_NAME)?.to_owned();
        let mdisp = image.i32_at(descriptor + 8)?;
        let pdisp = image.i32_at(descriptor + 0xC)?;
        bases.push(BaseClass { mangled_name: name, offset: mdisp, is_virtual: pdisp != -1 });
    }
    Some(ClassInfo { mangled_name, vtable_offset, bases })
}

/// The RTTI behind the vtable at `vtable_rva`.
pub fn class_info(image: &impl Image, vtable_rva: u32) -> Option<ClassInfo> {
    let col = image.rva_of(image.u64_at(vtable_rva.checked_sub(8)?)?)?;
    locator(image, col)
}

/// `.?AVTESObjectWEAP@@` → `TESObjectWEAP`, `.?AVVirtualMachine@Internal@BSScript@@` →
/// `BSScript::Internal::VirtualMachine`. Templates come back unchanged.
#[must_use]
pub fn demangle(mangled: &str) -> String {
    let inner = mangled.strip_prefix(".?AV").or_else(|| mangled.strip_prefix(".?AU")).and_then(|s| s.strip_suffix("@@"));
    match inner {
        Some(inner) if !inner.contains('?') => inner.rsplit('@').collect::<Vec<_>>().join("::"),
        _ => mangled.to_owned(),
    }
}

/// `BSScript::Internal::VirtualMachine` → `.?AVVirtualMachine@Internal@BSScript@@`
/// (`struct_` selects the `.?AU` prefix). Non-template names only.
#[must_use]
pub fn mangle(name: &str, struct_: bool) -> String {
    let mut parts: Vec<&str> = name.split("::").collect();
    parts.reverse();
    format!("{}{}@@", if struct_ { ".?AU" } else { ".?AV" }, parts.join("@"))
}

/// One vtable found by [`RttiIndex::scan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vtable {
    /// Mangled name of the complete class.
    pub class: String,
    pub vtable_rva: u32,
    pub locator_rva: u32,
    /// Offset of the subobject this vtable belongs to (0 = primary vtable).
    pub offset: u32,
}

/// Every type descriptor and vtable in an image, by class name.
#[derive(Default, Debug)]
pub struct RttiIndex {
    /// Mangled name → type descriptor RVA.
    pub type_descriptors: HashMap<String, u32>,
    pub vtables: Vec<Vtable>,
    by_class: HashMap<String, Vec<usize>>,
}

impl RttiIndex {
    /// Scan every non-executable section of `image`.
    pub fn scan(image: &impl Image) -> Self {
        let data_sections: Vec<_> = image.sections().iter().filter(|s| !s.is_executable()).cloned().collect();

        // 1. Type descriptors: ".?AV"/".?AU" names 16 bytes after an 8-aligned start whose
        //    first field points into the image (type_info's vtable). Keep the most common
        //    first-field value, which is that vtable, to reject lookalike strings.
        let mut candidates: Vec<(u32, u64)> = Vec::new();
        for section in &data_sections {
            let bytes = image.section_bytes(section);
            let mut at = 0;
            while let Some(found) = find(&bytes[at..], b".?A") {
                let offset = at + found;
                at = offset + 3;
                if !matches!(bytes.get(offset + 3), Some(b'V' | b'U')) || offset < 0x10 {
                    continue;
                }
                let td = section.rva + (offset - 0x10) as u32;
                if td % 8 != 0 {
                    continue;
                }
                if let Some(vftable) = image.u64_at(td)
                    && image.rva_of(vftable).is_some()
                {
                    candidates.push((td, vftable));
                }
            }
        }
        let mut votes: HashMap<u64, usize> = HashMap::new();
        for &(_, vftable) in &candidates {
            *votes.entry(vftable).or_default() += 1;
        }
        let type_info_vtable = votes.into_iter().max_by_key(|&(_, n)| n).map(|(v, _)| v);
        let mut index = RttiIndex::default();
        let mut td_set = HashSet::new();
        for (td, vftable) in candidates {
            if Some(vftable) == type_info_vtable
                && let Some(name) = image.c_str_at(td + 0x10, MAX_NAME)
            {
                index.type_descriptors.insert(name.to_owned(), td);
                td_set.insert(td);
            }
        }

        // 2. Complete object locators: signature 1, self RVA, a known type descriptor.
        let mut locators: HashMap<u32, (u32, u32)> = HashMap::new(); // col -> (td, offset)
        for section in &data_sections {
            let bytes = image.section_bytes(section);
            for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
                if u32::from_le_bytes(*chunk) != COL_SIGNATURE {
                    continue;
                }
                let col = section.rva + 4 * i as u32;
                if image.u32_at(col + 0x14) == Some(col)
                    && let Some(td) = image.u32_at(col + 0xC)
                    && td_set.contains(&td)
                {
                    locators.insert(col, (td, image.u32_at(col + 4).unwrap_or(0)));
                }
            }
        }

        // 3. Vtables: the slot just before each vtable holds a pointer to its locator.
        let td_names: HashMap<u32, &str> = index.type_descriptors.iter().map(|(n, &td)| (td, n.as_str())).collect();
        let base = image.base();
        for section in &data_sections {
            let bytes = image.section_bytes(section);
            for (i, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
                let pointer = u64::from_le_bytes(*chunk);
                let Some(col) = pointer.checked_sub(base).and_then(|r| u32::try_from(r).ok()) else { continue };
                if let Some(&(td, offset)) = locators.get(&col) {
                    let class = td_names[&td].to_owned();
                    let vtable_rva = section.rva + 8 * i as u32 + 8;
                    index.vtables.push(Vtable { class, vtable_rva, locator_rva: col, offset });
                }
            }
        }
        for (i, vtable) in index.vtables.iter().enumerate() {
            index.by_class.entry(vtable.class.clone()).or_default().push(i);
        }
        index
    }

    /// All vtables of the class with this mangled name (primary and secondary).
    pub fn vtables_of(&self, mangled: &str) -> impl Iterator<Item = &Vtable> {
        self.by_class.get(mangled).into_iter().flatten().map(|&i| &self.vtables[i])
    }

    /// The primary vtable (subobject offset 0) of a class, if it has exactly one.
    #[must_use]
    pub fn primary_vtable(&self, mangled: &str) -> Option<u32> {
        let mut primary = self.vtables_of(mangled).filter(|v| v.offset == 0);
        let first = primary.next()?;
        primary.next().is_none().then_some(first.vtable_rva)
    }
}

/// Number of consecutive slots from `vtable_rva` that point into executable code,
/// stopping at the next vtable's locator slot.
pub fn vtable_len(image: &impl Image, vtable_rva: u32) -> usize {
    let mut count = 0;
    loop {
        let Some(pointer) = image.u64_at(vtable_rva + 8 * count as u32) else { break };
        let Some(target) = image.rva_of(pointer) else { break };
        if !image.section_at(target).is_some_and(|s| s.is_executable()) {
            break;
        }
        count += 1;
    }
    count
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Section;

    /// A fake image: two sections (.text, .rdata) in one buffer.
    struct Fake {
        base: u64,
        bytes: Vec<u8>,
        sections: Vec<Section>,
    }

    impl Image for Fake {
        fn base(&self) -> u64 {
            self.base
        }
        fn sections(&self) -> &[Section] {
            &self.sections
        }
        fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]> {
            self.bytes.get(rva as usize..rva as usize + len)
        }
    }

    impl Fake {
        fn put(&mut self, rva: usize, data: &[u8]) {
            self.bytes[rva..rva + data.len()].copy_from_slice(data);
        }
        fn put_u32(&mut self, rva: usize, v: u32) {
            self.put(rva, &v.to_le_bytes());
        }
        fn put_u64(&mut self, rva: usize, v: u64) {
            self.put(rva, &v.to_le_bytes());
        }
    }

    const BASE: u64 = 0x1_4000_0000;

    /// `class Derived : Base(0x00), Component(0x28)` with a secondary vtable for the
    /// Component subobject. Code at 0x000..0x100, data at 0x100..0x1000.
    fn fake_image() -> (Fake, u32, u32) {
        let mut m = Fake {
            base: BASE,
            bytes: vec![0; 0x1000],
            sections: vec![
                Section { name: ".text".into(), rva: 0, virtual_size: 0x100, readable_size: 0x100, characteristics: Section::EXECUTE },
                Section { name: ".rdata".into(), rva: 0x100, virtual_size: 0xF00, readable_size: 0xF00, characteristics: 0 },
            ],
        };
        let type_info_vtable = BASE + 0x900;
        let (td_derived, td_base, td_component) = (0x100, 0x140, 0x180);
        for (td, name) in [(td_derived, ".?AVDerived@@"), (td_base, ".?AVBase@@"), (td_component, ".?AVComponent@@")] {
            m.put_u64(td, type_info_vtable);
            m.put(td + 0x10, name.as_bytes());
        }
        for (rva, td, mdisp) in [(0x200, td_derived, 0u32), (0x220, td_base, 0), (0x240, td_component, 0x28)] {
            m.put_u32(rva, td as u32);
            m.put_u32(rva + 8, mdisp);
            m.put_u32(rva + 0xC, u32::MAX);
        }
        for (i, bcd) in [0x200u32, 0x220, 0x240].into_iter().enumerate() {
            m.put_u32(0x280 + 4 * i, bcd);
        }
        m.put_u32(0x2C0 + 8, 3);
        m.put_u32(0x2C0 + 0xC, 0x280);
        for (col, offset) in [(0x300usize, 0u32), (0x330, 0x28)] {
            m.put_u32(col, 1);
            m.put_u32(col + 4, offset);
            m.put_u32(col + 0xC, td_derived as u32);
            m.put_u32(col + 0x10, 0x2C0);
            m.put_u32(col + 0x14, col as u32);
        }
        // Primary vtable: locator slot, then 3 code pointers; secondary: 1 code pointer.
        m.put_u64(0x400, BASE + 0x300);
        for slot in 0..3 {
            m.put_u64(0x408 + 8 * slot, BASE + 0x10 * (slot as u64 + 1));
        }
        m.put_u64(0x440, BASE + 0x330);
        m.put_u64(0x448, BASE + 0x50);
        (m, 0x408, 0x448)
    }

    #[test]
    fn reads_class_and_bases() {
        let (m, primary, _) = fake_image();
        let info = class_info(&m, primary).unwrap();
        assert_eq!(info.name(), "Derived");
        assert_eq!(info.vtable_offset, 0);
        let names: Vec<_> = info.bases.iter().map(|b| demangle(&b.mangled_name)).collect();
        assert_eq!(names, ["Derived", "Base", "Component"]);
        assert_eq!(info.base_offset(".?AVComponent@@"), Some(0x28));
        assert_eq!(info.base_offset(".?AVMissing@@"), None);
    }

    #[test]
    fn offsets_are_relative_to_the_subobject() {
        let (m, _, secondary) = fake_image();
        let info = class_info(&m, secondary).unwrap();
        assert_eq!(info.vtable_offset, 0x28);
        assert_eq!(info.base_offset(".?AVBase@@"), Some(-0x28));
    }

    #[test]
    fn rejects_garbage() {
        let (m, primary, _) = fake_image();
        assert!(class_info(&m, primary + 0x100).is_none());
        assert!(class_info(&m, 0x10).is_none());
    }

    #[test]
    fn scan_finds_vtables_by_name() {
        let (m, primary, secondary) = fake_image();
        let index = RttiIndex::scan(&m);
        assert_eq!(index.type_descriptors.len(), 3);
        assert_eq!(index.primary_vtable(".?AVDerived@@"), Some(primary));
        let all: Vec<_> = index.vtables_of(".?AVDerived@@").map(|v| (v.vtable_rva, v.offset)).collect();
        assert_eq!(all, [(primary, 0), (secondary, 0x28)]);
        assert_eq!(vtable_len(&m, primary), 3);
        assert_eq!(vtable_len(&m, secondary), 1);
    }

    #[test]
    fn mangling_round_trips() {
        for name in ["TESObjectWEAP", "BSScript::Internal::VirtualMachine", "RE::Foo"] {
            assert_eq!(demangle(&mangle(name, false)), name);
        }
        assert_eq!(mangle("NiPoint3", true), ".?AUNiPoint3@@");
        assert_eq!(demangle(".?AV?$BSTEventSink@VMenuOpenCloseEvent@@@@"), ".?AV?$BSTEventSink@VMenuOpenCloseEvent@@@@");
    }
}
