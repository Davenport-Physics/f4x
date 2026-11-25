//! `f4x-insngrep`: find instructions in a `Fallout4.exe` whose Intel-syntax text contains
//! every given substring (case-insensitive), across all functions of the index.
//!
//! ```text
//! f4x-insngrep <exe> <substring>... [--max N]
//! ```
//! Prints `rva  (in function)  instruction`. Read-only: used to find the code that uses a
//! field displacement or flag constant (e.g. `+43Ch]` and `80000h`) in either build.

use std::process::ExitCode;

use f4x_re::index::Binary;
use iced_x86::{Decoder, DecoderOptions, Formatter as _, IntelFormatter};

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: f4x-insngrep <exe> <substring>... [--max N]";
    let mut max = 200usize;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--max" {
            max = it.next().and_then(|n| n.parse().ok()).ok_or(usage)?;
        } else {
            rest.push(a.clone());
        }
    }
    if rest.len() < 2 {
        return Err(usage.into());
    }
    let bytes = std::fs::read(&rest[0]).map_err(|e| format!("{}: {e}", rest[0]))?;
    let b = Binary::load(&bytes)?;
    let needles: Vec<String> = rest[1..].iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut formatter = IntelFormatter::new();
    let mut found = 0;
    for f in &b.funcs {
        for &(begin, end) in &f.chunks {
            let code = b.code(begin, (end - begin) as usize);
            let mut decoder = Decoder::with_ip(64, code, b.base + u64::from(begin), DecoderOptions::NONE);
            for instruction in decoder.iter() {
                let mut text = String::new();
                formatter.format(&instruction, &mut text);
                let lower = text.to_ascii_lowercase();
                if needles.iter().all(|n| lower.contains(n.as_str())) {
                    println!("{:#09x}  (in {:#x})  {text}", instruction.ip() - b.base, f.start);
                    found += 1;
                    if found >= max {
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
