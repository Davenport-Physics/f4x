use std::ffi::{CStr, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use f4x_abi::f4se::{self, F4seInterface, Message, PluginInfo, TaskDelegate, TaskDelegateVtbl, interface_id, message};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, TerminateProcess};

static STARTED: OnceLock<Instant> = OnceLock::new();
static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);
static PUMPS: AtomicU64 = AtomicU64::new(0);
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);
static REAL_PEEK: AtomicUsize = AtomicUsize::new(0);
static INTERFACE: AtomicUsize = AtomicUsize::new(0);
static HANDLE: AtomicU32 = AtomicU32::new(u32::MAX);
static SCALEFORM_CALLS: AtomicU32 = AtomicU32::new(0);

const NAME: &CStr = c"f4x_probe";

fn now_ms() -> f64 {
    let started = STARTED.get_or_init(Instant::now);
    (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

fn thread() -> u32 {
    // SAFETY: trivially safe.
    unsafe { GetCurrentThreadId() }
}

/// Append one event to probe.jsonl.
fn emit(kind: &str, mut fields: Value) {
    let Ok(mut log) = LOG.lock() else { return };
    if log.is_none() {
        let Some(dir) = std::env::var_os("F4X_SESSION_DIR") else { return };
        *log = OpenOptions::new().create(true).append(true).open(std::path::Path::new(&dir).join("probe.jsonl")).ok();
    }
    let Some(file) = log.as_mut() else { return };
    fields["event"] = kind.into();
    fields["seq"] = SEQ.fetch_add(1, Relaxed).into();
    fields["t_ms"] = now_ms().into();
    fields["thread"] = thread().into();
    fields["main_thread"] = (MAIN_THREAD.load(Relaxed) == thread()).into();
    fields["pumps"] = PUMPS.load(Relaxed).into();
    let _ = writeln!(file, "{fields}");
    let _ = file.flush();
}

/// The call stack as `module+rva`, skipping this probe's own frames. Addresses inside
/// the host DLL (`f4se_*`) are recorded by module name only.
fn stack() -> Value {
    let mut frames = [std::ptr::null_mut::<c_void>(); 32];
    // SAFETY: valid buffer of 32 entries.
    let count = unsafe { RtlCaptureStackBackTrace(0, 32, frames.as_mut_ptr(), std::ptr::null_mut()) } as usize;
    let mut out = Vec::new();
    for &frame in &frames[..count] {
        let mut module: HMODULE = std::ptr::null_mut();
        // SAFETY: lookup by address only; no reference taken.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                frame.cast(),
                &mut module,
            )
        } != 0;
        if !found {
            out.push(Value::String(format!("?+{:#x}", frame as usize)));
            continue;
        }
        let name = module_name(module);
        let lower = name.to_ascii_lowercase();
        if lower == "f4x_probe.dll" {
            continue;
        }
        if lower.starts_with("f4se_") {
            out.push(Value::String(name));
        } else {
            out.push(Value::String(format!("{name}+{:#x}", frame as usize - module as usize)));
        }
    }
    Value::Array(out)
}

fn module_name(module: HMODULE) -> String {
    let mut buf = [0u16; 260];
    // SAFETY: valid buffer.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned()
}

fn describe_interface(f4se: &F4seInterface) -> Value {
    json!({
        "f4se_version": format!("{}", f4se.f4se_version),
        "f4se_version_raw": f4se.f4se_version.0,
        "runtime_version": format!("{}", f4se.runtime_version),
        "runtime_version_raw": f4se.runtime_version.0,
        "editor_version_raw": f4se.editor_version.0,
        "is_editor": f4se.is_editor,
    })
}

// ---- PeekMessageA pump counter (same technique as the f4x runtime spike) ----

type PeekMessageA = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> i32;

unsafe extern "system" fn peek_hook(msg: *mut c_void, wnd: *mut c_void, min: u32, max: u32, remove: u32) -> i32 {
    if PUMPS.fetch_add(1, Relaxed) == 0 {
        MAIN_THREAD.store(thread(), Relaxed);
        emit("first_pump", json!({}));
    }
    // SAFETY: REAL_PEEK was set before the hook went live.
    unsafe { std::mem::transmute::<usize, PeekMessageA>(REAL_PEEK.load(Relaxed))(msg, wnd, min, max, remove) }
}

/// # Safety
/// Must run while the game image is mapped (always true for a loaded plugin).
unsafe fn hook_peek_message() -> bool {
    // SAFETY: walking the exe's own import table.
    unsafe {
        let base = GetModuleHandleW(std::ptr::null()).cast::<u8>();
        let nt = base.add(base.add(0x3C).cast::<i32>().read_unaligned() as usize);
        let imports = nt.add(24 + 112 + 8).cast::<u32>().read_unaligned() as usize;
        let mut descriptor = base.add(imports);
        loop {
            let name_rva = descriptor.add(12).cast::<u32>().read_unaligned() as usize;
            if name_rva == 0 {
                return false;
            }
            if CStr::from_ptr(base.add(name_rva).cast()).to_bytes().eq_ignore_ascii_case(b"USER32.dll") {
                let lookup = descriptor.cast::<u32>().read_unaligned() as usize;
                let iat = descriptor.add(16).cast::<u32>().read_unaligned() as usize;
                for i in 0.. {
                    let entry = base.add(lookup).cast::<u64>().add(i).read_unaligned();
                    if entry == 0 {
                        return false;
                    }
                    if entry >> 63 == 0 && CStr::from_ptr(base.add(entry as usize + 2).cast()).to_bytes() == b"PeekMessageA" {
                        let slot = base.add(iat).cast::<usize>().add(i);
                        let mut old: PAGE_PROTECTION_FLAGS = 0;
                        VirtualProtect(slot.cast(), 8, PAGE_READWRITE, &mut old);
                        REAL_PEEK.store(slot.read(), Relaxed);
                        slot.write(peek_hook as PeekMessageA as usize);
                        VirtualProtect(slot.cast(), 8, old, &mut old);
                        return true;
                    }
                }
            }
            descriptor = descriptor.add(20);
        }
    }
}

// ---- Exports ----

/// Next-gen F4SE (0.7.x) loads plugins by this declaration instead of calling
/// `F4SEPlugin_Query` (built with `ng`; same declaration as the f4x agent plugin).
#[cfg(feature = "ng")]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static F4SEPlugin_Version: f4x_abi::f4se::PluginVersionData = {
    use f4x_abi::f4se::{PluginVersionData, address_independence, structure_independence};
    use f4x_abi::version::{PackedVersion, runtime};
    PluginVersionData::declare(
        "f4x_probe",
        "f4x",
        PackedVersion::new(0, 0, 1, 0),
        address_independence::SIGNATURES,
        structure_independence::LAYOUT_1_11_137,
        &[runtime::V1_11_240],
        PackedVersion(0),
    )
};

/// # Safety
/// Called by the host with a valid interface and info struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn F4SEPlugin_Query(f4se: *const F4seInterface, info: *mut PluginInfo) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: host-provided pointers (contract).
        unsafe {
            (*info).info_version = PluginInfo::VERSION;
            (*info).name = NAME.as_ptr();
            (*info).version = 1;
            let f4se = &*f4se;
            let handle = f4se.get_plugin_handle.map(|f| f());
            let mut fields = describe_interface(f4se);
            fields["plugin_handle"] = json!(handle);
            fields["release_index"] = json!(f4se.get_release_index.map(|f| f()));
            fields["stack"] = stack();
            emit("query", fields);
        }
        true
    })
    .unwrap_or(false)
}

/// # Safety
/// Called by the host with a valid interface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn F4SEPlugin_Load(f4se: *const F4seInterface) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: host-provided pointer (contract).
        unsafe { load(&*f4se) }
    })
    .unwrap_or(false)
}

unsafe fn load(f4se: &'static F4seInterface) -> bool {
    INTERFACE.store(f4se as *const _ as usize, Relaxed);
    let _ = std::thread::Builder::new().name("f4x-probe-heartbeat".into()).spawn(|| {
        let mut last = u64::MAX;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let pumps = PUMPS.load(Relaxed);
            if pumps != last {
                emit("heartbeat", json!({}));
                last = pumps;
            } else {
                emit("heartbeat_stalled", json!({}));
            }
        }
    });
    // SAFETY: called on the loading thread with the game image mapped.
    let pump_hook = unsafe { hook_peek_message() };
    // SAFETY: the host's function pointers are valid for the process lifetime.
    unsafe {
        let handle = f4se.get_plugin_handle.map(|f| f()).unwrap_or(u32::MAX);
        HANDLE.store(handle, Relaxed);
        let mut fields = describe_interface(f4se);
        fields["plugin_handle"] = handle.into();
        fields["release_index"] = json!(f4se.get_release_index.map(|f| f()));
        fields["pump_hook"] = pump_hook.into();
        fields["stack"] = stack();
        // Which interface ids exist, and the version each reports.
        let mut interfaces = serde_json::Map::new();
        if let Some(query) = f4se.query_interface {
            for id in 0..=12u32 {
                let p = query(id);
                interfaces.insert(id.to_string(), if p.is_null() { Value::Null } else { p.cast::<u32>().read().into() });
            }
        }
        fields["interfaces"] = Value::Object(interfaces);
        if let Some(get_info) = f4se.get_plugin_info {
            for name in [NAME, c"F4SE", c"nonexistent_plugin"] {
                let info = get_info(name.as_ptr()).cast::<PluginInfo>();
                let described = if info.is_null() {
                    Value::Null
                } else {
                    json!({
                        "info_version": (*info).info_version,
                        "name": if (*info).name.is_null() { Value::Null } else { CStr::from_ptr((*info).name).to_string_lossy().into() },
                        "version": (*info).version,
                    })
                };
                fields[format!("plugin_info:{}", name.to_string_lossy())] = described;
            }
        }
        emit("load", fields);

        let query = f4se.query_interface.expect("QueryInterface");
        let messaging = query(interface_id::MESSAGING).cast::<f4se::MessagingInterface>();
        if let Some(messaging) = messaging.as_ref() {
            let register = messaging.register_listener.expect("RegisterListener");
            let to_host = register(handle, message::HOST_SENDER.as_ptr(), on_message as f4se::MessageCallback as *mut c_void);
            let to_all = register(handle, std::ptr::null(), on_any_message as f4se::MessageCallback as *mut c_void);
            emit("register_listener", json!({"host": to_host, "null_sender": to_all}));
        }
        let papyrus = query(interface_id::PAPYRUS).cast::<f4se::PapyrusInterface>();
        if let Some(papyrus) = papyrus.as_ref() {
            let ok = papyrus.register.expect("Register")(on_papyrus as f4se::PapyrusRegisterCallback as *mut c_void);
            emit("papyrus_register", json!({"ok": ok}));
        }
        let scaleform = query(interface_id::SCALEFORM).cast::<f4se::ScaleformInterface>();
        if let Some(scaleform) = scaleform.as_ref() {
            let ok = scaleform.register.expect("Register")(c"f4xprobe".as_ptr(), on_scaleform as f4se::ScaleformCallback as *mut c_void);
            emit("scaleform_register", json!({"ok": ok}));
        }
        let serialization = query(interface_id::SERIALIZATION).cast::<f4se::SerializationInterface>();
        if let Some(s) = serialization.as_ref() {
            s.set_unique_id.expect("SetUniqueID")(handle, u32::from_be_bytes(*b"F4XP"));
            s.set_save_callback.expect("SetSaveCallback")(handle, on_save as f4se::SerializationCallback as *mut c_void);
            s.set_load_callback.expect("SetLoadCallback")(handle, on_load as f4se::SerializationCallback as *mut c_void);
            s.set_revert_callback.expect("SetRevertCallback")(handle, on_revert as f4se::SerializationCallback as *mut c_void);
            emit("serialization_register", json!({}));
        }
    }
    true
}

// ---- Callbacks ----

unsafe extern "C" fn on_message(msg: *mut Message) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the host passes a valid message.
        let msg = unsafe { &*msg };
        let sender = if msg.sender.is_null() {
            Value::Null
        } else {
            // SAFETY: NUL-terminated sender name.
            unsafe { CStr::from_ptr(msg.sender) }.to_string_lossy().into()
        };
        let mut payload = describe_payload(msg);
        // NewGame passes the starting quest: record its state at this moment (CommonLibF4
        // TESQuest: formID +0x14, data.flags +0xF4, currentStage +0x2B4, alreadyRun +0x2B6).
        if msg.kind == message::NEW_GAME && readable(msg.data as *const c_void, 0x2B8) {
            let q = msg.data as *const u8;
            // SAFETY: readable for the whole quest prefix.
            unsafe {
                payload["quest"] = json!({
                    "form_id": format!("{:08X}", (q.add(0x14) as *const u32).read_unaligned()),
                    "flags": format!("{:#06x}", (q.add(0xF4) as *const u16).read_unaligned()),
                    "current_stage": (q.add(0x2B4) as *const u16).read_unaligned(),
                    "already_run": *q.add(0x2B6),
                });
            }
        }
        emit(
            "message",
            json!({
                "kind": msg.kind, "name": message::name(msg.kind), "sender": sender,
                "data_len": msg.data_len, "data": payload, "stack": stack(),
            }),
        );
        if msg.kind == message::POST_LOAD {
            queue_tasks();
        }
        if msg.kind == message::GAME_LOADED
            && let Some(after) = std::env::var("F4X_PROBE_EXIT_AFTER_S").ok().and_then(|s| s.parse::<u64>().ok())
        {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(after));
                emit("exit", json!({"reason": "F4X_PROBE_EXIT_AFTER_S"}));
                // SAFETY: terminating our own process on request.
                unsafe { TerminateProcess(GetCurrentProcess(), 0) };
            });
        }
    });
}

unsafe extern "C" fn on_any_message(msg: *mut Message) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: as above.
        let msg = unsafe { &*msg };
        emit("message_any", json!({"kind": msg.kind, "data_len": msg.data_len}));
    });
}

fn describe_payload(msg: &Message) -> Value {
    let raw = msg.data as usize;
    let mut out = json!({"pointer": format!("{raw:#x}")});
    if msg.data_len == 0 || msg.data.is_null() {
        return out;
    }
    // Hosts may pass small values *in* the pointer (e.g. a bool with data_len 1), so
    // only read through it when the memory is actually readable.
    if !readable(msg.data, msg.data_len.min(1024) as usize) {
        out["inline_value"] = raw.into();
        return out;
    }
    // SAFETY: checked readable above.
    let bytes = unsafe { std::slice::from_raw_parts(msg.data.cast::<u8>(), msg.data_len.min(1024) as usize) };
    let text: String = String::from_utf8_lossy(bytes.split(|&b| b == 0).next().unwrap_or_default()).into_owned();
    out["as_text"] = text.into();
    out["first_bytes"] = format!("{:02x?}", &bytes[..bytes.len().min(16)]).into();
    out
}

/// Whether `len` bytes at `p` are committed, readable memory.
fn readable(p: *const c_void, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
    };
    if (p as usize) < 0x10000 {
        return false;
    }
    // SAFETY: VirtualQuery only inspects the address space.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let got = unsafe { VirtualQuery(p, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    if got == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
        return false;
    }
    (p as usize) + len <= info.BaseAddress as usize + info.RegionSize
}

unsafe extern "C" fn on_papyrus(vm: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| emit("papyrus_callback", json!({"vm_null": vm.is_null(), "stack": stack()})));
    true
}

unsafe extern "C" fn on_scaleform(movie: *mut c_void, value: *mut c_void) -> bool {
    let _ = std::panic::catch_unwind(|| {
        let n = SCALEFORM_CALLS.fetch_add(1, Relaxed);
        if n < 40 {
            // What the plugin gets: the value's type and where it sits. Mark it so the
            // agent can find it (`root.f4se.plugins.<name>.f4xProbeMark`).
            use f4x_engine::scaleform::{Movie, Value as GfxValue};
            let mut detail = json!({});
            if !value.is_null() && !movie.is_null() {
                // SAFETY: F4SE passes a live GFx::Value and movie.
                let v = unsafe { &mut *(value as *mut GfxValue) };
                let marked = v.set_member("f4xProbeMark", &GfxValue::number(f64::from(n)));
                let m = Movie(movie);
                let root = m.get_variable("root");
                let f4se = root.as_ref().and_then(|r| r.get_member("f4se"));
                let plugins = f4se.as_ref().and_then(|f| f.get_member("plugins"));
                let seen: Vec<&str> = ["f4xprobe", "f4x_probe", "f4mcm", "SettleAnywhere"]
                    .into_iter()
                    .filter(|name| plugins.as_ref().is_some_and(|p| p.has_member(name)))
                    .collect();
                detail = json!({
                    "value_type": v.type_name(),
                    "marked": marked,
                    "root_type": root.as_ref().map(|r| r.type_name()),
                    "root_has_f4se": f4se.is_some(),
                    "f4se_members": f4se.as_ref().map(|f| ["AllowTextInput", "GetDirectoryListing", "SendExternalEvent", "plugins", "version"].into_iter().filter(|k| f.has_member(k)).collect::<Vec<_>>()),
                    "plugins_seen": seen,
                });
            }
            emit("scaleform_callback", json!({"n": n, "movie_null": movie.is_null(), "value_null": value.is_null(), "detail": detail, "stack": if n < 3 { stack() } else { Value::Null }}));
        }
    });
    true
}

/// Patterned records: 'TST1' v1 written in one call; 'TST2' v2 opened then written in
/// three pieces; 'TST3' v3 empty.
unsafe extern "C" fn on_save(intfc: *const f4se::SerializationInterface) {
    // SAFETY: F4SE passes its serialization interface.
    let s = unsafe { &*intfc };
    let tag = |t: &[u8; 4]| u32::from_be_bytes(*t);
    let mut results = Vec::new();
    // SAFETY: calling the interface during the save callback, as plugins do.
    unsafe {
        results.push(s.write_record.unwrap()(tag(b"TST1"), 1, b"hello".as_ptr().cast(), 5));
        results.push(s.open_record.unwrap()(tag(b"TST2"), 2));
        for piece in [&b"abc"[..], &b"defg"[..], &[0u8, 1, 2, 255][..]] {
            results.push(s.write_record_data.unwrap()(piece.as_ptr().cast(), piece.len() as u32));
        }
        results.push(s.open_record.unwrap()(tag(b"TST3"), 3));
    }
    emit("serialization_save", json!({"results": results, "stack": stack()}));
}

unsafe extern "C" fn on_load(intfc: *const f4se::SerializationInterface) {
    // SAFETY: F4SE passes its serialization interface.
    let s = unsafe { &*intfc };
    let mut records = Vec::new();
    let (mut kind, mut version, mut len) = (0u32, 0u32, 0u32);
    // SAFETY: reading records during the load callback.
    unsafe {
        while s.get_next_record_info.unwrap()(&mut kind, &mut version, &mut len) {
            let mut buf = vec![0u8; len as usize];
            let read = s.read_record_data.unwrap()(buf.as_mut_ptr().cast(), len);
            records.push(json!({
                "type": String::from_utf8_lossy(&kind.to_be_bytes()), "version": version, "len": len,
                "read": read, "data": format!("{:02x?}", buf),
            }));
            if records.len() > 32 {
                break;
            }
        }
    }
    emit("serialization_load", json!({"records": records, "stack": stack()}));
}
unsafe extern "C" fn on_revert(_: *const f4se::SerializationInterface) {
    emit("serialization_revert", json!({"stack": stack()}));
}

// ---- Task delegates: record which thread and when they run, and how they are freed ----

#[repr(C)]
struct ProbeTask {
    base: TaskDelegate,
    label: &'static str,
}

unsafe extern "C" fn task_destroy(this: *mut TaskDelegate, flags: u32) -> *mut c_void {
    let task = this.cast::<ProbeTask>();
    // SAFETY: `this` is a ProbeTask we leaked from a Box.
    let label = unsafe { (*task).label };
    emit("task_destroy", json!({"label": label, "flags": flags}));
    if flags & 1 != 0 {
        // SAFETY: allocated by Box::into_raw in `queue_tasks`.
        drop(unsafe { Box::from_raw(task) });
    }
    this.cast()
}

unsafe extern "C" fn task_run(this: *mut TaskDelegate) {
    // SAFETY: as above.
    let label = unsafe { (*this.cast::<ProbeTask>()).label };
    emit("task_run", json!({"label": label, "stack": stack()}));
}

static TASK_VTBL: TaskDelegateVtbl = TaskDelegateVtbl { destroy: task_destroy, run: task_run };

fn queue_tasks() {
    let f4se = INTERFACE.load(Relaxed) as *const F4seInterface;
    // SAFETY: stored in Load; host pointers live for the process.
    unsafe {
        let Some(query) = (*f4se).query_interface else { return };
        let Some(tasks) = query(interface_id::TASK).cast::<f4se::TaskInterface>().as_ref() else { return };
        for (label, add) in [("task", tasks.add_task), ("ui_task", tasks.add_ui_task)] {
            let Some(add) = add else { continue };
            let task = Box::into_raw(Box::new(ProbeTask { base: TaskDelegate { vtable: &TASK_VTBL }, label }));
            add(task.cast());
            emit("task_queued", json!({"label": label}));
        }
    }
}
