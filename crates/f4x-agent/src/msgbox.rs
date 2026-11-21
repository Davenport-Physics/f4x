//! MessageBox interception (test mode).
//!
//! A modal dialog blocks a headless run forever. In test mode the agent patches
//! `user32!MessageBox{A,W}` and `MessageBoxEx{A,W}` to log the text as an error and
//! return the default button instead of showing anything.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};

use crate::hooks;

pub static INTERCEPTED: AtomicU64 = AtomicU64::new(0);

pub fn install() {
    let name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
    // SAFETY: plain module lookups.
    let user32 = unsafe {
        let module = GetModuleHandleW(name.as_ptr());
        if module.is_null() { LoadLibraryW(name.as_ptr()) } else { module }
    };
    if user32.is_null() {
        log::warn!(target: "f4x::msgbox", "user32 not available");
        return;
    }
    let targets: [(&std::ffi::CStr, usize); 4] = [
        (c"MessageBoxA", box_a as BoxA as usize),
        (c"MessageBoxW", box_w as BoxW as usize),
        (c"MessageBoxExA", box_ex_a as BoxExA as usize),
        (c"MessageBoxExW", box_ex_w as BoxExW as usize),
    ];
    for (symbol, hook) in targets {
        // SAFETY: valid module and NUL-terminated name.
        if let Some(function) = unsafe { GetProcAddress(user32, symbol.as_ptr().cast()) } {
            // SAFETY: the export is a function longer than 14 bytes, never called again.
            if let Err(error) = unsafe { hooks::write_abs_jmp(function as *mut u8, hook) } {
                log::warn!(target: "f4x::msgbox", "{symbol:?}: {error}");
            }
        }
    }
    log::debug!(target: "f4x::msgbox", "MessageBox interception on (test mode)");
}

type BoxA = unsafe extern "system" fn(*mut c_void, *const u8, *const u8, u32) -> i32;
type BoxW = unsafe extern "system" fn(*mut c_void, *const u16, *const u16, u32) -> i32;
type BoxExA = unsafe extern "system" fn(*mut c_void, *const u8, *const u8, u32, u16) -> i32;
type BoxExW = unsafe extern "system" fn(*mut c_void, *const u16, *const u16, u32, u16) -> i32;

/// The answer a user would most safely give: OK, Cancel, Abort or No.
fn default_answer(style: u32) -> i32 {
    match style & 0xF {
        0 => 1,     // MB_OK -> IDOK
        2 => 3,     // MB_ABORTRETRYIGNORE -> IDABORT
        4 => 7,     // MB_YESNO -> IDNO
        _ => 2,     // OKCANCEL, YESNOCANCEL, RETRYCANCEL, CANCELTRYCONTINUE -> IDCANCEL
    }
}

unsafe fn ansi(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: NUL-terminated string from the caller.
    unsafe { std::ffi::CStr::from_ptr(p.cast()).to_string_lossy().into_owned() }
}

unsafe fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: NUL-terminated string from the caller.
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

fn intercepted(text: String, caption: String, style: u32) -> i32 {
    INTERCEPTED.fetch_add(1, Relaxed);
    let answer = default_answer(style);
    crate::logsink::sink().emit(
        1,
        "f4x::msgbox",
        format!("MessageBox suppressed: [{caption}] {text}"),
        serde_json::json!({"caption": caption, "text": text, "style": style, "answer": answer}),
        None,
    );
    crate::logsink::sink().flush();
    answer
}

unsafe extern "system" fn box_a(_: *mut c_void, text: *const u8, caption: *const u8, style: u32) -> i32 {
    // SAFETY: caller-provided strings.
    unsafe { intercepted(ansi(text), ansi(caption), style) }
}
unsafe extern "system" fn box_w(_: *mut c_void, text: *const u16, caption: *const u16, style: u32) -> i32 {
    // SAFETY: caller-provided strings.
    unsafe { intercepted(wide(text), wide(caption), style) }
}
unsafe extern "system" fn box_ex_a(_: *mut c_void, text: *const u8, caption: *const u8, style: u32, _: u16) -> i32 {
    // SAFETY: caller-provided strings.
    unsafe { intercepted(ansi(text), ansi(caption), style) }
}
unsafe extern "system" fn box_ex_w(_: *mut c_void, text: *const u16, caption: *const u16, style: u32, _: u16) -> i32 {
    // SAFETY: caller-provided strings.
    unsafe { intercepted(wide(text), wide(caption), style) }
}
