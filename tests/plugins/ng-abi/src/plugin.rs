use std::ffi::{CStr, c_char, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use f4x_abi::f4se::{
    self, F4seInterface, F4seInterfaceNg, Message, PluginInfo, TaskDelegate, TaskDelegateVtbl, TaskInterfaceNg,
    interface_id, message,
};
use f4x_abi::version::PackedVersion;
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{FILETIME, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, GetProcessTimes};

/// The F4SE version that added `GetSaveFolderName` (whatsnew 0.7.1).
const SAVE_FOLDER_API: PackedVersion = PackedVersion::new(0, 7, 1, 0);

/// What this plugin declares (the fork's plugin template, `res/commonlibf4-plugin.cpp.in`).
#[cfg(not(feature = "legacy"))]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static F4SEPlugin_Version: f4se::PluginVersionData = {
    use f4x_abi::f4se::{PluginVersionData, address_independence, structure_independence};
    use f4x_abi::version::runtime;
    PluginVersionData::declare(
        "f4x_ng_abi",
        "f4x tests",
        PackedVersion::new(1, 2, 3, 0),
        address_independence::ADDRESS_LIBRARY_1_11_137,
        structure_independence::LAYOUT_1_11_137,
        &[runtime::V1_11_221],
        PackedVersion(0),
    )
};

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);
static INTERFACE: AtomicUsize = AtomicUsize::new(0);
static PRELOAD_HANDLE: AtomicU32 = AtomicU32::new(u32::MAX);
static LOAD_HANDLE: AtomicU32 = AtomicU32::new(u32::MAX);
static PRELOADED: AtomicBool = AtomicBool::new(false);
static PERMANENT_RUNS: AtomicU64 = AtomicU64::new(0);
static FIRST_CALLBACK: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static MESSAGES: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static SAVE_FOLDER: OnceLock<Option<String>> = OnceLock::new();

fn this_module() -> HMODULE {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: lookup by an address inside this DLL; no reference taken.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            this_module as *const u16,
            &mut module,
        )
    };
    module
}

fn module_name(module: HMODULE) -> String {
    let mut buf = [0u16; 260];
    // SAFETY: valid buffer.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned()
}

fn own_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| module_name(this_module()))
}

/// Milliseconds since the process was created.
fn process_ms() -> f64 {
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user, mut now) = (zero, zero, zero, zero, zero);
    // SAFETY: valid out-pointers.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
        GetSystemTimePreciseAsFileTime(&mut now);
    }
    (ft(now).saturating_sub(ft(created)) as f64 / 1000.0).round() / 10.0
}

fn thread() -> u32 {
    // SAFETY: trivially safe.
    unsafe { GetCurrentThreadId() }
}

fn emit(kind: &str, mut fields: Value) {
    let Ok(mut log) = LOG.lock() else { return };
    if log.is_none() {
        let Some(dir) = std::env::var_os("F4X_SESSION_DIR") else { return };
        *log = OpenOptions::new().create(true).append(true).open(std::path::Path::new(&dir).join("ng-abi.jsonl")).ok();
    }
    let Some(file) = log.as_mut() else { return };
    fields["event"] = kind.into();
    fields["module"] = own_name().into();
    fields["seq"] = SEQ.fetch_add(1, Relaxed).into();
    fields["process_ms"] = process_ms().into();
    fields["thread"] = thread().into();
    let _ = writeln!(file, "{fields}");
    let _ = file.flush();
}

/// The call stack: game code (and other non-host modules) as `module+rva`; host DLLs
/// (`f4se_*`, `f4x_runtime`) by name only; this DLL's own frames skipped.
fn stack() -> Value {
    let mut frames = [std::ptr::null_mut::<c_void>(); 32];
    // SAFETY: valid buffer of 32 entries.
    let count = unsafe { RtlCaptureStackBackTrace(0, 32, frames.as_mut_ptr(), std::ptr::null_mut()) } as usize;
    let me = this_module();
    let mut out = Vec::new();
    for &frame in &frames[..count] {
        let mut module: HMODULE = std::ptr::null_mut();
        // SAFETY: lookup by address only.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                frame.cast(),
                &mut module,
            )
        } != 0;
        if !found {
            out.push(Value::String("?".into()));
            continue;
        }
        if module == me {
            continue;
        }
        let name = module_name(module);
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("f4se_") || lower == "f4x_runtime.dll" {
            out.push(Value::String(name));
        } else {
            out.push(Value::String(format!("{name}+{:#x}", frame as usize - module as usize)));
        }
    }
    Value::Array(out)
}

fn loaded_modules() -> Value {
    let names = ["d3d11.dll", "dxgi.dll", "steam_api64.dll", "XINPUT1_3.dll", "x3daudio1_7.dll", "f4x_agent.dll"];
    let present: Vec<&str> = names
        .into_iter()
        .filter(|n| {
            let wide: Vec<u16> = n.encode_utf16().chain(Some(0)).collect();
            // SAFETY: NUL-terminated name; no reference taken.
            !unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null()
        })
        .collect();
    json!(present)
}

fn mapped(names: &[&str]) -> Value {
    let present: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            let wide: Vec<u16> = n.encode_utf16().chain(Some(0)).collect();
            // SAFETY: NUL-terminated name; no reference taken.
            !unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null()
        })
        .collect();
    json!(present)
}

fn c_str(p: *const c_char) -> Value {
    if p.is_null() {
        return Value::Null;
    }
    // SAFETY: the host passes NUL-terminated strings.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into()
}

/// The save folder name, when the host's F4SE version has the API (0.7.1+).
///
/// # Safety
/// `f4se` is the host's table.
unsafe fn save_folder_name(f4se: &F4seInterface) -> Option<String> {
    if f4se.f4se_version < SAVE_FOLDER_API {
        return None;
    }
    // SAFETY: a 0.7.1+ table has GetSaveFolderName after GetPluginInfo.
    let ng = unsafe { &*(f4se as *const F4seInterface).cast::<F4seInterfaceNg>() };
    let get = ng.get_save_folder_name?;
    // SAFETY: the host's function.
    let p = unsafe { get() };
    if p.is_null() {
        return Some(String::new());
    }
    // SAFETY: NUL-terminated.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Everything readable from the table at this moment.
///
/// # Safety
/// `f4se` is the host's table.
unsafe fn describe(f4se: &F4seInterface) -> Value {
    // SAFETY: host function pointers (contract).
    unsafe {
        let mut fields = json!({
            "f4se_version": f4se.f4se_version.to_string(),
            "f4se_version_raw": f4se.f4se_version.0,
            "runtime_version": f4se.runtime_version.to_string(),
            "runtime_version_raw": f4se.runtime_version.0,
            "editor_version_raw": f4se.editor_version.0,
            "is_editor": f4se.is_editor,
            "plugin_handle": f4se.get_plugin_handle.map(|f| f()),
            "release_index": f4se.get_release_index.map(|f| f()),
            "has_get_plugin_info": f4se.get_plugin_info.is_some(),
            "save_folder_name": save_folder_name(f4se),
            "stack": stack(),
            "modules": loaded_modules(),
        });
        let mut interfaces = serde_json::Map::new();
        if let Some(query) = f4se.query_interface {
            for id in 0..=12u32 {
                let p = query(id);
                interfaces.insert(id.to_string(), if p.is_null() { Value::Null } else { p.cast::<u32>().read().into() });
            }
        }
        fields["interfaces"] = Value::Object(interfaces);
        fields["plugin_info"] = plugin_infos(f4se);
        fields
    }
}

/// This copy's declared name (patched copies differ), else the legacy `PluginInfo` name.
/// Read through the export, as the fork's `PluginVersionData::GetSingleton` does: the
/// compiler would fold a direct read of the static to the unpatched value.
fn declared_name() -> String {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    // SAFETY: our own module and export.
    let data = unsafe { GetProcAddress(this_module(), f4se::EXPORT_VERSION.as_ptr().cast()) };
    match data {
        // SAFETY: the export is a PluginVersionData.
        Some(p) => String::from_utf8_lossy(unsafe { &*(p as *const f4se::PluginVersionData) }.name_bytes()).into_owned(),
        None => "f4x_ng_abi".to_owned(),
    }
}

/// `GetPluginInfo` for this copy's own name, the agent, the host and a missing name.
///
/// # Safety
/// `f4se` is the host's table.
unsafe fn plugin_infos(f4se: &F4seInterface) -> Value {
    let Some(get_info) = f4se.get_plugin_info else { return Value::Null };
    let own = declared_name();
    let file = own_name();
    let stem = file.trim_end_matches(".dll").trim_end_matches(".DLL");
    let mut infos = serde_json::Map::new();
    let names = [
        ("(own)", own.as_str()),
        ("(own file)", file),
        ("(own file stem)", stem),
        ("F4SE", "F4SE"),
        ("f4x_agent", "f4x_agent"),
        ("f4x_ng_abi_a_listed_exact", "f4x_ng_abi_a_listed_exact"),
        ("f4x_ng_abi_h3_independent", "f4x_ng_abi_h3_independent"),
        ("F4X_AGENT", "F4X_AGENT"),
        ("nonexistent_plugin", "nonexistent_plugin"),
    ];
    for (key, name) in names {
        let Ok(c) = std::ffi::CString::new(name) else { continue };
        // SAFETY: the host's function; NUL-terminated name.
        let info = unsafe { get_info(c.as_ptr()).cast::<PluginInfo>().as_ref() };
        let described = match info {
            None => Value::Null,
            Some(info) => json!({"info_version": info.info_version, "name": c_str(info.name), "version": info.version}),
        };
        infos.insert(key.to_owned(), described);
    }
    Value::Object(infos)
}

/// Allocate from the trampoline pools and say how far from the game / this DLL.
///
/// # Safety
/// `f4se` is the host's table; called from Preload or Load.
unsafe fn try_trampoline(f4se: &F4seInterface, handle: u32) -> Value {
    // SAFETY: host functions (contract).
    unsafe {
        let Some(query) = f4se.query_interface else { return Value::Null };
        let Some(t) = query(interface_id::TRAMPOLINE).cast::<f4se::TrampolineInterface>().as_ref() else {
            return Value::Null;
        };
        let game = GetModuleHandleW(std::ptr::null()) as i64;
        let me = this_module() as i64;
        let branch = t.allocate_from_branch_pool.map_or(std::ptr::null_mut(), |f| f(handle, 64));
        let local = t.allocate_from_local_pool.map_or(std::ptr::null_mut(), |f| f(handle, 64));
        let near = |p: *mut c_void, base: i64| !p.is_null() && (p as i64 - base).abs() < (1i64 << 31);
        json!({
            "version": t.interface_version,
            "branch_non_null": !branch.is_null(), "branch_near_game": near(branch, game),
            "local_non_null": !local.is_null(), "local_near_module": near(local, me),
        })
    }
}

// ---- Entry points ----

/// # Safety
/// Called by the host with its table.
#[cfg(not(feature = "legacy"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn F4SEPlugin_Preload(f4se: *const F4seInterface) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: host pointer (contract).
        let f4se = unsafe { &*f4se };
        PRELOADED.store(true, Relaxed);
        // SAFETY: as above.
        let mut fields = unsafe { describe(f4se) };
        let handle = fields["plugin_handle"].as_u64().map_or(u32::MAX, |h| h as u32);
        PRELOAD_HANDLE.store(handle, Relaxed);
        // SAFETY: as above.
        fields["trampoline"] = unsafe { try_trampoline(f4se, handle) };
        // SAFETY: as above.
        let _ = SAVE_FOLDER.set(unsafe { save_folder_name(f4se) });
        // A copy named `*preload_false*` declines in Preload (tools/ng_abi.py variants).
        let accept = !own_name().to_ascii_lowercase().contains("preload_false");
        fields["returning"] = accept.into();
        emit("preload", fields);
        accept
    })
    .unwrap_or(false)
}

/// # Safety
/// Called by the host with valid pointers.
#[cfg(any(feature = "query", feature = "legacy"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn F4SEPlugin_Query(f4se: *const F4seInterface, info: *mut PluginInfo) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: host pointers (contract).
        unsafe {
            (*info).info_version = PluginInfo::VERSION;
            (*info).name = c"f4x_ng_abi".as_ptr();
            (*info).version = 7;
            emit("query", describe(&*f4se));
        }
        true
    })
    .unwrap_or(false)
}

/// # Safety
/// Called by the host with its table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn F4SEPlugin_Load(f4se: *const F4seInterface) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: host pointer (contract); the table lives for the process.
        unsafe { load(&*f4se) }
    })
    .unwrap_or(false)
}

unsafe fn load(f4se: &'static F4seInterface) -> bool {
    INTERFACE.store(f4se as *const _ as usize, Relaxed);
    // SAFETY: host functions (contract) for everything below.
    unsafe {
        let mut fields = describe(f4se);
        let handle = fields["plugin_handle"].as_u64().map_or(u32::MAX, |h| h as u32);
        LOAD_HANDLE.store(handle, Relaxed);
        fields["preloaded"] = PRELOADED.load(Relaxed).into();
        // A copy named `*load_false*` (not `preload_false`) declines in Load, before it
        // registers anything (a host may unload it).
        let lower = own_name().to_ascii_lowercase();
        if lower.contains("load_false") && !lower.contains("preload_false") {
            fields["returning"] = false.into();
            emit("load", fields);
            return false;
        }
        fields["trampoline"] = try_trampoline(f4se, handle);
        let query = f4se.query_interface.expect("QueryInterface");
        if let Some(m) = query(interface_id::MESSAGING).cast::<f4se::MessagingInterface>().as_ref() {
            let register = m.register_listener.expect("RegisterListener");
            fields["listen_host"] = register(handle, message::HOST_SENDER.as_ptr(), on_message as f4se::MessageCallback as *mut c_void).into();
            fields["listen_any"] = register(handle, std::ptr::null(), on_any_message as f4se::MessageCallback as *mut c_void).into();
        }
        if let Some(p) = query(interface_id::PAPYRUS).cast::<f4se::PapyrusInterface>().as_ref() {
            fields["papyrus_register"] = p.register.expect("Register")(on_papyrus as f4se::PapyrusRegisterCallback as *mut c_void).into();
        }
        if let Some(s) = query(interface_id::SCALEFORM).cast::<f4se::ScaleformInterface>().as_ref() {
            fields["scaleform_register"] =
                s.register.expect("Register")(c"f4xngabi".as_ptr(), on_scaleform as f4se::ScaleformCallback as *mut c_void).into();
        }
        if let Some(s) = query(interface_id::SERIALIZATION).cast::<f4se::SerializationInterface>().as_ref() {
            s.set_unique_id.expect("SetUniqueID")(handle, u32::from_be_bytes(*b"F4XN"));
            s.set_revert_callback.expect("SetRevertCallback")(handle, on_revert as f4se::SerializationCallback as *mut c_void);
            fields["serialization"] = true.into();
        }
        fields["returning"] = true.into();
        emit("load", fields);
        true
    }
}

// ---- Callbacks ----

fn first(label: &'static str) -> bool {
    FIRST_CALLBACK.lock().map(|mut seen| if seen.contains(&label) { false } else { seen.push(label); true }).unwrap_or(false)
}

unsafe extern "C" fn on_message(msg: *mut Message) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the host passes a valid message.
        let msg = unsafe { &*msg };
        if let Ok(mut seen) = MESSAGES.lock() {
            seen.push(msg.kind);
        }
        emit(
            "message",
            json!({
                "kind": msg.kind, "name": message::name(msg.kind), "sender": c_str(msg.sender),
                "data_len": msg.data_len, "data_inline": if msg.data_len <= 1 { json!(msg.data as usize) } else { Value::Null },
                "stack": stack(),
            }),
        );
        if msg.kind == message::POST_LOAD {
            queue_tasks();
            let f4se = INTERFACE.load(Relaxed) as *const F4seInterface;
            // SAFETY: stored in Load; the host's table.
            emit("plugin_info_postload", json!({"plugin_info": unsafe { plugin_infos(&*f4se) }}));
        }
        if msg.kind == message::GAME_LOADED {
            checks();
        }
    });
}

unsafe extern "C" fn on_any_message(msg: *mut Message) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: as above.
        let msg = unsafe { &*msg };
        emit("message_any", json!({"kind": msg.kind, "sender": c_str(msg.sender), "data_len": msg.data_len}));
    });
}

unsafe extern "C" fn on_papyrus(vm: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| emit("papyrus_callback", json!({"vm_null": vm.is_null(), "stack": stack()})));
    true
}

unsafe extern "C" fn on_scaleform(movie: *mut c_void, value: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| {
        if first("scaleform") {
            emit("scaleform_callback", json!({"movie_null": movie.is_null(), "value_null": value.is_null()}));
        }
    });
    true
}

unsafe extern "C" fn on_revert(_: *const f4se::SerializationInterface) {
    let _ = std::panic::catch_unwind(|| emit("serialization_revert", json!({})));
}

// ---- Tasks: when and where they run, and how they are freed ----

#[repr(C)]
struct Task {
    base: TaskDelegate,
    label: &'static str,
}

unsafe extern "C" fn task_destroy(this: *mut TaskDelegate, flags: u32) -> *mut c_void {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: `this` is one of our boxed tasks.
        let label = unsafe { (*this.cast::<Task>()).label };
        emit("task_destroy", json!({"label": label, "flags": flags, "runs": PERMANENT_RUNS.load(Relaxed)}));
    });
    if flags & 1 != 0 {
        // SAFETY: allocated by Box::into_raw in `queue_tasks`.
        drop(unsafe { Box::from_raw(this.cast::<Task>()) });
    }
    this.cast()
}

unsafe extern "C" fn task_run(this: *mut TaskDelegate) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: as above.
        let label = unsafe { (*this.cast::<Task>()).label };
        if label == "permanent" {
            let n = PERMANENT_RUNS.fetch_add(1, Relaxed) + 1;
            if n <= 3 || n.is_power_of_two() {
                emit("task_run", json!({"label": label, "run": n}));
            }
        } else {
            emit("task_run", json!({"label": label, "stack": stack()}));
        }
    });
}

static TASK_VTBL: TaskDelegateVtbl = TaskDelegateVtbl { destroy: task_destroy, run: task_run };

fn queue_tasks() {
    let f4se = INTERFACE.load(Relaxed) as *const F4seInterface;
    // SAFETY: stored in Load; host tables live for the process.
    unsafe {
        let Some(query) = (*f4se).query_interface else { return };
        let tasks = query(interface_id::TASK).cast::<TaskInterfaceNg>();
        let Some(tasks) = tasks.as_ref() else { return };
        // AddTaskPermanent exists in the next-gen fork's table (0.7.x hosts).
        let has_permanent = (*f4se).f4se_version >= PackedVersion::new(0, 7, 0, 0);
        let mut queued = Vec::new();
        for (label, add) in [
            ("task", tasks.add_task),
            ("ui_task", tasks.add_ui_task),
            ("permanent", if has_permanent { tasks.add_task_permanent } else { None }),
        ] {
            let Some(add) = add else { continue };
            let task = Box::into_raw(Box::new(Task { base: TaskDelegate { vtable: &TASK_VTBL }, label }));
            add(task.cast());
            queued.push(label);
        }
        emit("tasks_queued", json!({"version": tasks.interface_version, "queued": queued}));
    }
}

/// What the plugin can verify by itself (the host-specific values are compared by
/// `tools/ng_abi.py`).
fn checks() {
    let preload = PRELOAD_HANDLE.load(Relaxed);
    let load = LOAD_HANDLE.load(Relaxed);
    let messages = MESSAGES.lock().map(|m| m.clone()).unwrap_or_default();
    emit(
        "checks",
        json!({
            "preloaded_before_load": PRELOADED.load(Relaxed),
            "same_handle": preload == load,
            "preload_handle": preload, "load_handle": load,
            "save_folder_name": SAVE_FOLDER.get().cloned().flatten(),
            "messages": messages,
            "permanent_runs": PERMANENT_RUNS.load(Relaxed),
            // Which declined copies are still mapped (did the host unload them?).
            "still_mapped": mapped(&[
                "f4x_ng_abi_c_unlisted_noflags.dll", "f4x_ng_abi_t_preload_false.dll",
                "f4x_ng_abi_u_load_false.dll", "f4x_ng_abi_y_legacy.dll", "f4x_ng_abi_z1_no_load.dll",
            ]),
            // SAFETY: stored in Load; the host's table.
            "plugin_info": unsafe { plugin_infos(&*(INTERFACE.load(Relaxed) as *const F4seInterface)) },
        }),
    );
}
