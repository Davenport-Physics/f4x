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
pub mod declare;
pub mod env;
pub mod junit;
pub mod launch;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod profile;
pub mod session;
pub mod signals;
pub mod supervisor;
pub mod symbolize;

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

pub use f4x_engine_sys::Generation;

/// Where `cargo win` puts the Windows binaries (the 1.10.163 build).
pub fn windows_out(root: &Path) -> PathBuf {
    root.join("target/x86_64-pc-windows-msvc/release")
}

/// The Windows binaries for a game generation: next-gen builds (cargo feature `ng`) go
/// to their own target dir, `target/ng`.
pub fn windows_out_for(root: &Path, generation: Generation) -> PathBuf {
    match generation {
        Generation::Og => windows_out(root),
        Generation::Ng => root.join("target/ng/x86_64-pc-windows-msvc/release"),
    }
}

/// Which generation the `Fallout4.exe` in `game_dir` is: a known build by PE timestamp,
/// else by its C runtime (1.10.163 and older link MSVCR110, next-gen the UCRT).
pub fn exe_generation(game_dir: &Path) -> Result<Generation, String> {
    let path = game_dir.join("Fallout4.exe");
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pe = f4x_pe::PeFile::parse(&bytes).map_err(|e| format!("{}: {}", path.display(), e.0))?;
    if let Some(build) = f4x_engine_sys::BUILDS.iter().find(|b| b.timestamp == pe.timestamp()) {
        return Ok(build.generation);
    }
    let ucrt = pe.imports().iter().any(|i| i.dll.eq_ignore_ascii_case("api-ms-win-crt-runtime-l1-1-0.dll"));
    Ok(if ucrt { Generation::Ng } else { Generation::Og })
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
