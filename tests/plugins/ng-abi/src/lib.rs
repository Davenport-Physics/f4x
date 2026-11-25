//! Next-gen F4SE plugin ABI test (`f4x_ng_abi.dll`).
//!
//! A plugin shaped the way the next-gen CommonLibF4 fork (libxse/commonlibf4 at
//! 40a93f69, MIT) builds them: exported `F4SEPlugin_Version` data declaring what its
//! template declares (Address Library for 1.11.137+, layout-dependent on 1.11.137+,
//! compatible with 1.11.221), `F4SEPlugin_Preload` and `F4SEPlugin_Load`. It records what
//! its host gives it to `%F4X_SESSION_DIR%\ng-abi.jsonl`, one JSON event per line, each
//! tagged with this DLL's file name (so patched copies loaded side by side can be told
//! apart; see `tools/ng_abi.py`):
//! - `preload` / `load` / `query`: the interface values (versions, release index, plugin
//!   handle, save folder name), which interface ids exist and their versions,
//!   `GetPluginInfo` answers, a trampoline allocation, the thread and the call stack (game
//!   code as module + RVA; host DLLs by name only);
//! - every host message (`message`), tasks (`task_run` / `task_destroy`), the Papyrus and
//!   Scaleform callbacks (first call each);
//! - `checks`: what the plugin itself verified, once `GameLoaded` arrives.
//!
//! Copies whose file name contains `preload_false` / `load_false` return false from that
//! entry point.
//!
//! It runs under either host: f4x (`f4x launch --profile vanilla-f4x-ng --plugin <dll>`:
//! the runtime loads `F4X_PLUGINS` entries that only have F4SE entry points as F4SE
//! plugins) and real F4SE 0.7.x (`--host f4se --profile f4se-ng-baseline --plugin <dll>`).
//!
//! Features: `query` also exports `F4SEPlugin_Query`; `legacy` builds the old-gen shape
//! (`Query` + `Load` only).
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
mod plugin;
