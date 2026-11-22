//! The game console: run commands, print, and the console command table.
//!
//! Layouts from CommonLibF4 `RE/Bethesda/Script.h` (`SCRIPT_PARAMETER` 0x10,
//! `SCRIPT_FUNCTION` 0x50, `SCRIPT_PARAM_TYPE`) and `Console.h` (`ConsoleLog`, its
//! `buffer` at +0x08). Functions and globals resolve through signature recipes.

use std::ffi::{CString, c_char, c_void};

use f4x_engine_sys::og_1_10_163_addresses::{
    CONSOLE_EXECUTE_COMMAND, CONSOLE_FUNCTIONS, CONSOLE_LOG_ADD_STRING, CONSOLE_LOG_SINGLETON, SCRIPT_PARSE_PARAMETERS,
};

use crate::address::resolve;

/// Run a console command, exactly as if typed into the console (`coc QASmoke`,
/// `save name`, ...). Main thread only.
pub fn execute(command: &str) -> Result<(), String> {
    let function = resolve(&CONSOLE_EXECUTE_COMMAND).ok_or("Console::ExecuteCommand not found in this build")?;
    let command = CString::new(command).map_err(|_| "command contains a NUL byte")?;
    type ExecuteCommand = unsafe extern "C" fn(*const c_char);
    // SAFETY: a static engine function taking a C string (CommonLibF4 Console::ExecuteCommand).
    unsafe { std::mem::transmute::<usize, ExecuteCommand>(function)(command.as_ptr()) };
    Ok(())
}

fn console_log() -> Option<*mut c_void> {
    let global = resolve(&CONSOLE_LOG_SINGLETON)?;
    // SAFETY: the ConsoleLog* global.
    let log = unsafe { *(global as *const *mut c_void) };
    (!log.is_null()).then_some(log)
}

/// Print a line to the console (`ConsoleLog::AddString`). Main thread only.
pub fn print(text: &str) -> Result<(), String> {
    let add = resolve(&CONSOLE_LOG_ADD_STRING).ok_or("ConsoleLog::AddString not found")?;
    let log = console_log().ok_or("no console log yet")?;
    let mut line = text.replace('\0', " ");
    if !line.ends_with('\n') {
        line.push('\n');
    }
    let line = CString::new(line).unwrap_or_default();
    type AddString = unsafe extern "C" fn(*mut c_void, *const c_char);
    // SAFETY: a member function of the live ConsoleLog.
    unsafe { std::mem::transmute::<usize, AddString>(add)(log, line.as_ptr()) };
    Ok(())
}

/// The console's text so far (`ConsoleLog::buffer`).
pub fn text() -> Option<String> {
    let log = console_log()?;
    // SAFETY: BSStringT<char> at +0x08: data pointer first.
    let data: *const c_char = unsafe { *(log as *const u8).add(8).cast::<*const c_char>() };
    if data.is_null() {
        return Some(String::new());
    }
    // SAFETY: NUL-terminated buffer owned by the console log.
    Some(unsafe { std::ffi::CStr::from_ptr(data) }.to_string_lossy().into_owned())
}

/// `SCRIPT_PARAMETER`.
#[repr(C)]
pub struct ScriptParameter {
    pub name: *const c_char,
    pub kind: i32,
    pub optional: bool,
}

/// `SCRIPT_FUNCTION`.
#[repr(C)]
pub struct ScriptFunction {
    pub name: *const c_char,
    pub short_name: *const c_char,
    pub output: i32,
    pub help: *const c_char,
    pub reference_function: bool,
    pub param_count: u16,
    pub params: *const ScriptParameter,
    pub execute: Option<ExecuteFunction>,
    pub compile: *const c_void,
    pub condition: *const c_void,
    pub editor_filter: bool,
    pub invalidates_cell_list: bool,
}

const _: () = {
    assert!(std::mem::size_of::<ScriptParameter>() == 0x10);
    assert!(std::mem::size_of::<ScriptFunction>() == 0x50);
    assert!(std::mem::offset_of!(ScriptFunction, param_count) == 0x22);
    assert!(std::mem::offset_of!(ScriptFunction, execute) == 0x30);
};

/// `SCRIPT_FUNCTION::ExecuteFunction_t`.
pub type ExecuteFunction = unsafe extern "C" fn(
    params: *const ScriptParameter,
    compiled: *const u8,
    reference: *mut c_void,
    container: *mut c_void,
    script: *mut c_void,
    locals: *mut c_void,
    result: *mut f32,
    offset: *mut u32,
) -> bool;

pub const CONSOLE_FUNCTION_COUNT: usize = 522;

/// The console command table (`SCRIPT_FUNCTION[522]`).
pub fn console_functions() -> Option<*mut ScriptFunction> {
    resolve(&CONSOLE_FUNCTIONS).map(|p| p as *mut ScriptFunction)
}

/// Whether a table entry is a real command whose handler is a release-build stub
/// (`mov al,1; ret` / `xor al,al; ret`), i.e. a slot that can be reused. The table's
/// terminator (no name, no handler, no compile function) is never free.
pub fn is_free_slot(entry: &ScriptFunction) -> bool {
    // SAFETY: entry names are static C strings or null.
    let named = !entry.name.is_null() && unsafe { *entry.name } != 0;
    named && !entry.compile.is_null() && is_noop_handler(entry.execute)
}

/// Whether an execute function is a release-build stub (`mov al,1; ret` / `xor al,al; ret`).
pub fn is_noop_handler(handler: Option<ExecuteFunction>) -> bool {
    let Some(handler) = handler else { return false };
    let p = handler as usize;
    if !crate::mem::readable(p, 3) {
        return false;
    }
    // SAFETY: readable code bytes.
    let code = unsafe { std::slice::from_raw_parts(p as *const u8, 3) };
    code == [0xB0, 0x01, 0xC3] || code == [0x32, 0xC0, 0xC3]
}

/// `Script::ParseParameters`: decode compiled console arguments into typed outputs
/// (`i32*`, `f32*`, `char*` buffers, form pointers), one per parameter.
pub type ParseParameters = unsafe extern "C" fn(
    *const ScriptParameter,
    *const u8,
    *mut u32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    ...
) -> bool;

pub fn parse_parameters() -> Option<ParseParameters> {
    // SAFETY: the resolved engine function has this (variadic) signature.
    resolve(&SCRIPT_PARSE_PARAMETERS).map(|p| unsafe { std::mem::transmute::<usize, ParseParameters>(p) })
}

/// `SCRIPT_PARAM_TYPE` values f4x exposes (CommonLibF4 order).
pub mod param {
    pub const STRING: u32 = 0;
    pub const INT: u32 = 1;
    pub const FLOAT: u32 = 2;
    pub const INVENTORY_OBJECT: u32 = 3;
    pub const OBJECT_REF: u32 = 4;
    pub const ACTOR_VALUE: u32 = 5;
    pub const ACTOR: u32 = 6;
    pub const CELL: u32 = 9;
    pub const QUEST: u32 = 14;
    pub const RACE: u32 = 15;
    pub const FACTION: u32 = 17;
    pub const GLOBAL: u32 = 19;
    pub const OBJECT: u32 = 21;
    pub const ACTOR_BASE: u32 = 25;
    pub const MAGIC_EFFECT: u32 = 31;
    pub const WEATHER: u32 = 33;
    pub const NPC: u32 = 34;
    pub const FORM_LIST: u32 = 37;
    pub const PERK: u32 = 39;
    pub const ENCOUNTER_ZONE: u32 = 50;
    pub const MESSAGE: u32 = 52;
    pub const KEYWORD: u32 = 59;
    pub const LOCATION: u32 = 61;
    pub const FORM: u32 = 62;

    /// Kinds whose output is a number or text rather than a form pointer.
    #[must_use]
    pub fn is_form(kind: u32) -> bool {
        !matches!(kind, STRING | INT | FLOAT)
    }
}
