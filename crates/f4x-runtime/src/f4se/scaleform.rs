//! `root.f4se` in every Scaleform movie, plus F4SE plugins' Scaleform callbacks.
//!
//! Black-box facts (F4SE 0.6.23; probe + `ui.*`, 2026-11-21 and 2026-11-23;
//! `provenance/scaleform-features.jsonl`):
//! - plugin callbacks run once per movie, from the call at `BSScaleformManager::LoadMovie`
//!   + 0x188 (return address +0x18d), right after the movie instance is created;
//! - `root.f4se` is built first, in this order: the functions `GetMembers`,
//!   `AllowTextInput`, `SendExternalEvent`, `CallFunctionNoWait`,
//!   `CallGlobalFunctionNoWait`, `GetDirectoryListing`, `MountImage`, `UnmountImage`,
//!   then `version` `{major, minor, beta, releaseIdx}` (ints, 0 under F4SE 0.6.23). Then
//!   each plugin callback gets (movie, the plugin's object), which becomes
//!   `root.f4se.plugins.<name>`; then `plugins` is set, then `root.f4se`. No other
//!   member exists (2961 candidate names asked with `ui.has`);
//! - `onF4SEObjCreated(codeObject)`: right after `root.f4se` is attached, the root and
//!   then each first-level display child (`numChildren`/`getChildAt`, in display order;
//!   not grandchildren, not non-display properties) is asked `HasMember` +
//!   `GetMember("onF4SEObjCreated")`; when that is a function (an AS3 method closure, GFx
//!   kind 11; a plain `Function` object is skipped) it is invoked with one argument, the
//!   `root.f4se` object. Every movie gets this, custom menus included;
//! - `GetDirectoryListing(path, pattern = "*")`:
//!   - `path` is relative to the game folder;
//!   - it returns an array in FindFirstFile order (`.` and `..` included, not
//!     recursive) of `{name, nativePath, isDirectory, isHidden, lastModified,
//!     creationDate}`. `name` and `nativePath` are both `path + "\\" + file`;
//!   - the dates are AS3 `Date`s. Their `getTime()` is the file time plus the local UTC
//!     offset, which matches UTC date parts passed to the `Date` constructor (it reads
//!     them as local time). f4x builds them the same way;
//!   - F4SE crashes when called without arguments; f4x returns an empty array.
//! - `AllowTextInput` and `SendExternalEvent` return undefined. `SendExternalEvent`
//!   calls Papyrus external-event registrations (`natives.rs`);
//! - `GetMembers(object)`: an array of the member names, in the engine's
//!   `VisitMembers` order (dynamic members, then a display object's children by
//!   instance name); `[]` for arrays and functions; extra arguments are ignored. F4SE
//!   crashes on a string or number; f4x returns `[]` for anything that is not an object;
//! - `CallFunctionNoWait`, `CallGlobalFunctionNoWait`: [`papyrus_calls`];
//! - `MountImage`, `UnmountImage`: [`images`];
//! - the tint hook (`onApplyColorChange`): [`tint`].
//!
//! Differential (the same probe and scripts under f4x, sessions 1795441244..1795442372):
//! member set, build order, `GetMembers`, `onF4SEObjCreated` calls, the tint hook and
//! `img://` loading match F4SE; the remaining differences are listed in [`images`]
//! (missing files) and [`papyrus_calls`] (an empty `Var` reads as `[]`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_engine::scaleform::{Call, Function, Movie, Value, kind};
use f4x_engine_sys::addresses::SCALEFORM_MANAGER_LOAD_MOVIE;

use super::interfaces::SCALEFORM_CALLBACKS;

mod images;
mod papyrus_calls;
mod tint;

/// Offset of the call that F4SE's injection follows (black-box return address - 5): the
/// movie view's `AddRef` right after `MovieDef::CreateInstance` (`call [rax+0xC0]`).
/// NG 1.11.240: the same call at +0x226 (our disassembly: 0x1b1b3f6 in 0x1b1b1d0, to the
/// AddRef 0x1b19130 that matches 1.10.163's 0x210ebf0).
const LOAD_MOVIE_CALL_OFFSET: usize = f4x_engine::pick!(0x188, 0x226);

static AFTER_CREATE: AtomicUsize = AtomicUsize::new(0);
static INJECTED: AtomicUsize = AtomicUsize::new(0);

type Worker = unsafe extern "C" fn(usize, usize, usize, usize) -> usize;
/// F4SE's `ScaleformCallback`: `bool(GFxMovieView*, GFxValue*)`.
type PluginCallback = unsafe extern "C" fn(*mut c_void, *mut Value) -> bool;

pub(super) fn install() {
    tint::install();
    let site = f4x_engine::address::resolve(&SCALEFORM_MANAGER_LOAD_MOVIE).map(|f| f + LOAD_MOVIE_CALL_OFFSET);
    // SAFETY: inside the resolved function's code.
    let Some(site) = site.filter(|&s| unsafe { *(s as *const u8) } == 0xE8) else {
        log::warn!(target: "f4x::scaleform", "LoadMovie call site not found; root.f4se disabled");
        return;
    };
    // SAFETY: an `E8 rel32` call in the game's code.
    let callee = unsafe { (site + 5).wrapping_add_signed((site as *const u8).add(1).cast::<i32>().read_unaligned() as isize) };
    AFTER_CREATE.store(callee, Relaxed);
    let sites = super::lifecycle::redirect_calls_in(site, 5, callee, after_create_hook as Worker as usize);
    log::info!(target: "f4x::scaleform", "LoadMovie hooked ({sites} site)");
}

unsafe extern "C" fn after_create_hook(movie: usize, b: usize, c: usize, d: usize) -> usize {
    // SAFETY: the engine's own function with its own arguments.
    let result = unsafe { std::mem::transmute::<usize, Worker>(AFTER_CREATE.load(Relaxed))(movie, b, c, d) };
    let _ = std::panic::catch_unwind(|| inject(Movie(movie as *mut c_void)));
    result
}

/// `root.f4se`'s functions, in the order F4SE adds them.
fn functions() -> &'static [(&'static str, &'static Function)] {
    static F: std::sync::OnceLock<Vec<(&'static str, &'static Function)>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        vec![
            ("GetMembers", Function::leak(get_members)),
            ("AllowTextInput", Function::leak(allow_text_input)),
            ("SendExternalEvent", Function::leak(send_external_event)),
            ("CallFunctionNoWait", Function::leak(papyrus_calls::call_function_no_wait)),
            ("CallGlobalFunctionNoWait", Function::leak(papyrus_calls::call_global_function_no_wait)),
            ("GetDirectoryListing", Function::leak(get_directory_listing)),
            ("MountImage", Function::leak(images::mount_image)),
            ("UnmountImage", Function::leak(images::unmount_image)),
        ]
    })
}

fn inject(movie: Movie) {
    let Some(mut root) = movie.get_variable("root").filter(Value::is_object) else { return };
    let Some(mut f4se) = movie.create_object() else { return };
    let mut set_ok = Vec::new();
    for (name, f) in functions() {
        match movie.create_function(f, 0) {
            Some(function) => set_ok.push(f4se.set_member(name, &function)),
            None => log::warn!(target: "f4x::scaleform", "CreateFunction failed for {name}"),
        }
    }
    let Some(mut version) = movie.create_object() else { return };
    for field in ["major", "minor", "beta", "releaseIdx"] {
        set_ok.push(version.set_member(field, &Value::int(0)));
    }
    f4se.set_member("version", &version);
    let Some(mut plugins) = movie.create_object() else { return };
    let callbacks: Vec<(String, usize)> = SCALEFORM_CALLBACKS.lock().map(|c| c.clone()).unwrap_or_default();
    for (name, callback) in &callbacks {
        let Some(mut object) = movie.create_object() else { continue };
        // SAFETY: the plugin's `bool(GFxMovieView*, GFxValue*)` callback.
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            std::mem::transmute::<usize, PluginCallback>(*callback)(movie.0, &mut object)
        }));
        if ok.is_err() {
            log::error!(target: "f4x::scaleform", "{name}: Scaleform callback panicked");
        }
        plugins.set_member(name, &object);
    }
    f4se.set_member("plugins", &plugins);
    root.set_member("f4se", &f4se);
    let created = notify_obj_created(&root, &f4se);
    let n = INJECTED.fetch_add(1, Relaxed);
    if n < 64 {
        log::debug!(target: "f4x::scaleform", "root.f4se injected ({} plugin object(s), {created} onF4SEObjCreated call(s)); set_member results {set_ok:?}", callbacks.len());
    }
}

const OBJ_CREATED: &str = "onF4SEObjCreated";

/// Call `onF4SEObjCreated(f4se)` on the root and its first-level display children that
/// have it as a method. Returns the number of calls.
fn notify_obj_created(root: &Value, f4se: &Value) -> usize {
    let call = |object: &Value| -> bool {
        let is_method = object.has_member(OBJ_CREATED) && object.get_member(OBJ_CREATED).is_some_and(|f| f.kind() == kind::CLOSURE);
        is_method && {
            let _ = object.invoke(OBJ_CREATED, &[f4se.clone_ref()]);
            true
        }
    };
    let mut calls = usize::from(call(root));
    let count = root.get_member("numChildren").and_then(|n| n.as_number()).unwrap_or(0.0).max(0.0) as usize;
    for i in 0..count.min(4096) {
        if let Some(child) = root.invoke("getChildAt", &[Value::int(i as i32)]).filter(|c| c.kind() == kind::DISPLAY_OBJECT) {
            calls += usize::from(call(&child));
        }
    }
    calls
}

/// `GetMembers(object)`: the member names in the engine's `VisitMembers` order.
fn get_members(call: &mut Call) {
    let Some(mut array) = call.movie.create_array() else { return };
    if let Some(object) = call.args.first().filter(|v| matches!(v.kind(), kind::OBJECT | kind::DISPLAY_OBJECT)) {
        for name in f4x_engine::scaleform_ext::visit_members(object, false) {
            if let Some(text) = call.movie.create_string(&name) {
                array.push_back(&text);
            }
        }
    }
    call.ret(array);
}

// ---- root.f4se functions ----

fn allow_text_input(call: &mut Call) {
    let allow = call.args.first().and_then(Value::as_bool).unwrap_or(false);
    let count = f4x_engine::controls::allow_text_input(allow);
    log::debug!(target: "f4x::scaleform", "AllowTextInput({allow}) -> text entry count {count:?}");
}

fn get_directory_listing(call: &mut Call) {
    let Some(mut array) = call.movie.create_array() else { return };
    if let Some(path) = call.args.first().and_then(Value::as_string) {
        let pattern = call.args.get(1).and_then(Value::as_string).unwrap_or_else(|| "*".into());
        for found in find_files(&format!("{path}\\{pattern}")) {
            let Some(mut entry) = call.movie.create_object() else { continue };
            if let Some(text) = call.movie.create_string(&format!("{path}\\{}", found.name)) {
                entry.set_member("name", &text);
                entry.set_member("nativePath", &text);
            }
            entry.set_member("isDirectory", &Value::bool(found.attributes & 0x10 != 0));
            entry.set_member("isHidden", &Value::bool(found.attributes & 0x2 != 0));
            if let Some(date) = date(call.movie, found.modified) {
                entry.set_member("lastModified", &date);
            }
            if let Some(date) = date(call.movie, found.created) {
                entry.set_member("creationDate", &date);
            }
            array.push_back(&entry);
        }
    }
    call.ret(array);
}

struct Found {
    name: String,
    attributes: u32,
    created: windows_sys::Win32::Foundation::FILETIME,
    modified: windows_sys::Win32::Foundation::FILETIME,
}

/// `new Date(y, m - 1, d, h, min, s, ms)` from a FILETIME's UTC parts.
fn date(movie: Movie, time: windows_sys::Win32::Foundation::FILETIME) -> Option<Value> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::FileTimeToSystemTime;
    // SAFETY: plain conversion into a local struct.
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&time, &mut st) } == 0 {
        return None;
    }
    let parts = [st.wYear, st.wMonth.saturating_sub(1), st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds];
    let args: Vec<Value> = parts.iter().map(|&p| Value::number(f64::from(p))).collect();
    movie.create_instance("Date", &args)
}

/// Entries matching a `FindFirstFileW` pattern, in the order it returns them.
fn find_files(pattern: &str) -> Vec<Found> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW};
    let wide: Vec<u16> = pattern.encode_utf16().chain(Some(0)).collect();
    let mut out = Vec::new();
    // SAFETY: plain Win32 directory enumeration with a NUL-terminated pattern.
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = FindFirstFileW(wide.as_ptr(), &mut data);
        if handle == INVALID_HANDLE_VALUE {
            return out;
        }
        loop {
            let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
            out.push(Found {
                name: String::from_utf16_lossy(&data.cFileName[..len]),
                attributes: data.dwFileAttributes,
                created: data.ftCreationTime,
                modified: data.ftLastWriteTime,
            });
            if FindNextFileW(handle, &mut data) == 0 {
                break;
            }
        }
        FindClose(handle);
    }
    out
}

/// `SendExternalEvent(name, args...)`: numbers become Float, strings String, booleans
/// Bool (black-box); anything else is passed as None.
fn send_external_event(call: &mut Call) {
    use f4x_engine::papyrus::Arg;
    use f4x_engine::scaleform::kind;
    let Some(event) = call.args.first().and_then(Value::as_string) else { return };
    let args: Vec<Arg> = call.args[1..]
        .iter()
        .map(|v| match v.kind() {
            kind::BOOL => Arg::Bool(v.as_bool().unwrap_or(false)),
            kind::INT | kind::UINT | kind::NUMBER => Arg::Float(v.as_number().unwrap_or(0.0) as f32),
            kind::STRING => Arg::String(v.as_string().unwrap_or_default()),
            _ => Arg::None,
        })
        .collect();
    super::natives::send_external_event(&event, &args);
}
