//! `pex declare`: see `f4x_pex::f4x_natives` and `f4x_host::declare`.

use std::path::PathBuf;
use std::process::ExitCode;

use f4x_pex::f4x_natives::{inventory_for, verify_for};

pub fn run(args: &[&str]) -> Result<ExitCode, String> {
    let value = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    // `--ng`: the next-gen (F4SE 0.7.9) inventory.
    let ng = args.contains(&"--ng");
    if let Some(dir) = value("--verify") {
        let problems = verify_for(&dir, ng)?;
        for p in &problems {
            println!("MISMATCH {p}");
        }
        let count: usize = inventory_for(ng).iter().map(|s| s.functions.len()).sum();
        println!("{count} function declaration(s) checked against {}: {} mismatch(es)", dir.display(), problems.len());
        return Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE });
    }
    let game = value("--game").ok_or("--game <Fallout 4 dir> is required")?;
    let out = value("--out").ok_or("--out <dir to write patched scripts into> is required")?;
    let generation = if ng { f4x_host::Generation::Ng } else { f4x_host::Generation::Og };
    for line in f4x_host::declare::generate_for(&game.join("Data"), &out, generation)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}
