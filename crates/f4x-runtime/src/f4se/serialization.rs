//! The Serialization interface: plugins' co-save records.
//!
//! Records accumulate per plugin uid while its save callback runs; during load, a
//! plugin's load callback iterates the chunks of its uid's block. Format and behaviour:
//! `docs/formats/cosave.md`, `f4x-cosave`.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;

use f4x_abi::f4se::{PluginHandle, SerializationCallback, SerializationInterface};
use f4x_cosave::{Chunk, plgn};

#[derive(Default, Clone)]
pub(crate) struct Registration {
    pub uid: Option<u32>,
    pub revert: Option<usize>,
    pub save: Option<usize>,
    pub load: Option<usize>,
}

enum Mode {
    Idle,
    Saving { chunks: Vec<Chunk>, open: bool },
    Loading { chunks: Vec<Chunk>, next: usize, current: Option<(usize, usize)> },
}

struct State {
    registrations: BTreeMap<PluginHandle, Registration>,
    mode: Mode,
    /// The load order stored in the co-save being loaded, for `ResolveFormID`.
    save_order: Vec<plgn::Entry>,
}

static STATE: Mutex<State> = Mutex::new(State { registrations: BTreeMap::new(), mode: Mode::Idle, save_order: Vec::new() });

pub static INTERFACE: SerializationInterface = SerializationInterface {
    version: SerializationInterface::VERSION,
    set_unique_id: Some(set_unique_id),
    set_revert_callback: Some(set_revert),
    set_save_callback: Some(set_save),
    set_load_callback: Some(set_load),
    set_form_delete_callback: Some(set_form_delete),
    write_record: Some(write_record),
    open_record: Some(open_record),
    write_record_data: Some(write_record_data),
    get_next_record_info: Some(get_next_record_info),
    read_record_data: Some(read_record_data),
    resolve_handle: Some(resolve_handle),
    resolve_form_id: Some(resolve_form_id),
};

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut state = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut state)
}

fn register(handle: PluginHandle, f: impl FnOnce(&mut Registration)) {
    with(|s| f(s.registrations.entry(handle).or_default()));
}

unsafe extern "C" fn set_unique_id(handle: PluginHandle, uid: u32) {
    log::debug!(target: "f4x::f4se", "plugin {handle} co-save id {}", f4x_cosave::fourcc(uid));
    register(handle, |r| r.uid = Some(uid));
}
unsafe extern "C" fn set_revert(handle: PluginHandle, callback: *mut c_void) {
    register(handle, |r| r.revert = (!callback.is_null()).then_some(callback as usize));
}
unsafe extern "C" fn set_save(handle: PluginHandle, callback: *mut c_void) {
    register(handle, |r| r.save = (!callback.is_null()).then_some(callback as usize));
}
unsafe extern "C" fn set_load(handle: PluginHandle, callback: *mut c_void) {
    register(handle, |r| r.load = (!callback.is_null()).then_some(callback as usize));
}
unsafe extern "C" fn set_form_delete(_handle: PluginHandle, _callback: *mut c_void) {}

unsafe extern "C" fn write_record(kind: u32, version: u32, buf: *const c_void, len: u32) -> bool {
    // SAFETY: the plugin passes `len` readable bytes.
    let data = if len == 0 || buf.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), len as usize) }.to_vec() };
    with(|s| match &mut s.mode {
        Mode::Saving { chunks, open } => {
            chunks.push(Chunk { kind, version, data });
            *open = false;
            true
        }
        _ => false,
    })
}

unsafe extern "C" fn open_record(kind: u32, version: u32) -> bool {
    with(|s| match &mut s.mode {
        Mode::Saving { chunks, open } => {
            chunks.push(Chunk { kind, version, data: Vec::new() });
            *open = true;
            true
        }
        _ => false,
    })
}

unsafe extern "C" fn write_record_data(buf: *const c_void, len: u32) -> bool {
    // SAFETY: as above.
    let data = if len == 0 || buf.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), len as usize) } };
    with(|s| match &mut s.mode {
        Mode::Saving { chunks, open: true } => match chunks.last_mut() {
            Some(chunk) => {
                chunk.data.extend_from_slice(data);
                true
            }
            None => false,
        },
        _ => false,
    })
}

unsafe extern "C" fn get_next_record_info(kind: *mut u32, version: *mut u32, len: *mut u32) -> bool {
    with(|s| match &mut s.mode {
        Mode::Loading { chunks, next, current } => {
            let Some(chunk) = chunks.get(*next) else { return false };
            // SAFETY: the plugin passes valid out pointers.
            unsafe {
                *kind = chunk.kind;
                *version = chunk.version;
                *len = chunk.data.len() as u32;
            }
            *current = Some((*next, 0));
            *next += 1;
            true
        }
        _ => false,
    })
}

unsafe extern "C" fn read_record_data(buf: *mut c_void, len: u32) -> u32 {
    with(|s| match &mut s.mode {
        Mode::Loading { chunks, current: Some((index, read)), .. } => {
            let chunk = &chunks[*index];
            let n = (len as usize).min(chunk.data.len() - *read);
            // SAFETY: the plugin passes `len` writable bytes.
            unsafe { std::ptr::copy_nonoverlapping(chunk.data[*read..].as_ptr(), buf.cast::<u8>(), n) };
            *read += n;
            n as u32
        }
        _ => 0,
    })
}

unsafe extern "C" fn resolve_handle(handle: u64, out: *mut u64) -> bool {
    // Reference handles stay valid across a load in this engine; TODO: map their form id.
    // SAFETY: caller-provided out pointer.
    unsafe { *out = handle };
    true
}

/// Map a form id from the save's load order to the current one (by file name), for
/// full (`II xxxxxx`) and light (`FE LLL xxx`) files.
unsafe extern "C" fn resolve_form_id(form_id: u32, out: *mut u32) -> bool {
    match remap_form_id(form_id) {
        Some(id) => {
            // SAFETY: caller-provided out pointer.
            unsafe { *out = id };
            true
        }
        None => false,
    }
}

/// See [`resolve_form_id`].
pub(crate) fn remap_form_id(form_id: u32) -> Option<u32> {
    let index = (form_id >> 24) as u8;
    let resolved = if index == 0xFF {
        Some(form_id) // created at run time
    } else if index == plgn::LIGHT {
        let light = ((form_id >> 12) & 0xFFF) as u16;
        let name = with(|s| s.save_order.iter().find(|e| e.light_index == Some(light)).map(|e| e.name.clone()));
        match name {
            None => Some(form_id),
            Some(name) => current_file(&name)
                .and_then(|f| f.light_index)
                .map(|l| 0xFE00_0000 | (u32::from(l) << 12) | (form_id & 0xFFF)),
        }
    } else {
        let name = with(|s| s.save_order.iter().find(|e| e.index == index && e.light_index.is_none()).map(|e| e.name.clone()));
        match name {
            None => Some(form_id), // no co-save load order (e.g. no co-save): identity
            Some(name) => current_file(&name)
                .filter(|f| f.light_index.is_none())
                .map(|f| (u32::from(f.index) << 24) | (form_id & 0x00FF_FFFF)),
        }
    };
    resolved
}

fn current_file(name: &str) -> Option<f4x_engine::File> {
    f4x_engine::DataHandler::get()?.load_order().into_iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

/// The current load order as `PLGN` entries (full and light, in load order).
pub fn current_load_order() -> Vec<plgn::Entry> {
    f4x_engine::DataHandler::get()
        .map(|h| {
            h.load_order()
                .into_iter()
                .map(|f| match f.light_index {
                    Some(light) => plgn::Entry::light(light, &f.name),
                    None => plgn::Entry::full(f.index, &f.name),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn registrations() -> Vec<(PluginHandle, Registration)> {
    with(|s| s.registrations.iter().map(|(h, r)| (*h, r.clone())).collect())
}

/// Run a plugin's save callback and return the chunks it wrote.
pub fn collect_save(callback: usize) -> Vec<Chunk> {
    with(|s| s.mode = Mode::Saving { chunks: Vec::new(), open: false });
    // SAFETY: the plugin's `void(const SerializationInterface*)` callback.
    unsafe { std::mem::transmute::<usize, SerializationCallback>(callback)(&INTERFACE) };
    with(|s| match std::mem::replace(&mut s.mode, Mode::Idle) {
        Mode::Saving { chunks, .. } => chunks,
        _ => Vec::new(),
    })
}

/// Run a plugin's load callback over `chunks`.
pub fn run_load(callback: usize, chunks: Vec<Chunk>) {
    with(|s| s.mode = Mode::Loading { chunks, next: 0, current: None });
    // SAFETY: as above.
    unsafe { std::mem::transmute::<usize, SerializationCallback>(callback)(&INTERFACE) };
    with(|s| s.mode = Mode::Idle);
}

/// Run a revert callback.
pub fn run_revert(callback: usize) {
    // SAFETY: as above.
    unsafe { std::mem::transmute::<usize, SerializationCallback>(callback)(&INTERFACE) };
}

pub fn set_save_order(order: Vec<plgn::Entry>) {
    with(|s| s.save_order = order);
}
