//! `pex roundtrip`: parse + write every script and compare bytes.

use std::process::ExitCode;
use std::time::Instant;

use f4x_pex::Pex;

use crate::{CliResult, emit, open_archive};

enum Outcome {
    Identical,
    Parse(String),
    Write(String),
    /// First differing offset, original length, rewritten length.
    Mismatch {
        offset: usize,
        original: usize,
        rewritten: usize,
    },
}

fn check(bytes: &[u8]) -> Outcome {
    let pex = match Pex::parse(bytes) {
        Ok(pex) => pex,
        Err(e) => return Outcome::Parse(e.to_string()),
    };
    let rewritten = match pex.to_bytes() {
        Ok(out) => out,
        Err(e) => return Outcome::Write(e.to_string()),
    };
    if rewritten == bytes {
        return Outcome::Identical;
    }
    let offset =
        bytes.iter().zip(&rewritten).position(|(a, b)| a != b).unwrap_or(bytes.len().min(rewritten.len()));
    Outcome::Mismatch { offset, original: bytes.len(), rewritten: rewritten.len() }
}

pub fn run(args: &[&str]) -> CliResult {
    let mut show = 10usize;
    let mut archive_path = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        match arg {
            "--show" => {
                show = it.next().and_then(|n| n.parse().ok()).ok_or("--show needs a number")?;
            }
            "--ba2" => archive_path = Some(*it.next().ok_or("--ba2 needs an archive path")?),
            _ => files.push(arg),
        }
    }

    let started = Instant::now();
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some(path) = archive_path {
        let archive = open_archive(path)?;
        for entry in archive.entries() {
            if entry.name.to_ascii_lowercase().ends_with(".pex") {
                let bytes = archive.extract(entry).map_err(|e| format!("{}: {e}", entry.name))?;
                inputs.push((entry.name.clone(), bytes));
            }
        }
    }
    for file in files {
        let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        inputs.push((file.to_string(), bytes));
    }
    if inputs.is_empty() {
        return Err("no .pex inputs (give --ba2 <archive> or files)".into());
    }

    let (mut identical, mut parse_failed, mut write_failed, mut mismatched) = (0, 0, 0, 0);
    let mut shown = 0;
    let total_bytes: usize = inputs.iter().map(|(_, b)| b.len()).sum();
    for (name, bytes) in &inputs {
        let outcome = check(bytes);
        let message = match &outcome {
            Outcome::Identical => {
                identical += 1;
                continue;
            }
            Outcome::Parse(e) => {
                parse_failed += 1;
                format!("parse failed {e}")
            }
            Outcome::Write(e) => {
                write_failed += 1;
                format!("write failed: {e}")
            }
            Outcome::Mismatch { offset, original, rewritten } => {
                mismatched += 1;
                format!("differs at byte {offset:#x} (original {original} bytes, rewritten {rewritten})")
            }
        };
        if shown < show {
            emit(&format!("FAIL {name}: {message}\n"))?;
            shown += 1;
        }
    }
    let failed = parse_failed + write_failed + mismatched;
    emit(&format!(
        "{} .pex ({total_bytes} bytes): {identical} identical, {failed} failed \
         ({parse_failed} parse, {write_failed} write, {mismatched} mismatch) in {:.2?}\n",
        inputs.len(),
        started.elapsed()
    ))?;
    Ok(if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
