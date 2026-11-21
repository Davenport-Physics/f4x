//! Crash capture.
//!
//! A vectored exception handler records serious exceptions as *first-chance* reports
//! (`crash-<n>.json`, `fatal: false`) and always continues the search, so the game's own
//! handlers and tools like Buffout 4 still run. An unhandled-exception filter (which
//! chains to whatever filter was installed before it) re-writes the report as
//! `fatal: true`. The host treats a session that ends without `quit` as crashed and
//! picks up the reports.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

use f4x_proto::{CrashReport, Frame, Notification, notification};
use serde_json::json;
use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, RtlLookupFunctionEntry, RtlVirtualUnwind,
    SetUnhandledExceptionFilter,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW,
};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
const MAX_FIRST_CHANCE: u64 = 16;

static EXCEPTIONS: AtomicU64 = AtomicU64::new(0);
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
/// Serialises report writing (several threads can fault at once).
static WRITING: Mutex<()> = Mutex::new(());
/// Subscribers to crash notifications (connections that asked for logs).
pub static NOTIFY: Mutex<Vec<std::sync::mpsc::Sender<String>>> = Mutex::new(Vec::new());

pub fn exception_count() -> u64 {
    EXCEPTIONS.load(Relaxed)
}

type TopLevelFilter = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;

/// Put our unhandled-exception filter back on top (the game installs its own during
/// start-up), still chaining to whatever was installed before.
pub fn reassert_filter() {
    // SAFETY: as in `install`.
    unsafe {
        let previous = SetUnhandledExceptionFilter(Some(unhandled));
        if let Some(previous) = previous
            && previous as usize != unhandled as TopLevelFilter as usize
        {
            PREVIOUS_FILTER.store(previous as usize, Relaxed);
            log::debug!(target: "f4x::crash", "re-asserted the unhandled-exception filter (chaining to the game's)");
        }
    }
}

pub fn install() {
    // SAFETY: registering process-wide handlers with valid function pointers.
    unsafe {
        AddVectoredExceptionHandler(0, Some(vectored));
        let previous = SetUnhandledExceptionFilter(Some(unhandled));
        PREVIOUS_FILTER.store(previous.map_or(0, |f| f as usize), Relaxed);
        crate::pump::add_ticker(|| {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(reassert_filter);
        });
    }
    log::debug!(target: "f4x::crash", "crash handlers installed");
}

fn serious(code: u32) -> bool {
    // Error-severity NTSTATUS codes, minus well-known noise.
    code >> 30 == 3 && !matches!(code, 0xE06D_7363 /* C++ throw */ | 0xE043_4352 /* CLR */)
}

unsafe extern "system" fn vectored(pointers: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the OS passes valid exception pointers.
    let code = unsafe { (*(*pointers).ExceptionRecord).ExceptionCode } as u32;
    if serious(code) {
        let n = EXCEPTIONS.fetch_add(1, Relaxed);
        if n < MAX_FIRST_CHANCE {
            // SAFETY: as above.
            unsafe { record(pointers, false) };
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

unsafe extern "system" fn unhandled(pointers: *const EXCEPTION_POINTERS) -> i32 {
    // SAFETY: as above.
    unsafe { record(pointers, true) };
    crate::logsink::sink().flush();
    let previous = PREVIOUS_FILTER.load(Relaxed);
    if previous != 0 {
        // SAFETY: the filter that was installed before ours.
        return unsafe { std::mem::transmute::<usize, TopLevelFilter>(previous)(pointers) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// # Safety
/// `pointers` must be the OS-provided exception pointers.
unsafe fn record(pointers: *const EXCEPTION_POINTERS, fatal: bool) {
    let _ = std::panic::catch_unwind(|| {
        let Ok(_guard) = WRITING.lock() else { return };
        // SAFETY: as documented.
        let report = unsafe { build(pointers, fatal) };
        log::error!(
            target: "f4x::crash",
            "{} exception {:#010x} ({}) at {}",
            if fatal { "fatal" } else { "first-chance" },
            report.code,
            report.code_name,
            describe(&report.address)
        );
        if let Some(session) = crate::session::get() {
            let name = format!("{}{}.json", f4x_proto::files::CRASH_PREFIX, if fatal { "fatal".to_owned() } else { EXCEPTIONS.load(Relaxed).to_string() });
            let _ = session.write_atomic(&name, serde_json::to_string_pretty(&report).unwrap_or_default().as_bytes());
        }
        if let Ok(mut subscribers) = NOTIFY.lock() {
            let note = serde_json::to_string(&Notification::new(notification::CRASH, serde_json::to_value(&report).unwrap_or_default())).unwrap_or_default();
            subscribers.retain(|tx| tx.send(note.clone()).is_ok());
        }
        crate::logsink::sink().flush();
    });
}

fn describe(frame: &Frame) -> String {
    match (&frame.module, frame.rva) {
        (Some(module), Some(rva)) => format!("{module}+{rva:#x}"),
        _ => format!("{:#x}", frame.address),
    }
}

/// # Safety
/// As [`record`].
unsafe fn build(pointers: *const EXCEPTION_POINTERS, fatal: bool) -> CrashReport {
    // SAFETY: valid exception pointers.
    let (record, context) = unsafe { (&*(*pointers).ExceptionRecord, &*(*pointers).ContextRecord) };
    let code = record.ExceptionCode as u32;
    let access = (code == EXCEPTION_ACCESS_VIOLATION as u32 && record.NumberParameters >= 2).then(|| {
        let kind = match record.ExceptionInformation[0] {
            0 => "read",
            1 => "write",
            8 => "execute",
            _ => "unknown",
        };
        (kind.to_owned(), record.ExceptionInformation[1] as u64)
    });
    CrashReport {
        t_ms: crate::clock::t_ms(),
        thread: crate::clock::thread_id(),
        code,
        code_name: code_name(code).to_owned(),
        fatal,
        address: frame(record.ExceptionAddress as u64),
        access,
        // SAFETY: unwinding a copy of the faulting context.
        frames: unsafe { backtrace(context) },
        registers: json!({
            "rip": format!("{:#x}", context.Rip), "rsp": format!("{:#x}", context.Rsp), "rbp": format!("{:#x}", context.Rbp),
            "rax": format!("{:#x}", context.Rax), "rbx": format!("{:#x}", context.Rbx), "rcx": format!("{:#x}", context.Rcx),
            "rdx": format!("{:#x}", context.Rdx), "rsi": format!("{:#x}", context.Rsi), "rdi": format!("{:#x}", context.Rdi),
            "r8": format!("{:#x}", context.R8), "r9": format!("{:#x}", context.R9), "r10": format!("{:#x}", context.R10),
            "r11": format!("{:#x}", context.R11), "r12": format!("{:#x}", context.R12), "r13": format!("{:#x}", context.R13),
            "r14": format!("{:#x}", context.R14), "r15": format!("{:#x}", context.R15),
        }),
        flight_recorder: crate::logsink::sink().tail(64),
    }
}

/// Walk the stack from the faulting context with the x64 unwind tables.
///
/// # Safety
/// `context` must be a valid thread context.
unsafe fn backtrace(context: &CONTEXT) -> Vec<Frame> {
    let mut ctx: CONTEXT = *context;
    let mut frames = Vec::new();
    for _ in 0..64 {
        if ctx.Rip == 0 {
            break;
        }
        frames.push(frame(ctx.Rip));
        let mut image_base = 0u64;
        // SAFETY: lookups and unwinding on our own copy of the context.
        unsafe {
            let entry = RtlLookupFunctionEntry(ctx.Rip, &mut image_base, std::ptr::null_mut());
            if entry.is_null() {
                // Leaf function: the return address is on top of the stack.
                if !readable(ctx.Rsp as *const c_void, 8) {
                    break;
                }
                ctx.Rip = *(ctx.Rsp as *const u64);
                ctx.Rsp += 8;
            } else {
                let mut handler_data = std::ptr::null_mut();
                let mut establisher = 0u64;
                RtlVirtualUnwind(0, image_base, ctx.Rip, entry, &mut ctx, &mut handler_data, &mut establisher, std::ptr::null_mut());
            }
        }
        if !readable(ctx.Rsp as *const c_void, 8) {
            if ctx.Rip != 0 {
                frames.push(frame(ctx.Rip));
            }
            break;
        }
    }
    frames
}

/// `module+rva` for an address.
pub fn frame(address: u64) -> Frame {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: lookup by address only; no reference is taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    } != 0;
    if !found {
        return Frame { address, module: None, rva: None, symbol: None };
    }
    let mut buf = [0u16; 260];
    // SAFETY: valid buffer.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
    Frame { address, module: Some(name), rva: Some(address - module as u64), symbol: None }
}

/// Whether `len` bytes at `p` are committed, readable memory.
pub fn readable(p: *const c_void, len: usize) -> bool {
    if (p as usize) < 0x10000 {
        return false;
    }
    // SAFETY: VirtualQuery only inspects the address space.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let got = unsafe { VirtualQuery(p, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    got != 0
        && info.State == MEM_COMMIT
        && info.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0
        && (p as usize).checked_add(len).is_some_and(|end| end <= info.BaseAddress as usize + info.RegionSize)
}

fn code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
        0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION",
        0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
        0xC000_0409 => "STATUS_STACK_BUFFER_OVERRUN",
        0xC000_0374 => "STATUS_HEAP_CORRUPTION",
        0xC000_0096 => "EXCEPTION_PRIV_INSTRUCTION",
        0xC000_0025 => "EXCEPTION_NONCONTINUABLE_EXCEPTION",
        0xC000_008C => "EXCEPTION_ARRAY_BOUNDS_EXCEEDED",
        0xC000_0006 => "EXCEPTION_IN_PAGE_ERROR",
        _ => "unknown",
    }
}

/// Crash on purpose (test mode only).
pub fn crash_on_purpose(kind: &str) -> ! {
    log::warn!(target: "f4x::crash", "crashing on purpose: {kind}");
    crate::logsink::sink().flush();
    match kind {
        "panic" => panic!("f4x debug.crash: deliberate panic"),
        "abort" => std::process::abort(),
        _ => {
            // SAFETY: deliberately invalid; this is the point.
            unsafe { std::ptr::write_volatile(8 as *mut u64, 0xDEAD) };
            unreachable!()
        }
    }
}
