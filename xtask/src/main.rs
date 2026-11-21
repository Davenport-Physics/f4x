//! Repository automation: `cargo xtask <task>`.

mod baseline;
mod catalog_check;
mod lint;
mod profile;
mod spike_launch;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  provenance-lint   reject banned F4SE-source citations and unsourced provenance entries
  baseline          [--host f4se] [--profile NAME] [--headless] [--out FILE]  S5: record what the host does to a probe plugin
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

/// The workspace root (the parent of `xtask/`).
pub fn workspace_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("xtask lives one level below the workspace root").to_path_buf()
}
