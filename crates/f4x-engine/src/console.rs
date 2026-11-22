//! The game console.

use std::ffi::CString;

use f4x_engine_sys::og_1_10_163_addresses::CONSOLE_EXECUTE_COMMAND;

/// Run a console command, exactly as if typed into the console (`coc QASmoke`,
/// `save name`, ...). Main thread only.
pub fn execute(command: &str) -> Result<(), String> {
    let function = crate::address::resolve(&CONSOLE_EXECUTE_COMMAND)
        .ok_or("Console::ExecuteCommand not found in this build")?;
    let command = CString::new(command).map_err(|_| "command contains a NUL byte")?;
    type ExecuteCommand = unsafe extern "C" fn(*const std::ffi::c_char);
    // SAFETY: a static engine function taking a C string (CommonLibF4 Console::ExecuteCommand).
    unsafe { std::mem::transmute::<usize, ExecuteCommand>(function)(command.as_ptr()) };
    Ok(())
}
