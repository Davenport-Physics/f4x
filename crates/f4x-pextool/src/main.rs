//! `f4x-pextool`: BA2 listing/extraction and `.pex` inspection.
//!
//! ```text
//! f4x-pextool ba2 list <archive>
//! f4x-pextool ba2 extract <archive> <glob> <outdir>
//! f4x-pextool pex dump [--asm] <file.pex | --ba2 <archive> <entry>>
//! f4x-pextool pex roundtrip [--show N] (--ba2 <archive> | <file.pex>...)
//! ```

mod dump;
mod roundtrip;
mod usage;

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use f4x_ba2::{Archive, EntryData};

const USAGE: &str = "\
usage:
  f4x-pextool ba2 list <archive>
  f4x-pextool ba2 extract <archive> <glob> <outdir>
      <glob> matches entry paths case-insensitively with `/` separators; `*` also
      crosses directories (e.g. `scripts/*.pex`, `*.txt`).
  f4x-pextool pex dump [--asm] <file.pex>
  f4x-pextool pex dump [--asm] --ba2 <archive> <entry>
      human-readable declarations; --asm adds each function's instructions.
  f4x-pextool pex roundtrip [--show N] --ba2 <archive>
  f4x-pextool pex roundtrip [--show N] <file.pex>...
      parse and re-serialize every .pex in memory and compare byte-for-byte;
      exits 1 on any failure, listing the first N (default 10).
";

type CliResult = Result<ExitCode, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["ba2", "list", archive] => ba2_list(archive),
        ["ba2", "extract", archive, pattern, outdir] => ba2_extract(archive, pattern, outdir),
        ["pex", "dump", rest @ ..] => dump::run(rest),
        ["pex", "roundtrip", rest @ ..] => roundtrip::run(rest),
        ["pex", "f4se-usage", rest @ ..] => usage::run(rest),
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Writes to stdout; a closed pipe (e.g. `| head`) ends output quietly instead of panicking.
fn emit(text: &str) -> Result<(), String> {
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        result => result.map_err(|e| format!("stdout: {e}")),
    }
}

fn open_archive(path: &str) -> Result<Archive, String> {
    Archive::open(path).map_err(|e| format!("{path}: {e}"))
}

fn ba2_list(path: &str) -> CliResult {
    let archive = open_archive(path)?;
    let header = archive.header();
    let tag = header.kind.tag();
    let mut out = format!(
        "{path}: BTDX v{} {}, {} entries\n",
        header.version,
        String::from_utf8_lossy(&tag),
        archive.entries().len()
    );
    for entry in archive.entries() {
        let z = if entry.is_compressed() { 'z' } else { '-' };
        let detail = match &entry.data {
            EntryData::General(_) => String::new(),
            EntryData::Texture(t) => format!(
                "  [{}x{} mips={} dxgi={} chunks={}{}]",
                t.width,
                t.height,
                t.mip_count,
                t.dxgi_format,
                t.chunks.len(),
                if t.flags & 1 != 0 { " cube" } else { "" }
            ),
        };
        out.push_str(&format!(
            "{:>10} {:>10} {z} {}{detail}\n",
            entry.size(),
            entry.stored_size(),
            entry.name
        ));
    }
    emit(&out)?;
    Ok(ExitCode::SUCCESS)
}

fn ba2_extract(path: &str, pattern: &str, outdir: &str) -> CliResult {
    let archive = open_archive(path)?;
    let pattern = glob::Pattern::new(&pattern.replace('\\', "/")).map_err(|e| format!("bad glob: {e}"))?;
    let options = glob::MatchOptions {
        case_sensitive: false,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let (mut written, mut skipped) = (0usize, 0usize);
    for entry in archive.entries() {
        let name = entry.name.replace('\\', "/");
        if !pattern.matches_with(&name, options) {
            continue;
        }
        if matches!(entry.data, EntryData::Texture(_)) {
            eprintln!("skip {name}: DX10 texture extraction is not supported");
            skipped += 1;
            continue;
        }
        let Some(target) = safe_join(Path::new(outdir), &name) else {
            eprintln!("skip {name}: not a safe relative path");
            skipped += 1;
            continue;
        };
        let bytes = archive.extract(entry).map_err(|e| format!("{name}: {e}"))?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        written += 1;
    }
    let skipped = if skipped > 0 { format!(", skipped {skipped}") } else { String::new() };
    emit(&format!("extracted {written} file(s) to {outdir}{skipped}\n"))?;
    Ok(ExitCode::SUCCESS)
}

/// Joins an archive path under `root`, refusing anything that could escape it.
fn safe_join(root: &Path, name: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    let mut any = false;
    for part in name.split('/') {
        let component = Path::new(part).components().collect::<Vec<_>>();
        match component.as_slice() {
            [Component::Normal(c)] if !part.contains(':') => {
                out.push(c);
                any = true;
            }
            _ => return None,
        }
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_escapes() {
        let root = Path::new("out");
        assert_eq!(safe_join(root, "scripts/Form.pex"), Some(PathBuf::from("out/scripts/Form.pex")));
        for bad in ["../x", "a/../../x", "/etc/passwd", "c:/x", "a//b", "", "a/./b"] {
            assert_eq!(safe_join(root, bad), None, "{bad}");
        }
    }
}
