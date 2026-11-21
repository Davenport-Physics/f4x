//! Black-box probe plugin (spike S5).
//!
//! Loaded as an ordinary F4SE-ABI plugin by either host (real F4SE or f4x), it records
//! what the host does to a plugin to `%F4X_SESSION_DIR%\probe.jsonl`, one JSON event per
//! line, so the two hosts can be diffed:
//! - the `F4seInterface` values (versions, release index, plugin handle);
//! - which interface ids exist and their `interface_version`;
//! - every host message: kind, payload, thread, time, main-loop pump count, and the call
//!   stack as module + RVA (RVAs are recorded for game code only, never for the host DLL);
//! - when the Papyrus, Scaleform and task callbacks fire, and on which thread.
//!
//! `F4X_PROBE_EXIT_AFTER_S=<n>`: terminate the game `n` seconds after `GameLoaded`.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
mod probe;
