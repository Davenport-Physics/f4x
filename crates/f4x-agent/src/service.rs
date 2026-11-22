//! The C-ABI service table handed to plugins (`f4x_abi::service::Service`).

use std::ffi::c_void;
use std::sync::OnceLock;

use f4x_abi::service::{ConsoleCommandSpec, SERVICE_VERSION, Service, Str, TestVtbl};
use serde_json::Value;

static TABLE: OnceLock<Service> = OnceLock::new();

/// The table for this process (the first call fixes `host`).
pub fn table(host: &'static str) -> &'static Service {
    TABLE.get_or_init(|| Service {
        struct_size: std::mem::size_of::<Service>() as u32,
        version: SERVICE_VERSION,
        host: Str::new(host),
        log: svc_log,
        register_test: svc_register_test,
        frames: svc_frames,
        pumps: svc_pumps,
        run_on_main: svc_run_on_main,
        register_console_command: svc_register_console_command,
        console_print: svc_console_print,
        console_execute: svc_console_execute,
    })
}

pub fn get() -> Option<&'static Service> {
    TABLE.get()
}

unsafe extern "C" fn svc_log(level: u32, plugin: Str, target: Str, message: Str) {
    // SAFETY: the plugin passes valid UTF-8 for the duration of the call.
    let (plugin, target, message) = unsafe { (plugin.as_str(), target.as_str(), message.as_str()) };
    crate::logsink::sink().emit(level, target, message.to_owned(), Value::Null, (!plugin.is_empty()).then_some(plugin));
}

unsafe extern "C" fn svc_register_test(plugin: Str, name: Str, vtbl: *const TestVtbl, user: *mut c_void) -> bool {
    // SAFETY: as above.
    let (plugin, name) = unsafe { (plugin.as_str(), name.as_str()) };
    !vtbl.is_null() && crate::tests::register(plugin, name, vtbl, user)
}

unsafe extern "C" fn svc_frames() -> u64 {
    crate::clock::frames()
}

unsafe extern "C" fn svc_pumps() -> u64 {
    crate::clock::pumps()
}

struct SendPtr(*mut c_void);
// SAFETY: the plugin hands the pointer over to be used by its own job on the main thread.
unsafe impl Send for SendPtr {}

unsafe extern "C" fn svc_run_on_main(job: unsafe extern "C" fn(*mut c_void), user: *mut c_void) {
    let user = SendPtr(user);
    crate::pump::post(move || {
        let user = user;
        // SAFETY: the plugin's job contract.
        unsafe { job(user.0) }
    });
}

unsafe extern "C" fn svc_register_console_command(plugin: Str, spec: *const ConsoleCommandSpec) -> bool {
    #[cfg(windows)]
    {
        if spec.is_null() {
            return false;
        }
        // SAFETY: valid strings and spec for the call (plugin contract).
        let plugin = unsafe { plugin.as_str() }.to_owned();
        match unsafe { crate::console_cmds::register(&plugin, &*spec) } {
            Ok(_) => true,
            Err(error) => {
                log::warn!(target: "f4x::console", "{plugin}: console command not registered: {error}");
                false
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (plugin, spec);
        false
    }
}

unsafe extern "C" fn svc_console_print(text: Str) -> bool {
    // SAFETY: valid UTF-8 for the call.
    let text = unsafe { text.as_str() }.to_owned();
    #[cfg(windows)]
    {
        if crate::clock::on_main_thread() {
            return f4x_engine::console::print(&text).is_ok();
        }
        crate::pump::post(move || {
            let _ = f4x_engine::console::print(&text);
        });
        true
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

unsafe extern "C" fn svc_console_execute(command: Str) -> bool {
    // SAFETY: valid UTF-8 for the call.
    let command = unsafe { command.as_str() }.to_owned();
    #[cfg(windows)]
    {
        if crate::clock::on_main_thread() {
            return f4x_engine::console::execute(&command).is_ok();
        }
        crate::pump::post(move || {
            let _ = f4x_engine::console::execute(&command);
        });
        true
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        false
    }
}
