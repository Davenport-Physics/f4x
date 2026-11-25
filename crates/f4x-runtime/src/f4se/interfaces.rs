//! The interface tables handed to F4SE plugins.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Mutex, OnceLock};

use f4x_abi::f4se::{
    F4seInterface, F4seInterfaceNg, Message, MessageCallback, MessagingInterface, PapyrusInterface,
    PapyrusRegisterCallback, PluginHandle, ScaleformInterface, TaskDelegate, TaskInterface, TaskInterfaceNg,
    TrampolineInterface, interface_id, message,
};
use f4x_abi::version::PackedVersion;

use super::trampoline::{self, Pool};
use super::{CURRENT, PLUGINS};

/// What we report as the F4SE version: 0.6.23 on old-gen; on next-gen 0.7.9, the F4SE for
/// Steam 1.11.240 (f4se.silverlock.org; black-box: it reports `0x00070090`).
pub(crate) const F4SE_VERSION: PackedVersion =
    f4x_engine::pick!(PackedVersion::new(0, 6, 23, 0), PackedVersion::new(0, 7, 9, 0));
/// Black-box: 25 under F4SE 0.6.23, 33 under F4SE 0.7.9.
pub(crate) const RELEASE_INDEX: u32 = f4x_engine::pick!(25, 33);
const HOST_SENDER: &CStr = c"F4SE";

/// The runtime version reported to plugins: 1.10.163 on old-gen; on next-gen the running
/// exe's file version (F4SE 0.7.9 reports `0x010B0F00`, 1.11.240.0, on the Steam exe).
pub(crate) fn runtime_version() -> PackedVersion {
    if !f4x_engine::NG {
        return f4x_abi::version::runtime::V1_10_163;
    }
    static RUNTIME: OnceLock<PackedVersion> = OnceLock::new();
    *RUNTIME.get_or_init(|| exe_version().unwrap_or(f4x_abi::version::runtime::V1_11_240))
}

/// The game exe's fixed file version, packed.
fn exe_version() -> Option<PackedVersion> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
    let mut path = [0u16; 1024];
    // SAFETY: valid buffer; null = the game exe.
    let len = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), path.len() as u32)
    } as usize;
    if len == 0 || len >= path.len() {
        return None;
    }
    // SAFETY: NUL-terminated path (the buffer was zeroed past `len`).
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut()) };
    let mut data = vec![0u8; size as usize];
    // SAFETY: buffer of `size` bytes.
    if size == 0 || unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut info_len = 0u32;
    // SAFETY: querying the root block of the data just read.
    if unsafe { VerQueryValueW(data.as_ptr().cast(), windows_sys::w!("\\"), &mut info, &mut info_len) } == 0
        || info.is_null()
        || (info_len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }
    // SAFETY: VerQueryValueW returned a VS_FIXEDFILEINFO inside `data`.
    let fixed = unsafe { std::ptr::read_unaligned(info.cast::<VS_FIXEDFILEINFO>()) };
    let (ms, ls) = (fixed.dwFileVersionMS, fixed.dwFileVersionLS);
    Some(PackedVersion::new(ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF))
}

pub fn interface() -> &'static F4seInterface {
    let base = || F4seInterface {
        f4se_version: F4SE_VERSION,
        runtime_version: runtime_version(),
        editor_version: PackedVersion(0),
        is_editor: 0,
        query_interface: Some(query_interface),
        get_plugin_handle: Some(get_plugin_handle),
        get_release_index: Some(get_release_index),
        get_plugin_info: Some(get_plugin_info),
    };
    if f4x_engine::NG {
        // Next-gen: the table continues with GetSaveFolderName (F4SE 0.7.1+).
        static IFACE_NG: OnceLock<F4seInterfaceNg> = OnceLock::new();
        return &IFACE_NG.get_or_init(|| F4seInterfaceNg { base: base(), get_save_folder_name: Some(get_save_folder_name) }).base;
    }
    static IFACE: OnceLock<F4seInterface> = OnceLock::new();
    IFACE.get_or_init(base)
}

/// The `My Games` folder name: `Fallout4` (black-box: what F4SE 0.7.9 returns on the
/// Steam exe; our reading of 1.11.240: the function that builds `Documents\My Games\%s\`
/// formats a static pointer to "Fallout4", which nothing writes).
unsafe extern "C" fn get_save_folder_name() -> *const c_char {
    c"Fallout4".as_ptr()
}

fn plugin_name(handle: PluginHandle) -> String {
    PLUGINS
        .lock()
        .ok()
        .and_then(|p| p.iter().find(|p| p.handle == handle).map(|p| p.name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| format!("plugin {handle}"))
}

unsafe extern "C" fn query_interface(id: u32) -> *mut c_void {
    let table: *const c_void = match id {
        interface_id::MESSAGING => &MESSAGING as *const _ as *const c_void,
        interface_id::SCALEFORM => &SCALEFORM as *const _ as *const c_void,
        interface_id::PAPYRUS => &PAPYRUS as *const _ as *const c_void,
        interface_id::SERIALIZATION => &super::serialization::INTERFACE as *const _ as *const c_void,
        interface_id::TASK if f4x_engine::NG => &TASK_NG as *const _ as *const c_void,
        interface_id::TASK => &TASK as *const _ as *const c_void,
        interface_id::TRAMPOLINE => &TRAMPOLINE as *const _ as *const c_void,
        other => {
            log::debug!(target: "f4x::f4se", "QueryInterface({other}) by {}: not available", plugin_name(CURRENT.load(Relaxed)));
            std::ptr::null()
        }
    };
    table as *mut c_void
}

unsafe extern "C" fn get_plugin_handle() -> PluginHandle {
    CURRENT.load(Relaxed)
}

unsafe extern "C" fn get_release_index() -> u32 {
    RELEASE_INDEX
}

/// Next-gen: `GetPluginInfo` answers once every plugin has loaded (black-box, F4SE 0.7.9:
/// null during Preload and Load, even for plugins loaded earlier; from PostLoad on, any
/// loaded plugin by name, case-insensitively).
pub(super) static PLUGIN_INFO_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(!f4x_engine::NG);

unsafe extern "C" fn get_plugin_info(name: *const c_char) -> *const c_void {
    if name.is_null() || !PLUGIN_INFO_READY.load(Relaxed) {
        return std::ptr::null();
    }
    // SAFETY: caller passes a NUL-terminated name.
    let wanted = unsafe { CStr::from_ptr(name) }.to_string_lossy().to_ascii_lowercase();
    PLUGINS
        .lock()
        .ok()
        .and_then(|p| {
            p.iter()
                .find(|p| p.name.to_string_lossy().to_ascii_lowercase() == wanted)
                .map(|p| p.info as *const _ as *const c_void)
        })
        .unwrap_or(std::ptr::null())
}

// ---- Messaging ----

static MESSAGING: MessagingInterface = MessagingInterface {
    interface_version: MessagingInterface::VERSION,
    register_listener: Some(register_listener),
    dispatch: Some(dispatch),
    get_event_dispatcher: Some(get_event_dispatcher),
};

struct Listener {
    owner: PluginHandle,
    /// Lower-case sender name; `None` = any plugin (not the host).
    sender: Option<String>,
    callback: MessageCallback,
}

static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

unsafe extern "C" fn register_listener(handle: PluginHandle, sender: *const c_char, callback: *mut c_void) -> bool {
    if callback.is_null() {
        return false;
    }
    let sender = (!sender.is_null()).then(|| {
        // SAFETY: NUL-terminated sender name.
        unsafe { CStr::from_ptr(sender) }.to_string_lossy().to_ascii_lowercase()
    });
    log::debug!(target: "f4x::f4se", "{} listens to {}", plugin_name(handle), sender.as_deref().unwrap_or("(any plugin)"));
    // SAFETY: the plugin passes a `void(Message*)` callback.
    let callback: MessageCallback = unsafe { std::mem::transmute(callback) };
    LISTENERS.lock().map(|mut l| l.push(Listener { owner: handle, sender, callback })).is_ok()
}

fn deliver(sender_name: &CStr, kind: u32, data: *mut c_void, data_len: u32, receiver: Option<&str>, from_host: bool) -> bool {
    let sender_lower = sender_name.to_string_lossy().to_ascii_lowercase();
    let targets: Vec<(PluginHandle, MessageCallback)> = LISTENERS
        .lock()
        .map(|l| {
            l.iter()
                .filter(|l| match &l.sender {
                    Some(s) => *s == sender_lower,
                    None => !from_host,
                })
                .map(|l| (l.owner, l.callback))
                .collect()
        })
        .unwrap_or_default();
    let mut delivered = false;
    for (owner, callback) in targets {
        if let Some(receiver) = receiver
            && !plugin_name(owner).eq_ignore_ascii_case(receiver)
        {
            continue;
        }
        let mut msg = Message { sender: sender_name.as_ptr(), kind, data_len, data };
        // SAFETY: the plugin's registered listener.
        unsafe { callback(&mut msg) };
        delivered = true;
    }
    delivered
}

unsafe extern "C" fn dispatch(
    handle: PluginHandle,
    kind: u32,
    data: *mut c_void,
    data_len: u32,
    receiver: *const c_char,
) -> bool {
    let receiver = (!receiver.is_null()).then(|| {
        // SAFETY: NUL-terminated receiver name.
        unsafe { CStr::from_ptr(receiver) }.to_string_lossy().into_owned()
    });
    let name = PLUGINS.lock().ok().and_then(|p| p.iter().find(|p| p.handle == handle).map(|p| p.name.clone()));
    let Some(name) = name else { return false };
    deliver(&name, kind, data, data_len, receiver.as_deref(), false)
}

unsafe extern "C" fn get_event_dispatcher(_id: u32) -> *mut c_void {
    std::ptr::null_mut()
}

/// Send a host message (sender `"F4SE"`) to every plugin listening for it.
pub fn dispatch_from_host(kind: u32, data: *mut c_void, data_len: u32) {
    log::info!(target: "f4x::f4se", "message {} to plugins", message::name(kind));
    deliver(HOST_SENDER, kind, data, data_len, None, true);
}

// ---- Scaleform (callbacks run per movie by `scaleform.rs`) ----

static SCALEFORM: ScaleformInterface =
    ScaleformInterface { interface_version: ScaleformInterface::VERSION, register: Some(scaleform_register) };

pub(crate) static SCALEFORM_CALLBACKS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

unsafe extern "C" fn scaleform_register(name: *const c_char, callback: *mut c_void) -> bool {
    if name.is_null() || callback.is_null() {
        return false;
    }
    // SAFETY: NUL-terminated name.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    log::info!(target: "f4x::f4se", "Scaleform registration {name:?}");
    SCALEFORM_CALLBACKS
        .lock()
        .map(|mut c| {
            // Next-gen: a name already taken is refused (black-box, F4SE 0.7.9: false, and
            // only the first registrant's callback runs).
            if f4x_engine::NG && c.iter().any(|(n, _)| *n == name) {
                log::warn!(target: "f4x::f4se", "Scaleform registration {name:?} refused: the name is taken");
                return false;
            }
            c.push((name, callback as usize));
            true
        })
        .unwrap_or(false)
}

// ---- Papyrus ----

static PAPYRUS: PapyrusInterface = PapyrusInterface {
    interface_version: PapyrusInterface::VERSION,
    register: Some(papyrus_register),
    get_external_event_registrations: Some(get_external_event_registrations),
};

static PAPYRUS_CALLBACKS: Mutex<Vec<(PluginHandle, usize)>> = Mutex::new(Vec::new());
static PAPYRUS_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn papyrus_register(callback: *mut c_void) -> bool {
    if callback.is_null() {
        return false;
    }
    let handle = CURRENT.load(Relaxed);
    if PAPYRUS_DONE.load(Relaxed) {
        // Registered after the VM came up: run it right away.
        if let Some(vm) = f4x_agent::observe::virtual_machine() {
            // SAFETY: the plugin's `bool(IVirtualMachine*)` callback.
            let f: PapyrusRegisterCallback = unsafe { std::mem::transmute(callback) };
            return unsafe { f(vm as *mut c_void) };
        }
    }
    PAPYRUS_CALLBACKS.lock().map(|mut c| c.push((handle, callback as usize))).is_ok()
}

/// `GetExternalEventRegistrations(eventName, data, functor)`: calls
/// `functor(handle, scriptName, callbackName, data)` for each registrant (CommonLibF4
/// `F4SE/Interfaces.h` signature).
unsafe extern "C" fn get_external_event_registrations(event: *const c_char, data: *mut c_void, functor: *mut c_void) {
    if event.is_null() || functor.is_null() {
        return;
    }
    type RegFunctor = unsafe extern "C" fn(u64, *const c_char, *const c_char, *mut c_void);
    // SAFETY: NUL-terminated event name; the plugin's functor.
    let (event, functor) = unsafe { (CStr::from_ptr(event).to_string_lossy().into_owned(), std::mem::transmute::<*mut c_void, RegFunctor>(functor)) };
    for r in super::natives::external_registrants(&event) {
        let (script, callback) = (CString::new(r.script).unwrap_or_default(), CString::new(r.callback).unwrap_or_default());
        // SAFETY: valid strings for the call.
        unsafe { functor(r.handle, script.as_ptr(), callback.as_ptr(), data) };
    }
}

pub(super) fn papyrus_done() -> bool {
    PAPYRUS_DONE.load(Relaxed)
}

pub(super) fn run_papyrus_registrations() {
    if PAPYRUS_DONE.swap(true, Relaxed) {
        return;
    }
    let Some(vm) = f4x_agent::observe::virtual_machine() else {
        log::warn!(target: "f4x::f4se", "no Papyrus VM seen; plugin native registrations skipped");
        return;
    };
    let types = super::papyrus_natives::register_form_scripts(f4x_engine::papyrus::Vm(vm as *mut _));
    log::info!(target: "f4x::papyrus", "form script types: {}", types.join(", "));
    super::natives::bind_all(vm);
    let callbacks: Vec<(PluginHandle, usize)> = PAPYRUS_CALLBACKS.lock().map(|c| c.clone()).unwrap_or_default();
    for (handle, callback) in callbacks {
        // SAFETY: the plugin's `bool(IVirtualMachine*)` callback.
        let f: PapyrusRegisterCallback = unsafe { std::mem::transmute(callback) };
        let ok = unsafe { f(vm as *mut c_void) };
        log::info!(target: "f4x::f4se", "Papyrus registration of {}: {ok}", plugin_name(handle));
    }
}

// ---- Serialization: see `serialization.rs` ----

// ---- Tasks (run on the main-thread pump, then destroyed like F4SE does: slot 0, flags 1) ----

static TASK: TaskInterface =
    TaskInterface { interface_version: TaskInterface::VERSION, add_task: Some(add_task), add_ui_task: Some(add_task) };

/// Next-gen: the same, plus `AddTaskPermanent` (version still 2, black-box F4SE 0.7.9).
static TASK_NG: TaskInterfaceNg = TaskInterfaceNg {
    interface_version: TaskInterface::VERSION,
    add_task: Some(add_task),
    add_ui_task: Some(add_task),
    add_task_permanent: Some(add_task_permanent),
};

/// Permanent tasks, run on every main-loop iteration and never destroyed (black-box, F4SE
/// 0.7.9: a permanent task ran on the main thread every frame from the first frame on, in
/// the same frame as the one-shot tasks, and its destructor was never called).
static PERMANENT_TASKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe extern "C" fn add_task_permanent(task: *mut TaskDelegate) {
    if task.is_null() {
        return;
    }
    let first = PERMANENT_TASKS
        .lock()
        .map(|mut tasks| {
            tasks.push(task as usize);
            tasks.len() == 1
        })
        .unwrap_or(false);
    if first {
        f4x_agent::pump::add_ticker(run_permanent_tasks);
    }
}

fn run_permanent_tasks() {
    // Copy first: a task may add another.
    let tasks: Vec<usize> = PERMANENT_TASKS.lock().map(|t| t.clone()).unwrap_or_default();
    for task in tasks {
        let task = task as *mut TaskDelegate;
        // SAFETY: a live ITaskDelegate the plugin handed over for good.
        let _ = std::panic::catch_unwind(|| unsafe { ((*(*task).vtable).run)(task) });
    }
}

struct SendTask(*mut TaskDelegate);
// SAFETY: the delegate is handed over to the main thread and used only there.
unsafe impl Send for SendTask {}

unsafe extern "C" fn add_task(task: *mut TaskDelegate) {
    if task.is_null() {
        return;
    }
    let task = SendTask(task);
    f4x_agent::pump::post(move || {
        let task = task;
        // SAFETY: a live ITaskDelegate from the plugin; run once, then delete.
        unsafe {
            let vtbl = &*(*task.0).vtable;
            (vtbl.run)(task.0);
            (vtbl.destroy)(task.0, 1);
        }
    });
}

// ---- Trampoline ----

static TRAMPOLINE: TrampolineInterface = TrampolineInterface {
    interface_version: TrampolineInterface::VERSION,
    allocate_from_branch_pool: Some(alloc_branch),
    allocate_from_local_pool: Some(alloc_local),
};

fn game_base() -> usize {
    // SAFETY: trivially safe.
    unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) as usize }
}

unsafe extern "C" fn alloc_branch(handle: PluginHandle, size: usize) -> *mut c_void {
    let p = trampoline::BRANCH.lock().map(|mut pool| pool.allocate(game_base(), size)).unwrap_or(std::ptr::null_mut());
    log::info!(target: "f4x::f4se", "{} allocated {size} bytes from the branch pool at {p:p}", plugin_name(handle));
    p
}

unsafe extern "C" fn alloc_local(handle: PluginHandle, size: usize) -> *mut c_void {
    let module = PLUGINS.lock().ok().and_then(|p| p.iter().find(|p| p.handle == handle).map(|p| p.module)).unwrap_or_else(game_base);
    let p = trampoline::LOCAL
        .lock()
        .map(|mut pools| {
            if let Some((_, pool)) = pools.iter_mut().find(|(m, _)| *m == module) {
                return pool.allocate(module, size);
            }
            let mut pool = Pool::new();
            let p = pool.allocate(module, size);
            pools.push((module, pool));
            p
        })
        .unwrap_or(std::ptr::null_mut());
    log::info!(target: "f4x::f4se", "{} allocated {size} bytes from its local pool at {p:p}", plugin_name(handle));
    p
}
