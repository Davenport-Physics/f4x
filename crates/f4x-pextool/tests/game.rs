//! Spike S4 exit check against the user's game files. Runs only when `F4X_GAME_DIR` names
//! a Fallout 4 install; the archive is read in place and nothing is written.
//!
//! ```sh
//! F4X_GAME_DIR="/path/to/Fallout 4" cargo test -p f4x-pextool --release --test game -- --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

#[test]
fn vanilla_misc_ba2_round_trips() {
    let Some(game) = std::env::var_os("F4X_GAME_DIR") else {
        eprintln!("skipped: set F4X_GAME_DIR to the Fallout 4 install to run");
        return;
    };
    let archive = PathBuf::from(game).join("Data").join("Fallout4 - Misc.ba2");
    assert!(archive.is_file(), "{} not found", archive.display());

    let out = Command::new(env!("CARGO_BIN_EXE_f4x-pextool"))
        .args(["pex", "roundtrip", "--ba2"])
        .arg(&archive)
        .output()
        .expect("run f4x-pextool");
    let text = String::from_utf8_lossy(&out.stdout);
    eprint!("{text}");
    assert!(out.status.success(), "round-trip failed:\n{text}");
    // e.g. "7875 .pex (10460796 bytes): 7875 identical, 0 failed (...)"
    let summary = text.lines().last().unwrap_or_default();
    let total: usize = summary.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
    assert!(total > 7000, "expected ~7,875 scripts, saw {total}: {summary}");
    assert!(summary.contains(&format!("{total} identical, 0 failed")), "{summary}");
}
