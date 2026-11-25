//! The `GetF4SEVersion` console command (F4SE changelog 0.1.0), plus f4x's own
//! `GetF4XVersion`.
//!
//! Black-box (F4SE 0.6.23): `GetF4SEVersion` takes no parameters and has no short name or
//! help text; it prints one line, `F4SE version: 0.6.23, release idx 25, runtime
//! 010A0A30`. F4SE 0.7.9 on 1.11.240 prints the same line with its own versions.
//!
//! f4x answers `GetF4SEVersion` with the same line for the version it reports to plugins
//! (0.6.23, the ABI f4x implements), then a second line saying that the host is f4x.
//! `GetF4XVersion` prints f4x's own version. Both go through f4x's console-command
//! support (`f4x_agent::console_cmds`), which takes free slots of the engine's table.

use std::ffi::c_void;

use f4x_abi::service::{ConsoleCall, ConsoleCommandSpec, Str};

/// The F4SE version f4x reports to plugins (`interfaces.rs`): 0.6.23, release index 25 on
/// 1.10.163; 0.7.9, release index 33 on next-gen (black-box: F4SE 0.7.9 prints
/// `F4SE version: 0.7.9, release idx 33, runtime 010B0F00`).
const F4SE_MAJOR: u32 = 0;
const F4SE_MINOR: u32 = f4x_engine::pick!(6, 7);
const F4SE_BETA: u32 = f4x_engine::pick!(23, 9);
const F4SE_RELEASE_INDEX: u32 = super::interfaces::RELEASE_INDEX;

/// The game's runtime version, as F4SE encodes it (`major << 24 | minor << 16 |
/// build << 4 | sub`): the build this f4x is built for (1.10.163, or 1.11.240 for NG).
const RUNTIME_VERSION: u32 =
    f4x_engine::pick!(f4x_abi::version::runtime::V1_10_163, f4x_abi::version::runtime::V1_11_240).0;

fn print(text: &str) {
    if let Err(error) = f4x_engine::console::print(text) {
        log::warn!(target: "f4x::console", "console print failed: {error}");
    }
}

/// F4SE's line (black-box, see `provenance/compat-features.jsonl`).
#[must_use]
pub(super) fn f4se_version_text() -> String {
    format!(
        "F4SE version: {F4SE_MAJOR}.{F4SE_MINOR}.{F4SE_BETA}, release idx {F4SE_RELEASE_INDEX}, runtime {RUNTIME_VERSION:08X}"
    )
}

unsafe extern "C" fn get_f4se_version(_call: *const ConsoleCall, _user: *mut c_void) -> bool {
    print(&f4se_version_text());
    print(&format!(
        "(host: f4x {}, reporting F4SE {F4SE_MAJOR}.{F4SE_MINOR}.{F4SE_BETA} to plugins)",
        env!("CARGO_PKG_VERSION")
    ));
    true
}

unsafe extern "C" fn get_f4x_version(_call: *const ConsoleCall, _user: *mut c_void) -> bool {
    print(&format!(
        "f4x version: {} (F4SE-compatible: {F4SE_MAJOR}.{F4SE_MINOR}.{F4SE_BETA}, runtime {RUNTIME_VERSION:08X})",
        env!("CARGO_PKG_VERSION")
    ));
    true
}

pub(super) fn install() {
    let commands = [
        (
            "GetF4SEVersion",
            "",
            "",
            get_f4se_version as unsafe extern "C" fn(*const ConsoleCall, *mut c_void) -> bool,
        ),
        ("GetF4XVersion", "", "Prints the f4x version", get_f4x_version),
    ];
    for (name, short, help, handler) in commands {
        let spec = ConsoleCommandSpec {
            name: Str::new(name),
            short_name: Str::new(short),
            help: Str::new(help),
            params: std::ptr::null(),
            param_count: 0,
            handler,
            user: std::ptr::null_mut(),
        };
        // SAFETY: a spec valid for the call; static strings and handler.
        if let Err(error) = unsafe { f4x_agent::console_cmds::register("f4x", &spec) } {
            log::warn!(target: "f4x::console", "{name}: {error}");
        }
    }
}
