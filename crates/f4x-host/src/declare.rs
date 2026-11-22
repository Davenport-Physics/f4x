//! Script declarations for f4x's F4SE-compatible natives, written into a profile.
//!
//! The user's own vanilla scripts are patched (see `f4x_pex::f4x_natives`). Each one is
//! read as the game would load it: the loose copy in `Data/Scripts`, else the copy in
//! `Fallout4 - Misc.ba2`. Patched copies are written only into a profile and never
//! committed.

use std::path::Path;

use f4x_pex::Pex;
use f4x_pex::f4x_natives::{patch, scripts};

/// The script as the game would load it: a loose file in `Data/Scripts`, else the copy
/// in `Fallout4 - Misc.ba2`.
fn source(data: &Path, script: &str, archive: Option<&f4x_ba2::Archive>) -> Result<(String, Vec<u8>), String> {
    let loose = data.join("Scripts").join(format!("{script}.pex"));
    if let Ok(bytes) = std::fs::read(&loose) {
        return Ok((loose.display().to_string(), bytes));
    }
    let archive = archive.ok_or("Fallout4 - Misc.ba2 not found")?;
    let wanted = format!("scripts/{}.pex", script.to_ascii_lowercase());
    let entry = archive
        .entries()
        .iter()
        .find(|e| e.name.replace('\\', "/").eq_ignore_ascii_case(&wanted))
        .ok_or_else(|| format!("{wanted} not in Fallout4 - Misc.ba2"))?;
    let bytes = archive.extract(entry).map_err(|e| e.to_string())?;
    Ok((format!("Fallout4 - Misc.ba2:{}", entry.name), bytes))
}

/// Patch each script that lacks declarations and write it into `out` (replacing a
/// symlink, never writing through one). Returns one line per script.
pub fn generate(data: &Path, out: &Path) -> Result<Vec<String>, String> {
    let archive = f4x_ba2::Archive::open(&data.join("Fallout4 - Misc.ba2")).ok();
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut report = Vec::new();
    for script in scripts() {
        let (from, bytes) = source(data, script, archive.as_ref())?;
        let mut pex = Pex::parse(&bytes).map_err(|e| format!("{from}: {e}"))?;
        let added = patch(&mut pex, script)?;
        if added.is_empty() {
            report.push(format!("{script}: already declares everything ({from})"));
            continue;
        }
        let path = out.join(format!("{script}.pex"));
        if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        std::fs::write(&path, pex.to_bytes().map_err(|e| format!("{script}: {e}"))?).map_err(|e| format!("{}: {e}", path.display()))?;
        report.push(format!("{script}: added {} ({from})", added.join(", ")));
    }
    Ok(report)
}
