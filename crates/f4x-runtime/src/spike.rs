//! Spike S1: main-loop hook + control channel.
//!
//! The "frame" signal is an IAT hook on `USER32!PeekMessageA`, which the game's main
//! loop calls every iteration. It needs no game addresses, which is all this spike needs.
//! Phase 1 replaces it with a real frame hook.

use std::ffi::c_void;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

use serde_json::{Value, json};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, TerminateProcess};

use crate::session::{self, Session};

static PUMP_CALLS: AtomicU64 = AtomicU64::new(0);
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);
static REAL_PEEK_MESSAGE: AtomicUsize = AtomicUsize::new(0);

type PeekMessageA = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> i32;

unsafe extern "system" fn peek_message_hook(
    msg: *mut c_void,
    window: *mut c_void,
    min: u32,
    max: u32,
    remove: u32,
) -> i32 {
    PUMP_CALLS.fetch_add(1, Relaxed);
    // SAFETY: trivially safe Win32 call.
    MAIN_THREAD.store(unsafe { GetCurrentThreadId() }, Relaxed);
    // SAFETY: REAL_PEEK_MESSAGE holds the bound USER32 import, set before the hook went live.
    unsafe {
        let real: PeekMessageA = std::mem::transmute(REAL_PEEK_MESSAGE.load(Relaxed));
        real(msg, window, min, max, remove)
    }
}

/// Runs inside `DllMain(DLL_PROCESS_ATTACH)`: keep it short and lock-free.
pub fn attach() {
    session::log(&format!("f4x runtime {} attached (pid {})", env!("CARGO_PKG_VERSION"), std::process::id()));

    // SAFETY: GetModuleHandleW(null) is the game image, which is mapped and bound by now.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) }.cast::<u8>();
    // SAFETY: `base` is the mapped exe.
    match unsafe { crate::iat::hook(base, "USER32.dll", "PeekMessageA", peek_message_hook as PeekMessageA as usize) } {
        Ok(real) => {
            REAL_PEEK_MESSAGE.store(real, Relaxed);
            session::log(&format!("hooked USER32!PeekMessageA (real at {real:#x})"));
        }
        Err(error) => session::log(&format!("PeekMessageA hook failed: {error}")),
    }

    if std::env::var_os("F4X_CONTROL").is_some_and(|v| v == "1")
        && let Some(session) = session::get()
    {
        let _ = std::thread::Builder::new().name("f4x-control".into()).spawn(move || control_main(session));
    }
}

fn token() -> String {
    let state = RandomState::new();
    format!("{:016x}{:016x}", state.hash_one(1u8), state.hash_one(2u8))
}

fn control_main(session: &'static Session) {
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) => {
            session.log(&format!("control: bind failed: {error}"));
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = token();
    session.write_atomic(
        "session.json",
        &json!({"pid": std::process::id(), "port": port, "token": token}).to_string(),
    );
    session.log(&format!("control: listening on 127.0.0.1:{port}"));

    let _ = std::thread::Builder::new().name("f4x-heartbeat".into()).spawn(move || {
        loop {
            session.write_atomic(
                "heartbeat.json",
                &json!({"uptime_ms": session.uptime_ms() as u64, "pump_calls": PUMP_CALLS.load(Relaxed)})
                    .to_string(),
            );
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    for stream in listener.incoming().flatten() {
        let token = token.clone();
        let _ = std::thread::Builder::new().name("f4x-client".into()).spawn(move || {
            if let Err(error) = serve(session, stream, &token) {
                session.log(&format!("control: client ended: {error}"));
            }
        });
    }
}

fn serve(session: &Session, stream: TcpStream, token: &str) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    let mut authed = false;
    for line in reader.lines() {
        let line = line?;
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                writeln!(writer, "{}", json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}))?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let reply = match (authed, method) {
            (_, "auth") => {
                if request.pointer("/params/token").and_then(Value::as_str) == Some(token) {
                    authed = true;
                    Ok(json!({"ok": true}))
                } else {
                    Err((-32001, "bad token"))
                }
            }
            (false, _) => Err((-32001, "auth first")),
            (true, "ping") => Ok(json!({
                "pid": std::process::id(),
                "uptime_ms": session.uptime_ms() as u64,
                "pump_calls": PUMP_CALLS.load(Relaxed),
                "main_thread": MAIN_THREAD.load(Relaxed),
            })),
            (true, "quit") => {
                writeln!(writer, "{}", json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}}))?;
                writer.flush()?;
                session.log("control: quit requested");
                // SAFETY: terminating our own process on request.
                unsafe { TerminateProcess(GetCurrentProcess(), 0) };
                return Ok(());
            }
            (true, _) => Err((-32601, "unknown method")),
        };
        let response = match reply {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
        };
        writeln!(writer, "{response}")?;
    }
    Ok(())
}
