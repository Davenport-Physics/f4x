//! `root.f4se` in every Scaleform movie, plus F4SE plugins' Scaleform callbacks.
//!
//! Black-box facts (F4SE 0.6.23, probe + `ui.*` on the user's mod set, 2026-11-21):
//! - plugin callbacks run once per movie, from the call at `BSScaleformManager::LoadMovie`
//!   + 0x188 (return address +0x18d), right after the movie instance is created;
//! - each callback gets (movie, the plugin's object) and runs before `root.f4se` is
//!   attached to the movie's root. The object then becomes `root.f4se.plugins.<name>`;
//! - `root.f4se` members that any vanilla or installed SWF can name: `AllowTextInput`,
//!   `GetDirectoryListing`, `SendExternalEvent` (functions), `plugins`, and `version`
//!   `{major, minor, beta, releaseIdx}`. Those four are ints and read 0 under F4SE
//!   0.6.23, so f4x reports the same;
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
//!   calls Papyrus external-event registrations (`natives.rs`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use f4x_engine::scaleform::{Call, Function, Movie, Value};
use f4x_engine_sys::og_1_10_163_addresses::SCALEFORM_MANAGER_LOAD_MOVIE;

use super::interfaces::SCALEFORM_CALLBACKS;

/// Offset of the call that F4SE's injection follows (black-box return address - 5).
const LOAD_MOVIE_CALL_OFFSET: usize = 0x188;

static AFTER_CREATE: AtomicUsize = AtomicUsize::new(0);
static INJECTED: AtomicUsize = AtomicUsize::new(0);

type Worker = unsafe extern "C" fn(usize, usize, usize, usize) -> usize;
/// F4SE's `ScaleformCallback`: `bool(GFxMovieView*, GFxValue*)`.
type PluginCallback = unsafe extern "C" fn(*mut c_void, *mut Value) -> bool;

pub(super) fn install() {
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

struct Functions {
    allow_text_input: &'static Function,
    get_directory_listing: &'static Function,
    send_external_event: &'static Function,
}

fn functions() -> &'static Functions {
    static F: std::sync::OnceLock<Functions> = std::sync::OnceLock::new();
    F.get_or_init(|| Functions {
        allow_text_input: Function::leak(allow_text_input),
        get_directory_listing: Function::leak(get_directory_listing),
        send_external_event: Function::leak(send_external_event),
    })
}

fn inject(movie: Movie) {
    let Some(mut root) = movie.get_variable("root").filter(Value::is_object) else { return };
    let Some(mut f4se) = movie.create_object() else { return };
    let Some(mut version) = movie.create_object() else { return };
    let mut set_ok = Vec::new();
    for field in ["major", "minor", "beta", "releaseIdx"] {
        set_ok.push(version.set_member(field, &Value::int(0)));
    }
    f4se.set_member("version", &version);
    let fs = functions();
    for (name, f) in [("AllowTextInput", fs.allow_text_input), ("GetDirectoryListing", fs.get_directory_listing), ("SendExternalEvent", fs.send_external_event)] {
        match movie.create_function(f, 0) {
            Some(function) => set_ok.push(f4se.set_member(name, &function)),
            None => log::warn!(target: "f4x::scaleform", "CreateFunction failed for {name}"),
        }
    }
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
    let n = INJECTED.fetch_add(1, Relaxed);
    if n < 64 {
        log::debug!(target: "f4x::scaleform", "root.f4se injected ({} plugin object(s)); set_member results {set_ok:?}", callbacks.len());
    }
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
