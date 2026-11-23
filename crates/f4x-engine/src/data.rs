//! `TESDataHandler`: loaded files and forms by type.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Mutex;

use crate::form::Form;
use crate::form_id::FormId;
use crate::form_type::FormType;
use crate::layout::RE;
use crate::mem::{BSTArray, read};

#[derive(Clone, Copy)]
pub struct DataHandler(NonNull<u8>);

/// A loaded plugin file.
#[derive(Clone, Debug)]
pub struct File {
    pub name: String,
    pub index: u8,
    pub light_index: Option<u16>,
}

static INDEX: Mutex<Option<(usize, HashMap<u32, usize>)>> = Mutex::new(None);

impl DataHandler {
    /// The singleton (None before the engine creates it).
    ///
    /// `TESDataHandler` has no vtable, so it is found structurally: the global whose object
    /// has, for several form types `t`, a non-empty `formArrays[t]` whose first form's
    /// `formType` is `t`.
    pub fn get() -> Option<DataHandler> {
        const PROBE_TYPES: [u8; 6] = [0x04, 0x09, 0x1D, 0x2B, 0x2D, 0x30]; // KYWD GLOB ARMO WEAP NPC_ ALCH
        let ptr = crate::game().object_by_structure("TESDataHandler", |object, regions| {
            if !regions.contains(object, RE::TESDataHandler::SIZE) {
                return false;
            }
            PROBE_TYPES.iter().all(|&t| {
                let array = object + RE::TESDataHandler::formArrays + 0x18 * t as usize;
                // SAFETY: inside the readable object (checked above).
                let (data, capacity, size): (usize, u32, u32) =
                    unsafe { (read(array as *const u8, 0), read(array as *const u8, 8), read(array as *const u8, 0x10)) };
                if size == 0 || size > 2_000_000 || capacity < size || !regions.contains(data, 8) {
                    return false;
                }
                // SAFETY: readable (checked).
                let first: usize = unsafe { read(data as *const u8, 0) };
                regions.contains(first, RE::TESForm::SIZE)
                    // SAFETY: readable (checked).
                    && unsafe { read::<u8>(first as *const u8, RE::TESForm::formType) } == t
            })
        })?;
        NonNull::new(ptr).map(DataHandler)
    }

    fn ptr(self) -> *const u8 {
        self.0.as_ptr()
    }

    /// Forms of one type (`formArrays[type]`).
    pub fn forms_of_type(self, form_type: FormType) -> impl Iterator<Item = Form> {
        let offset = RE::TESDataHandler::formArrays + 0x18 * form_type.0 as usize;
        // SAFETY: formArrays is BSTArray<TESForm*>[159] (generated layout).
        let array = unsafe { BSTArray::<*mut u8>::at(self.ptr().add(offset)) };
        (0..array.len()).filter_map(move |i| array.get(i)).filter_map(Form::checked)
    }

    pub fn count_of_type(self, form_type: FormType) -> usize {
        let offset = RE::TESDataHandler::formArrays + 0x18 * form_type.0 as usize;
        // SAFETY: as above.
        unsafe { BSTArray::<*mut u8>::at(self.ptr().add(offset)) }.len()
    }

    /// Find a form by id among the data handler's per-type arrays. Builds an index on
    /// first use and rebuilds it when the total count changes.
    pub fn lookup(self, id: FormId) -> Option<Form> {
        let total: usize = (0..FormType::COUNT as u8).map(|t| self.count_of_type(FormType(t))).sum();
        let mut index = INDEX.lock().ok()?;
        if index.as_ref().is_none_or(|(count, _)| *count != total) {
            let mut map = HashMap::with_capacity(total);
            for t in 0..FormType::COUNT as u8 {
                for form in self.forms_of_type(FormType(t)) {
                    map.insert(form.id().0, form.as_ptr() as usize);
                }
            }
            *index = Some((total, map));
        }
        let ptr = *index.as_ref()?.1.get(&id.0)?;
        Form::checked(ptr as *mut u8)
    }

    /// Loaded files: full plugins then light plugins (`compiledFileCollection`).
    pub fn files(self) -> Vec<File> {
        let collection = RE::TESDataHandler::compiledFileCollection;
        let mut out = Vec::new();
        for (array_offset, light) in [(RE::TESFileCollection::files, false), (RE::TESFileCollection::smallFiles, true)] {
            // SAFETY: TESFileCollection holds two BSTArray<TESFile*>.
            let array = unsafe { BSTArray::<*const u8>::at(self.ptr().add(collection + array_offset)) };
            for (i, file) in array.iter().enumerate() {
                if file.is_null() {
                    continue;
                }
                // SAFETY: TESFile::filename is char[260].
                let name = unsafe { crate::mem::c_str(file.add(RE::TESFile::filename)) };
                out.push(File {
                    name,
                    index: if light { 0xFE } else { i as u8 },
                    light_index: light.then_some(i as u16),
                });
            }
        }
        out
    }

    /// Active files in load order (full and light interleaved, as the game loaded them):
    /// `TESDataHandler::files` (a `BSSimpleList<TESFile*>` with an inline head node
    /// `{value, next}`), keeping files with a compile index (CommonLibF4 layouts).
    pub fn load_order(self) -> Vec<File> {
        let mut out = Vec::new();
        let mut node = unsafe { self.ptr().add(RE::TESDataHandler::files) };
        for _ in 0..4096 {
            // SAFETY: list nodes: TESFile* value at 0, next node at 8.
            let (file, next): (*const u8, *const u8) = unsafe { (read(node, 0), read(node, 8)) };
            if !file.is_null() {
                // SAFETY: a live TESFile.
                let (index, light): (u8, u16) =
                    unsafe { (read(file, RE::TESFile::compileIndex), read(file, RE::TESFile::smallFileCompileIndex)) };
                if index != 0xFF {
                    // SAFETY: TESFile::filename is char[260].
                    let name = unsafe { crate::mem::c_str(file.add(RE::TESFile::filename)) };
                    out.push(File { name, index, light_index: (index == 0xFE).then_some(light) });
                }
            }
            if next.is_null() {
                break;
            }
            node = next;
        }
        out
    }

    /// Total forms in the per-type arrays.
    pub fn form_count(self) -> usize {
        (0..FormType::COUNT as u8).map(|t| self.count_of_type(FormType(t))).sum()
    }

    pub fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }
}


/// Any form by id, placed references included: a scan of the engine's global form map
/// (`TESForm::allForms`, a `BSTHashMap<u32, TESForm*>`; CommonLibF4 `BSTHashMap.h`
/// layout: capacity at +0x0C, entries at +0x28, 24-byte entries `{key, form, next}`, in
/// use when `next` is set). Main thread; meant for tools, not hot paths.
#[must_use]
pub fn form_by_id(id: crate::FormId) -> Option<Form> {
    use f4x_engine_sys::og_1_10_163_addresses::ALL_FORMS;
    let global = crate::address::resolve(&ALL_FORMS)?;
    // SAFETY: the allForms global (a pointer to the map) and the map's own fields.
    unsafe {
        let map: *const u8 = read(global as *const u8, 0);
        if map.is_null() {
            return None;
        }
        let capacity: u32 = read(map, 0x0C);
        let entries: *const u8 = read(map, 0x28);
        if entries.is_null() || !crate::mem::readable(entries as usize, capacity as usize * 24) {
            return None;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * 24);
            let next: usize = read(entry, 16);
            if next != 0 && read::<u32>(entry, 0) == id.0 {
                return Form::checked(read::<*mut u8>(entry, 8));
            }
        }
    }
    None
}

/// Every form of a type in the global form map, placed references included (up to
/// `limit`, in map order). Main thread; for tools.
#[must_use]
pub fn all_forms_of_type(form_type: crate::form_type::FormType, limit: usize) -> Vec<Form> {
    use f4x_engine_sys::og_1_10_163_addresses::ALL_FORMS;
    let mut out = Vec::new();
    let Some(global) = crate::address::resolve(&ALL_FORMS) else { return out };
    // SAFETY: as in `form_by_id`.
    unsafe {
        let map: *const u8 = read(global as *const u8, 0);
        if map.is_null() {
            return out;
        }
        let capacity: u32 = read(map, 0x0C);
        let entries: *const u8 = read(map, 0x28);
        if entries.is_null() || !crate::mem::readable(entries as usize, capacity as usize * 24) {
            return out;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * 24);
            if read::<usize>(entry, 16) == 0 {
                continue;
            }
            if let Some(form) = Form::checked(read::<*mut u8>(entry, 8))
                && form.form_type() == form_type
                && !form.is_deleted()
            {
                out.push(form);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out.sort_by_key(|f| f.id().0);
    out
}
