//! The interface tables handed to F4SE plugins.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Mutex, OnceLock};

use f4x_abi::f4se::{
    F4seInterface, Message, MessageCallback, MessagingInterface, PapyrusInterface, PapyrusRegisterCallback,
    PluginHandle, ScaleformInterface, TaskDelegate, TaskInterface, TrampolineInterface,
    interface_id, message,
};
use f4x_abi::version::PackedVersion;

use super::trampoline::{self, Pool};
use super::{CURRENT, PLUGINS};

/// What we report as the F4SE version (0.6.23, as plugins expect on old-gen).
pub(crate) const F4SE_VERSION: PackedVersion = PackedVersion::new(0, 6, 23, 0);
const RELEASE_INDEX: u32 = 25;
const HOST_SENDER: &CStr = c"F4SE";

pub fn interface() -> &'static F4seInterface {
    static IFACE: OnceLock<F4seInterface> = OnceLock::new();
    IFACE.get_or_init(|| F4seInterface {
        f4se_version: F4SE_VERSION,
        runtime_version: f4x_abi::version::runtime::V1_10_163,
        editor_version: PackedVersion(0),
        is_editor: 0,
        query_interface: Some(query_interface),
        get_plugin_handle: Some(get_plugin_handle),
        get_release_index: Some(get_release_index),
        get_plugin_info: Some(get_plugin_info),
    })
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

unsafe extern "C" fn get_plugin_info(name: *const c_char) -> *const c_void {
    if name.is_null() {
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
    SCALEFORM_CALLBACKS.lock().map(|mut c| c.push((name, callback as usize))).is_ok()
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

unsafe extern "C" fn get_external_event_registrations(_event: *const c_char, _data: *mut c_void, _functor: *mut c_void) {}

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
