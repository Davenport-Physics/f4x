//! The in-game f4x agent.
//!
//! The same agent runs in both hosts:
//! - inside `f4x_runtime.dll` (host `f4x`);
//! - as the F4SE plugin `f4x_agent.dll` (host `f4se`), so tests and observability work
//!   unchanged under real F4SE for differential runs.
//!
//! It provides:
//! - [`server`]: the JSON-RPC control server (`f4x-proto`), opt-in via `F4X_CONTROL=1`;
//! - [`logsink`]: structured logs to `trace.jsonl`, a flight-recorder ring buffer, live
//!   subscriptions; also the `log` crate backend;
//! - [`pump`]: the main-thread pump (engine-safe work queue) and [`tests`], the in-game
//!   test runner driven by it;
//! - [`service`]: the C-ABI service table plugins use (`f4x_abi::service`);
//! - Windows only: crash capture, MessageBox interception (test mode), the D3D11 frame
//!   hook + screenshots, and engine observers (menus, native catalog).

pub mod clock;
pub mod logsink;
pub mod methods;
pub mod pump;
pub mod server;
pub mod service;
pub mod session;
pub mod tests;

#[cfg(windows)]
mod attach;
#[cfg(windows)]
pub mod console_cmds;
#[cfg(windows)]
pub mod crash;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
mod engine_rpc;
#[cfg(windows)]
mod engine_strings;
#[cfg(windows)]
pub mod hooks;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod msgbox;
#[cfg(windows)]
pub mod observe;
#[cfg(windows)]
pub mod patches;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
pub use attach::{AttachOptions, attach};

/// Plugins the host has loaded, for `state`.
pub static PLUGINS: std::sync::Mutex<Vec<f4x_proto::PluginState>> = std::sync::Mutex::new(Vec::new());

/// Whether the launcher asked for test mode (`F4X_TEST_MODE=1`): MessageBoxes are
/// auto-dismissed and `debug.*` methods are served.
pub fn test_mode() -> bool {
    std::env::var_os("F4X_TEST_MODE").is_some_and(|v| v == "1")
}
