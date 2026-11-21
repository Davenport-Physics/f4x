//! Support code for the `plugin!` macro. Not public API.

use std::ffi::c_void;
use std::sync::OnceLock;

use f4x_abi::f4se::{self, F4seInterface, Message, PluginInfo, interface_id, message};
use f4x_abi::service::{SERVICE_VERSION, Service, Str};

use crate::{Context, NAME, SERVICE};

type LoadFn = fn(&Context) -> Result<(), String>;

static PENDING_LOAD: OnceLock<LoadFn> = OnceLock::new();

fn name_str(name_nul: &'static str) -> &'static str {
    name_nul.trim_end_matches('\0')
}

/// Connect to the agent and run the plugin's `load`.
///
/// # Safety
/// `service` must be null or the agent's service table.
pub unsafe fn load(name_nul: &'static str, service: *const Service, user_load: LoadFn) -> bool {
    std::panic::catch_unwind(|| {
        // SAFETY: as documented.
        let Some(service) = (unsafe { service.as_ref() }) else { return false };
        if service.version < SERVICE_VERSION {
            return false;
        }
        let _ = NAME.set(name_str(name_nul));
        let _ = SERVICE.set(service);
        install_logger();
        match user_load(&Context { _private: () }) {
            Ok(()) => {
                log::info!("plugin loaded (host {})", crate::host());
                true
            }
            Err(error) => {
                log::error!("load failed: {error}");
                false
            }
        }
    })
    .unwrap_or(false)
}

/// # Safety
/// Called by F4SE with valid pointers.
pub unsafe fn f4se_query(name_nul: &'static str, _f4se: *const F4seInterface, info: *mut PluginInfo) -> bool {
    // SAFETY: host-provided pointer; the name is a 'static NUL-terminated literal.
    unsafe {
        (*info).info_version = PluginInfo::VERSION;
        (*info).name = name_nul.as_ptr().cast();
        (*info).version = 1;
    }
    true
}

/// Under real F4SE the agent is another plugin: connect now if it is already loaded,
/// otherwise at `PostLoad` (sent once every plugin has loaded).
///
/// # Safety
/// Called by F4SE with a valid interface.
pub unsafe fn f4se_load(name_nul: &'static str, f4se: *const F4seInterface, user_load: LoadFn) -> bool {
    let _ = NAME.set(name_str(name_nul));
    if let Some(service) = find_service() {
        // SAFETY: the agent's own table.
        return unsafe { load(name_nul, service, user_load) };
    }
    let _ = PENDING_LOAD.set(user_load);
    // SAFETY: F4SE's interface and messaging table.
    unsafe {
        let f4se = &*f4se;
        let (Some(query), Some(handle)) = (f4se.query_interface, f4se.get_plugin_handle) else { return false };
        let messaging = query(interface_id::MESSAGING).cast::<f4se::MessagingInterface>();
        let Some(register) = messaging.as_ref().and_then(|m| m.register_listener) else { return false };
        register(handle(), message::HOST_SENDER.as_ptr(), on_f4se_message as f4se::MessageCallback as *mut c_void)
    }
}

unsafe extern "C" fn on_f4se_message(msg: *mut Message) {
    // SAFETY: F4SE passes a valid message.
    let kind = unsafe { (*msg).kind };
    if kind != message::POST_LOAD {
        return;
    }
    let (Some(load_fn), Some(name)) = (PENDING_LOAD.get(), NAME.get()) else { return };
    match find_service() {
        Some(service) => {
            let name_nul: &'static str = Box::leak(format!("{name}\0").into_boxed_str());
            // SAFETY: the agent's own table.
            unsafe { load(name_nul, service, *load_fn) };
        }
        None => {
            // No agent: nothing to talk to. The plugin stays inert.
        }
    }
}

fn find_service() -> Option<*const Service> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
        for module in f4x_abi::service::SERVICE_MODULES {
            // SAFETY: NUL-terminated names; no reference is taken on the module.
            unsafe {
                let handle = GetModuleHandleA(module.as_ptr().cast());
                if handle.is_null() {
                    continue;
                }
                let Some(export) = GetProcAddress(handle, f4x_abi::service::SERVICE_EXPORT.as_ptr().cast()) else { continue };
                let get: f4x_abi::service::ServiceFn = std::mem::transmute(export);
                let table = get();
                if !table.is_null() {
                    return Some(table);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    None
}

struct ServiceLogger;

impl log::Log for ServiceLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let Some(service) = crate::service() else { return };
        let message = record.args().to_string();
        // SAFETY: valid strings for the duration of the call.
        unsafe {
            (service.log)(record.level() as u32, Str::new(crate::plugin_name()), Str::new(record.target()), Str::new(&message));
        }
    }

    fn flush(&self) {}
}

fn install_logger() {
    static LOGGER: ServiceLogger = ServiceLogger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

