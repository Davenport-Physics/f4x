//! `pex declare`: see `f4x_pex::f4x_natives` and `f4x_host::declare`.

use std::path::PathBuf;
use std::process::ExitCode;

use f4x_pex::f4x_natives::{DECLARATIONS, verify};

pub fn run(args: &[&str]) -> Result<ExitCode, String> {
    let value = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    if let Some(dir) = value("--verify") {
        let problems = verify(&dir)?;
        for p in &problems {
            println!("MISMATCH {p}");
        }
        println!("{} declaration(s) checked against {}: {} mismatch(es)", DECLARATIONS.len(), dir.display(), problems.len());
        return Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE });
    }
    let game = value("--game").ok_or("--game <Fallout 4 dir> is required")?;
    let out = value("--out").ok_or("--out <dir to write patched scripts into> is required")?;
    for line in f4x_host::declare::generate(&game.join("Data"), &out)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}
