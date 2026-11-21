//! The running game image: RTTI-based class lookups and singleton discovery.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use f4x_pe::rtti::{RttiIndex, class_info, demangle};
use f4x_pe::{Image, MappedImage};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::mem::{self, Regions};

pub struct Game {
    image: MappedImage,
    rtti: RttiIndex,
    base: usize,
    /// class (mangled) -> RVAs of globals that point at its instance.
    singletons: Mutex<HashMap<String, Vec<u32>>>,
    casts: Mutex<HashMap<(usize, &'static str), Option<isize>>>,
    structural: Mutex<HashMap<String, (Found, u32)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// A global at the RVA points to the object.
    Pointer,
    /// The object itself lives at the RVA.
    Static,
}

/// The game (first call scans RTTI, ~160 ms).
pub fn game() -> &'static Game {
    static GAME: OnceLock<Game> = OnceLock::new();
    GAME.get_or_init(|| {
        // SAFETY: the main module is the game exe, mapped for the process lifetime.
        let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
        let image = unsafe { MappedImage::from_base(base) }.expect("game image headers");
        let rtti = RttiIndex::scan(&image);
        Game {
            image,
            rtti,
            base,
            singletons: Mutex::new(HashMap::new()),
            casts: Mutex::new(HashMap::new()),
            structural: Mutex::new(HashMap::new()),
        }
    })
}

impl Game {
    pub fn base(&self) -> usize {
        self.base
    }

    pub fn image(&self) -> &MappedImage {
        &self.image
    }

    pub fn rtti(&self) -> &RttiIndex {
        &self.rtti
    }

    /// Absolute address of a class's primary vtable.
    pub fn vtable(&self, mangled: &str) -> Option<usize> {
        self.rtti.primary_vtable(mangled).map(|rva| self.base + rva as usize)
    }

    /// The demangled class name of a polymorphic object.
    ///
    /// # Safety
    /// `object` must point to a live object with a vtable.
    pub unsafe fn class_name(&self, object: *const u8) -> Option<String> {
        // SAFETY: guaranteed by the caller.
        let vtable: usize = unsafe { mem::read(object, 0) };
        class_info(&self.image, vtable.checked_sub(self.base)? as u32).map(|c| demangle(&c.mangled_name))
    }

    /// `dynamic_cast` to a non-virtual base class (`target` mangled, e.g.
    /// `.?AVTESFullName@@`). Cached per (vtable, target).
    ///
    /// # Safety
    /// `object` must point to a live object with a vtable.
    pub unsafe fn cast(&self, object: *const u8, target: &'static str) -> Option<*const u8> {
        // SAFETY: guaranteed by the caller.
        let vtable: usize = unsafe { mem::read(object, 0) };
        let mut casts = self.casts.lock().ok()?;
        let offset = *casts.entry((vtable, target)).or_insert_with(|| {
            class_info(&self.image, (vtable.checked_sub(self.base)?) as u32).and_then(|c| c.base_offset(target))
        });
        offset.map(|o| object.wrapping_offset(o))
    }

    /// The live instance of a singleton class: a global in the game's writable data that
    /// points at an object whose vtable is the class's primary vtable. The global's RVA is
    /// found once (a full scan, tens of ms) and re-read afterwards.
    pub fn singleton(&self, mangled: &str) -> Option<*mut u8> {
        let vtable = self.vtable(mangled)?;
        if let Some(globals) = self.singletons.lock().ok()?.get(mangled).cloned() {
            for rva in globals {
                // SAFETY: a global slot inside the mapped image.
                let object: usize = unsafe { mem::read((self.base + rva as usize) as *const u8, 0) };
                if object != 0 && mem::readable(object, 8) && unsafe { mem::read::<usize>(object as *const u8, 0) } == vtable {
                    return Some(object as *mut u8);
                }
            }
        }
        let globals = self.scan_pointer_globals(vtable);
        let found = globals.first().map(|&rva| unsafe { mem::read::<usize>((self.base + rva as usize) as *const u8, 0) } as *mut u8);
        if !globals.is_empty()
            && let Ok(mut cache) = self.singletons.lock()
        {
            cache.insert(mangled.to_owned(), globals);
        }
        found
    }

    /// RVAs of globals currently pointing at an object of the class with `vtable`.
    pub fn scan_pointer_globals(&self, vtable: usize) -> Vec<u32> {
        let regions = Regions::snapshot();
        let image_end = self.base + self.image.size();
        let mut found = Vec::new();
        for section in self.image.sections().iter().filter(|s| s.is_writable() && !s.is_executable()) {
            let bytes = self.image.section_bytes(section);
            for (i, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
                let value = usize::from_le_bytes(*chunk);
                if value < 0x10000 || value % 8 != 0 || (value >= self.base && value < image_end) {
                    continue;
                }
                if regions.contains(value, 8)
                    // SAFETY: checked readable just above.
                    && unsafe { mem::read::<usize>(value as *const u8, 0) } == vtable
                {
                    found.push(section.rva + 8 * i as u32);
                }
            }
        }
        found
    }

    /// An engine object found by a structural predicate (for classes without RTTI).
    /// Looks at, in order: objects that globals in the game's writable data point to
    /// (heap or static), then objects stored directly in the writable data. The result
    /// is cached under `key` (as the global's RVA, or the static object's RVA).
    pub fn object_by_structure(&self, key: &str, predicate: impl Fn(usize, &Regions) -> bool) -> Option<*mut u8> {
        if let Some(&(kind, rva)) = self.structural.lock().ok()?.get(key) {
            let slot = self.base + rva as usize;
            return Some(match kind {
                // SAFETY: a global slot inside the mapped image.
                Found::Pointer => (unsafe { mem::read::<usize>(slot as *const u8, 0) }) as *mut u8,
                Found::Static => slot as *mut u8,
            })
            .filter(|p| !p.is_null());
        }
        let regions = Regions::snapshot();
        let writable: Vec<_> = self.image.sections().iter().filter(|s| s.is_writable() && !s.is_executable()).cloned().collect();
        let remember = |kind: Found, rva: u32| {
            if let Ok(mut cache) = self.structural.lock() {
                cache.insert(key.to_owned(), (kind, rva));
            }
        };
        for section in &writable {
            let bytes = self.image.section_bytes(section);
            for (i, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
                let value = usize::from_le_bytes(*chunk);
                if value < 0x10000 || value % 8 != 0 || !regions.contains(value, 8) {
                    continue;
                }
                if predicate(value, &regions) {
                    remember(Found::Pointer, section.rva + 8 * i as u32);
                    return Some(value as *mut u8);
                }
            }
        }
        for section in &writable {
            let start = self.base + section.rva as usize;
            for offset in (0..section.virtual_size as usize).step_by(8) {
                if predicate(start + offset, &regions) {
                    remember(Found::Static, section.rva + offset as u32);
                    return Some((start + offset) as *mut u8);
                }
            }
        }
        None
    }

    /// Globals found so far, for diagnostics: (name, how, [rva]).
    pub fn known_singletons(&self) -> Vec<(String, Vec<u32>)> {
        let mut out: Vec<(String, Vec<u32>)> = Vec::new();
        if let Ok(cache) = self.singletons.lock() {
            out.extend(cache.iter().map(|(k, v)| (demangle(k), v.clone())));
        }
        if let Ok(cache) = self.structural.lock() {
            out.extend(cache.iter().map(|(k, (kind, rva))| (format!("{k} ({kind:?})"), vec![*rva])));
        }
        out.sort();
        out
    }
}
