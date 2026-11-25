//! Repository automation: `cargo xtask <task>`.

mod baseline;
mod catalog_check;
mod install;
mod lint;
mod profile;
mod spike_launch;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  provenance-lint   reject banned F4SE-source citations and unsourced provenance entries
  baseline          [--host f4se] [--profile NAME] [--headless] [--out FILE]  S5: record what the host does to a probe plugin
  install-game      [--game-dir DIR] [--reinstall]  use f4x instead of F4SE in a real install
                    (default F4X_GAME_DIR via Heroic; a Steam folder: its Fallout4Launcher.exe)
  uninstall-game    [--game-dir DIR]  undo install-game (manifests in ~/.local/share/f4x/install)
  compat            [--only surface,saveload,mcm,natives] [--hosts f4se,f4x] [--out DIR]
                    run the same scenarios under F4SE and f4x and diff them (tools/compat)
  catalog-check     [--natives FILE]  S3: check a native catalog against Fallout4.exe and the vanilla .pex
  profile           create NAME --host f4se|f4x [--plugin DLL]...   isolated game dir + prefix for a host
  spike-launch      [--headless] [--runs N] [--no-build]  launch the game with the runtime injected (S1)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("provenance-lint") => lint::provenance_lint(),
        Some("baseline") => baseline::run(&args[1..]),
        Some("catalog-check") => catalog_check::run(&args[1..]),
        Some("compat") => compat(&args[1..]),
        Some("install-game") => install::install(&args[1..]),
        Some("uninstall-game") => install::uninstall(&args[1..]),
        Some("profile") => profile::cli(&args[1..]),
        Some("spike-launch") => spike_launch::run(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Build what the scenarios need, then run `tools/compat/compat.py`.
fn compat(args: &[String]) -> Result<(), String> {
    let root = workspace_root();
    let run = |cmd: &mut std::process::Command| -> Result<(), String> {
        let status = cmd.current_dir(&root).status().map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err(format!("{cmd:?} failed")) }
    };
    run(std::process::Command::new("cargo").args([
        "xwin", "build", "--release", "--target", "x86_64-pc-windows-msvc", "-p", "f4x-loader", "-p", "f4x-runtime", "-p", "f4x-agent-f4se",
        "-p", "f4x-probe",
    ]))?;
    run(std::process::Command::new("cargo").args(["build", "-p", "f4x-cli", "-p", "f4x-pextool"]))?;
    run(std::process::Command::new("python3").arg("-I").arg(root.join("tools/compat/compat.py")).args(args))
}

/// The workspace root (the parent of `xtask/`).
pub fn workspace_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("xtask lives one level below the workspace root").to_path_buf()
}
