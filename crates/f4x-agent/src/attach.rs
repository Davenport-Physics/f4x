//! Install the agent into the game process.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

use f4x_abi::service::Service;
use f4x_proto::{Heartbeat, ScreenshotParams, method};
use serde_json::json;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

use crate::server::{self, params};
use crate::{clock, crash, d3d, hooks, logsink, methods, msgbox, observe, session};

pub struct AttachOptions {
    /// `"f4x"` or `"f4se"`.
    pub host: &'static str,
    /// Called on the main thread at the first main-loop iteration (e.g. load plugins).
    pub on_main_loop: Option<fn()>,
}

static REAL_PEEK: AtomicUsize = AtomicUsize::new(0);
static ON_MAIN_LOOP: std::sync::OnceLock<Option<fn()>> = std::sync::OnceLock::new();

type PeekMessageA = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> i32;

/// The main-loop hook: `USER32!PeekMessageA`, called by the game's message pump on the
/// main thread every loop iteration. A safe point between frames for engine work.
unsafe extern "system" fn peek_hook(msg: *mut c_void, wnd: *mut c_void, min: u32, max: u32, remove: u32) -> i32 {
    let first = clock::pumps() == 0;
    crate::pump::pump();
    if first
        && let Some(Some(callback)) = ON_MAIN_LOOP.get()
    {
        let _ = std::panic::catch_unwind(*callback);
    }
    // SAFETY: the bound import, saved before the hook went live.
    unsafe { std::mem::transmute::<usize, PeekMessageA>(REAL_PEEK.load(Relaxed))(msg, wnd, min, max, remove) }
}

/// Install everything. Safe to call from `DllMain` (no locks taken on other threads,
/// no module loads) or from an F4SE plugin's `Load`.
pub fn attach(options: AttachOptions) -> &'static Service {
    clock::start();
    let session = session::get();
    if let Some(session) = session {
        logsink::sink().open_file(&session.dir.join(f4x_proto::files::TRACE));
    }
    logsink::install_logger();
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(target: "f4x::panic", "{info}\n{backtrace}");
        logsink::sink().flush();
    }));
    log::info!(target: "f4x", "f4x agent {} attached (host {}, pid {})", env!("CARGO_PKG_VERSION"), options.host, std::process::id());
    let _ = ON_MAIN_LOOP.set(options.on_main_loop);

    crash::install();
    if crate::test_mode() {
        msgbox::install();
    }

    // SAFETY: GetModuleHandleW(null) is the game image.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    // SAFETY: the game image's imports are bound by now.
    match unsafe { hooks::iat_hook(base as *mut u8, "USER32.dll", "PeekMessageA", peek_hook as PeekMessageA as usize) } {
        Ok(real) => REAL_PEEK.store(real, Relaxed),
        Err(error) => log::error!(target: "f4x", "no main-loop hook: {error}"),
    }
    d3d::install(base as *mut u8);
    observe::install(base);
    crate::input::install();

    let service = crate::service::table(options.host);
    let _ = methods::PROVIDERS.set(methods::StateProviders {
        menus: observe::active_menus,
        exceptions: crash::exception_count,
        quit,
    });
    methods::register_builtin(options.host);
    register_windows_methods();
    crate::engine_rpc::register();
    crate::input::register_rpc();
    crate::ui::register();
    crate::compat_rpc::register();

    if std::env::var_os("F4X_CONTROL").is_some_and(|v| v == "1") {
        match server::start(options.host, &game_version()) {
            Ok(_) => {}
            Err(error) => log::error!(target: "f4x::server", "control server not started: {error}"),
        }
    }
    // The heartbeat is for the host's supervisor; normal play has none.
    if let Some(session) = session.filter(|s| s.hosted) {
        let _ = std::thread::Builder::new().name("f4x-heartbeat".into()).spawn(move || {
            loop {
                let beat = Heartbeat { uptime_ms: clock::uptime_ms(), frames: clock::frames(), pumps: clock::pumps() };
                let _ = session.write_atomic(f4x_proto::files::HEARTBEAT, serde_json::to_string(&beat).unwrap_or_default().as_bytes());
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }
    service
}

fn quit(code: i32) {
    logsink::sink().flush();
    // SAFETY: terminating our own process on request. TerminateProcess is used instead
    // of ExitProcess because the game's shutdown path can hang under Wine.
    unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
}

fn game_version() -> String {
    // The exe's fixed file version would be better; the RTTI-independent PE timestamp is
    // enough to tell builds apart for now.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    // SAFETY: reading the mapped PE header.
    let stamp = unsafe {
        let nt = base + (*((base + 0x3C) as *const i32)) as usize;
        *((nt + 8) as *const u32)
    };
    match stamp {
        0x5ddc_40fa => "1.10.163.0".to_owned(),
        other => format!("unknown (timestamp {other:#x})"),
    }
}

fn register_windows_methods() {
    server::register(method::SCREENSHOT, |_, p| {
        let p: ScreenshotParams = params(p)?;
        let shot = d3d::screenshot(&p, Duration::from_secs(10))?;
        Ok(serde_json::to_value(shot).unwrap_or_default())
    });
    server::register("patches", |_, _| crate::patches::map().map_err(|e| f4x_proto::RpcError::new(f4x_proto::code::INTERNAL, e)));
    server::register("catalog.natives", |_, _| {
        let session = session::get().ok_or_else(|| f4x_proto::RpcError::new(f4x_proto::code::INTERNAL, "no session"))?;
        let text = observe::natives_jsonl();
        session
            .write_atomic("natives.jsonl", text.as_bytes())
            .map_err(|e| f4x_proto::RpcError::new(f4x_proto::code::INTERNAL, e.to_string()))?;
        Ok(json!({"natives": observe::native_count(), "file": "natives.jsonl"}))
    });
    server::register(method::LOGS_SUBSCRIBE, |conn, p| {
        let p: f4x_proto::SubscribeParams = params(p)?;
        logsink::sink().subscribe(&p, conn.notify.clone());
        if let Ok(mut crash_subscribers) = crash::NOTIFY.lock() {
            crash_subscribers.push(conn.notify.clone());
        }
        Ok(json!({"ok": true}))
    });
    if crate::test_mode() {
        server::register(method::DEBUG_CRASH, |_, p| {
            let kind = p.get("kind").and_then(serde_json::Value::as_str).unwrap_or("access_violation").to_owned();
            // Crash on the main thread, like a real engine bug would.
            crate::pump::post(move || crash::crash_on_purpose(&kind));
            Ok(json!({"ok": true}))
        });
    }
}
