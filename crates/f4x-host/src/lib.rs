//! Host-side f4x (Linux): the game environment, test profiles, launching the game
//! under umu/Proton, session supervision and the control-protocol client.
//!
//! | Module | What |
//! |---|---|
//! | [`env`] | Paths to the game, Proton, umu and the test prefix; the prefix's mute |
//! | [`profile`] | Isolated game dir + prefix per host; the F4SE archive cache; plugin links |
//! | [`launch`] | `spawn` through umu/gamescope, the launch lock, `GameGuard` |
//! | [`contract`] | What each host gets: program, arguments, `F4X_*` environment |
//! | [`supervisor`] | The detached process that owns a running session |
//! | [`session`] | Session directories: endpoint, liveness, crashes, logs, status, quit |
//! | [`client`] | Blocking JSON-RPC client for the in-game agent |
//! | [`junit`] | JUnit XML for test runs |
//! | [`signals`] | SIGINT/SIGTERM as a flag |
//! | `mock` | (feature `mock`) an in-process agent for tests |

pub mod client;
pub mod contract;
pub mod env;
pub mod junit;
pub mod launch;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod profile;
pub mod session;
pub mod signals;
pub mod supervisor;

use std::path::{Path, PathBuf};

pub use client::Client;
pub use env::{GameEnv, mount_root, wine_path};
pub use launch::{GameGuard, Launch, build_windows, launch_lock, new_session, session_processes, spawn};
pub use profile::{Host, Profile, data_home};
pub use session::{SessionDir, SessionEndpoint, wait_for_session};

/// The f4x workspace: `$F4X_WORKSPACE`, else the checkout this crate was built from.
/// Windows build output and session directories live under its `target/`.
pub fn workspace_root() -> PathBuf {
    if let Some(root) = std::env::var_os("F4X_WORKSPACE") {
        return PathBuf::from(root);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).expect("f4x-host lives at crates/f4x-host").to_path_buf()
}

/// Where `cargo win` puts the Windows binaries.
pub fn windows_out(root: &Path) -> PathBuf {
    root.join("target/x86_64-pc-windows-msvc/release")
}

/// A fresh, empty scratch directory for a unit test.
#[cfg(test)]
pub(crate) fn test_dir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir()
        .join(format!("f4x-host-test-{}-{label}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
