//! The Scaleform translation table: the `$KEY` → text map that `BSScaleformTranslator`
//! holds and that every movie's text fields are translated through.
//!
//! Layouts:
//! - `BSScaleformManager` (CommonLibF4 `RE/Bethesda/BSScaleformManager.h`): the GFx
//!   `Loader*` at +0x10;
//! - `GFx::Loader` is a `StateBag` (`RE/Scaleform/GFx/GFx_Loader.h`): `SetState` in
//!   vtable slot 2, `GetStateAddRef` in slot 3; `State::StateType::kTranslator` = 1;
//! - `BSScaleformTranslator` (CommonLibF4 community branch, same header):
//!   `GFx::Translator` (0x20) then `BSTHashMap<BSFixedStringWCS, BSFixedStringWCS>` at
//!   +0x20 (`BSTScatterTable`, `RE/Bethesda/BSTHashMap.h`: capacity `u32` at +0xC, free
//!   at +0x10, sentinel at +0x18, entries at +0x28; an entry is key, value, next);
//! - wide string-pool entries (`RE/Bethesda/BSStringPool.h`): text after the 0x18-byte
//!   header, `kShallow` entries chain through +0x10.
//!
//! All the same on NG 1.11.240 (next-gen fork `BSScaleformManager.h`, `GFx_StateBag.h`,
//! `GFx_State.h`, `BSScaleformTranslator.h`, `BSTHashMap.h`, `BSStringPool.h`,
//! `Setting.h`, `BSResourceNiBinaryStream.h`; our RE: the NG manager constructor, 0x1b1a9c0,
//! keeps the loader at +0x10 and calls `SetState` at slot 2; the translator's destructor
//! walks entries at +0x48 with capacity at +0x2C and stride 0x18; `INISettingCollection`
//! adds settings to its list at +0x118; the stream's `operator bool` tests +0x10).
//!
//! Our own analysis of the BSScaleformManager constructor (0x2110430 in 1.10.163) shows
//! how the game fills the table: it builds `Interface\Translate_<sLanguage:General>.txt`,
//! creates the translator, parses the file into it (`scaleform_translator_load`, which
//! reads UTF-16 tokens separated by TAB/CR/LF/BOM in key, value pairs and inserts each
//! with `translation_map_insert`, an insert-or-assign), then hands it to the loader
//! (`SetState(kTranslator)`).

// The pointer-taking functions check what they are given (readability, the loader's
// vtable) before using it, like the rest of f4x-engine; they are safe to call with any
// pointer value.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;

use f4x_engine_sys::addresses::{
    INI_SETTING_COLLECTION_SINGLETON, SCALEFORM_MANAGER_SINGLETON, STRING_POOL_ENTRY_RELEASE,
    STRING_POOL_GET_ENTRY_WIDE, TRANSLATION_MAP_INSERT,
};

use crate::address::resolve;
use crate::layout::RE;
use crate::mem;

/// `State::StateType::kTranslator`.
pub const STATE_TRANSLATOR: i32 = 1;
/// `State::StateType::kLog`.
pub const STATE_LOG: i32 = 2;

pub const TRANSLATOR_RTTI: &str = ".?AVBSScaleformTranslator@@";
const MANAGER_RTTI: &str = ".?AVBSScaleformManager@@";

const MAP: usize = 0x20;
const MAP_CAPACITY: usize = MAP + 0x0C;
const MAP_FREE: usize = MAP + 0x10;
const MAP_SENTINEL: usize = MAP + 0x18;
const MAP_ENTRIES: usize = MAP + 0x28;
const ENTRY_SIZE: usize = 0x18;

/// The GFx loader of the `BSScaleformManager`, once it exists. (Scaleform's classes
/// have no RTTI; the manager's has, and the loader must have a vtable in the game image.)
#[must_use]
pub fn loader() -> Option<*mut u8> {
    let global = resolve(&SCALEFORM_MANAGER_SINGLETON)?;
    // SAFETY: the BSScaleformManager* global.
    let manager: *mut u8 = unsafe { mem::read(global as *const u8, 0) };
    let game = crate::game();
    if !game.is_instance_of(manager as usize, MANAGER_RTTI) {
        return None;
    }
    // SAFETY: a checked BSScaleformManager; Loader* at +0x10.
    let loader: *mut u8 = unsafe { mem::read(manager, RE::BSScaleformManager::loader) };
    if !mem::readable(loader as usize, 0x20) {
        return None;
    }
    // SAFETY: readable (checked).
    let vtable: usize = unsafe { mem::read(loader, 0) };
    let in_image = |p: usize| p >= game.base() && p < game.base() + game.image().size();
    (in_image(vtable) && mem::readable(vtable, 0x20)).then_some(loader)
}

/// `StateBag::GetStateAddRef(type)` on the loader, with the added reference dropped
/// again (the loader keeps its own, so the state stays alive).
#[must_use]
pub fn loader_state(loader: *mut u8, state_type: i32) -> Option<*mut u8> {
    if !mem::readable(loader as usize, 0x20) {
        return None;
    }
    let game = crate::game();
    // SAFETY: readable (checked).
    let vtable: usize = unsafe { mem::read(loader, 0) };
    if vtable < game.base() || vtable >= game.base() + game.image().size() || !mem::readable(vtable, 0x20) {
        return None;
    }
    // SAFETY: a checked GFx::Loader; GetStateAddRef is vtable slot 3.
    let state = unsafe {
        let vtable: *const usize = mem::read(loader, 0);
        let get: unsafe extern "C" fn(*mut u8, i32) -> *mut u8 = std::mem::transmute(*vtable.add(RE::Scaleform::GFx::StateBag::vtbl::GetStateAddRef));
        get(loader, state_type)
    };
    if state.is_null() {
        return None;
    }
    // SAFETY: a live RefCountImpl object: the count (i32) at +8.
    unsafe {
        let count = &*state.add(8).cast::<std::sync::atomic::AtomicI32>();
        if count.load(std::sync::atomic::Ordering::Acquire) > 1 {
            count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
    Some(state)
}

/// The game's `BSScaleformTranslator` (the loader's translator state).
#[must_use]
pub fn translator() -> Option<*mut u8> {
    let state = loader_state(loader()?, STATE_TRANSLATOR)?;
    crate::game().is_instance_of(state as usize, TRANSLATOR_RTTI).then_some(state)
}

/// The loader's translator state, whatever its class, if it carries a translation map
/// laid out like `BSScaleformTranslator`'s (the map's sentinel is a global of the game
/// image, the capacity a power of two). For observing other hosts' translators.
#[must_use]
pub fn translator_like() -> Option<*mut u8> {
    let state = loader_state(loader()?, STATE_TRANSLATOR)?;
    if crate::game().is_instance_of(state as usize, TRANSLATOR_RTTI) {
        return Some(state);
    }
    if !looks_valid(state) {
        return None;
    }
    let game = crate::game();
    // SAFETY: readable (checked by looks_valid).
    let (sentinel, capacity) =
        unsafe { (mem::read::<usize>(state, MAP_SENTINEL), mem::read::<u32>(state, MAP_CAPACITY)) };
    let in_image = sentinel >= game.base() && sentinel < game.base() + game.image().size();
    (in_image && (capacity == 0 || capacity.is_power_of_two())).then_some(state)
}

/// Read a wide string-pool entry (`BSFixedStringW*`).
///
/// # Safety
/// `entry` must be null or a live string-pool entry.
pub unsafe fn wide_fixed_string(mut entry: *const u8) -> Option<String> {
    if entry.is_null() {
        return Some(String::new());
    }
    for _ in 0..16 {
        if !mem::readable(entry as usize, 0x1A) {
            return None;
        }
        // SAFETY: readable entry header.
        let flags: u16 = unsafe { mem::read(entry, 8) };
        if flags & (1 << 14) == 0 {
            if flags & (1 << 15) == 0 {
                return None;
            }
            // SAFETY: NUL-terminated UTF-16 after the header.
            let text = unsafe { entry.add(0x18).cast::<u16>() };
            let mut len = 0usize;
            // SAFETY: as above; bounded.
            while len < 1 << 16 && unsafe { *text.add(len) } != 0 {
                len += 1;
            }
            // SAFETY: `len` units were just read.
            return Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) }));
        }
        // SAFETY: shallow entries chain at +0x10.
        entry = unsafe { mem::read::<*const u8>(entry, 0x10) };
        if entry.is_null() {
            return None;
        }
    }
    None
}

/// Size and capacity of a translator's map.
#[must_use]
pub fn stats(translator: *mut u8) -> (u32, u32) {
    if !looks_valid(translator) {
        return (0, 0);
    }
    // SAFETY: a checked BSScaleformTranslator.
    unsafe {
        let capacity: u32 = mem::read(translator, MAP_CAPACITY);
        let free: u32 = mem::read(translator, MAP_FREE);
        (capacity.saturating_sub(free), capacity)
    }
}

/// Every `(key, value)` in a translator's map, in table order.
#[must_use]
pub fn entries(translator: *mut u8) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !looks_valid(translator) {
        return out;
    }
    // SAFETY: a checked BSScaleformTranslator; entries are read only while readable.
    unsafe {
        let capacity: u32 = mem::read(translator, MAP_CAPACITY);
        let entries: *const u8 = mem::read(translator, MAP_ENTRIES);
        if entries.is_null()
            || capacity == 0
            || !mem::readable(entries as usize, capacity as usize * ENTRY_SIZE)
        {
            return out;
        }
        for i in 0..capacity as usize {
            let entry = entries.add(i * ENTRY_SIZE);
            let next: *const u8 = mem::read(entry, 0x10);
            if next.is_null() {
                continue;
            }
            let key: *const u8 = mem::read(entry, 0);
            let value: *const u8 = mem::read(entry, 8);
            if let (Some(k), Some(v)) = (wide_fixed_string(key), wide_fixed_string(value)) {
                out.push((k, v));
            }
        }
    }
    out
}

/// A `BSFixedStringWCS` (case-sensitive wide string-pool entry), released on drop.
struct WideString(*mut c_void);

impl WideString {
    fn new(text: &str) -> Option<WideString> {
        let get = resolve(&STRING_POOL_GET_ENTRY_WIDE)?;
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut entry: *mut c_void = std::ptr::null_mut();
        type GetEntry = unsafe extern "C" fn(*mut *mut c_void, *const u16, bool);
        // SAFETY: BSStringPool::GetEntry<wchar_t>(Entry*&, const wchar_t*, caseSensitive).
        unsafe { std::mem::transmute::<usize, GetEntry>(get)(&mut entry, wide.as_ptr(), true) };
        (!entry.is_null()).then_some(WideString(entry))
    }
}

impl Drop for WideString {
    fn drop(&mut self) {
        if let Some(release) = resolve(&STRING_POOL_ENTRY_RELEASE) {
            type Release = unsafe extern "C" fn(*mut *mut c_void);
            // SAFETY: BSStringPool::Entry::release(Entry*&) on our own reference.
            unsafe { std::mem::transmute::<usize, Release>(release)(&mut self.0) };
        }
    }
}

/// Set `key` to `value` in a translator's map (insert or overwrite), as the game's own
/// file parser does. Main thread (or the thread that owns the translator) only.
pub fn insert(translator: *mut u8, key: &str, value: &str) -> bool {
    if !crate::game().is_instance_of(translator as usize, TRANSLATOR_RTTI) {
        return false;
    }
    let Some(function) = resolve(&TRANSLATION_MAP_INSERT) else { return false };
    let (Some(key), Some(value)) = (WideString::new(key), WideString::new(value)) else { return false };
    type Insert = unsafe extern "C" fn(*mut u8, *const *mut c_void, *const *mut c_void);
    // SAFETY: the map's insert-or-assign with BSFixedStringWCS references; it copies
    // (adds references to) both strings.
    unsafe { std::mem::transmute::<usize, Insert>(function)(translator.add(MAP), &key.0, &value.0) };
    true
}

/// The current value of a string INI setting (`"sLanguage:General"`), from the game's
/// `INISettingCollection` (CommonLibF4 `RE/Bethesda/Settings.h`: `BSSimpleList<Setting*>`
/// at +0x118; `Setting` value at +8, key at +0x10).
#[must_use]
pub fn ini_string(name: &str) -> Option<String> {
    let global = resolve(&INI_SETTING_COLLECTION_SINGLETON)?;
    // SAFETY: the INISettingCollection* global.
    let collection: *const u8 = unsafe { mem::read(global as *const u8, 0) };
    if collection.is_null() || !mem::readable(collection as usize, 0x128) {
        return None;
    }
    // SAFETY: the list head node is embedded at +0x118: item, next.
    let mut node = unsafe { collection.add(0x118) };
    for _ in 0..100_000 {
        if !mem::readable(node as usize, 0x10) {
            return None;
        }
        // SAFETY: readable node.
        let (setting, next): (*const u8, *const u8) = unsafe { (mem::read(node, 0), mem::read(node, 8)) };
        if !setting.is_null() && mem::readable(setting as usize, 0x18) {
            // SAFETY: a Setting: key C string at +0x10.
            let key: *const u8 = unsafe { mem::read(setting, RE::Setting::_key) };
            if !key.is_null()
                && mem::readable(key as usize, 1)
                && unsafe { mem::c_str(key) }.eq_ignore_ascii_case(name)
            {
                // SAFETY: string settings hold a char* value at +8.
                let value: *const u8 = unsafe { mem::read(setting, RE::Setting::_value) };
                return (!value.is_null() && mem::readable(value as usize, 1))
                    .then(|| unsafe { mem::c_str(value) });
            }
        }
        if next.is_null() {
            return None;
        }
        node = next;
    }
    None
}

/// Whether `translator` still uses the game's own map sentinel (a sanity check).
#[must_use]
pub fn looks_valid(translator: *mut u8) -> bool {
    // SAFETY: readable (checked first).
    mem::readable(translator as usize, 0x50)
        && unsafe { !mem::read::<*const u8>(translator, MAP_SENTINEL).is_null() }
}

/// The whole content of a game resource (`Data`-relative path such as
/// `Interface\Translations\Name_en.txt`), found the way the game finds its own files:
/// loose files and the archives registered so far (CommonLibF4 community
/// `RE/Bethesda/BSResourceNiBinaryStream.h`: a 0x30-byte `NiBinaryStream` whose
/// `BSResource::Stream` smart pointer at +0x10 is null when nothing was found; `DoRead` is
/// vtable slot 5). `None` if the file does not exist.
#[must_use]
pub fn read_resource(path: &str) -> Option<Vec<u8>> {
    use f4x_engine_sys::addresses::{RESOURCE_STREAM_CTOR, RESOURCE_STREAM_DTOR};
    let (ctor, dtor) = (resolve(&RESOURCE_STREAM_CTOR)?, resolve(&RESOURCE_STREAM_DTOR)?);
    let name = std::ffi::CString::new(path).ok()?;
    #[repr(C, align(16))]
    struct Storage([u8; 0x40]);
    const _: () = assert!(RE::BSResourceNiBinaryStream::SIZE <= 0x40);
    let mut storage = Storage([0; 0x40]);
    let stream = storage.0.as_mut_ptr();
    type Ctor = unsafe extern "C" fn(*mut u8, *const std::ffi::c_char, bool, *mut c_void, bool) -> *mut u8;
    type Dtor = unsafe extern "C" fn(*mut u8);
    type DoRead = unsafe extern "C" fn(*mut u8, *mut u8, usize) -> usize;
    // SAFETY: the engine's constructor on zeroed storage of the class's size (0x30), read
    // through its own vtable, then its destructor; the storage outlives all three.
    unsafe {
        std::mem::transmute::<usize, Ctor>(ctor)(stream, name.as_ptr(), false, std::ptr::null_mut(), false);
        let found = !mem::read::<*const u8>(stream, RE::BSResourceNiBinaryStream::stream).is_null();
        let mut out = Vec::new();
        if found {
            let vtable: *const usize = mem::read(stream, 0);
            let read: DoRead = std::mem::transmute(*vtable.add(RE::BSResourceNiBinaryStream::vtbl::DoRead));
            let mut chunk = [0u8; 0x4000];
            loop {
                let n = read(stream, chunk.as_mut_ptr(), chunk.len());
                if n == 0 || n > chunk.len() {
                    break;
                }
                out.extend_from_slice(&chunk[..n]);
                if out.len() > 64 << 20 {
                    break;
                }
            }
        }
        std::mem::transmute::<usize, Dtor>(dtor)(stream);
        found.then_some(out)
    }
}
