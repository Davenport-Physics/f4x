//! Script declarations for F4SE's script API, written into a profile.
//!
//! The user's own vanilla scripts are patched, and the scripts F4SE adds are generated
//! (see `f4x_pex::f4x_natives`). Vanilla scripts are read as the game would load them:
//! the loose copy in `Data/Scripts`, else the copy in `Fallout4 - Misc.ba2`. Output goes
//! only into a profile and is never committed.

use std::path::Path;

use f4x_pex::Pex;
use f4x_pex::f4x_natives::{create, inventory, load_constants, patch};

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

/// The constant property values shipped with f4x (`crates/f4x-pex/data/constants`).
fn constants_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../f4x-pex/data/constants")
}

fn write(out: &Path, script: &str, pex: &Pex) -> Result<(), String> {
    let path = out.join(format!("{script}.pex"));
    if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    std::fs::write(&path, pex.to_bytes().map_err(|e| format!("{script}: {e}"))?).map_err(|e| format!("{}: {e}", path.display()))
}

/// Patch each vanilla script F4SE extends, and create each script F4SE adds, into `out`
/// (replacing a symlink, never writing through one). Returns one line per script.
pub fn generate(data: &Path, out: &Path) -> Result<Vec<String>, String> {
    let archive = f4x_ba2::Archive::open(&data.join("Fallout4 - Misc.ba2")).ok();
    let constants = load_constants(&constants_dir())?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let (_, template) = source(data, "ScriptObject", archive.as_ref())?;
    let template = Pex::parse(&template).map_err(|e| format!("ScriptObject.pex: {e}"))?;
    let mut report = Vec::new();
    for decl in inventory() {
        let script = decl.script.as_str();
        let loose = data.join("Scripts").join(format!("{script}.pex"));
        if decl.new_script && std::fs::metadata(&loose).is_err() {
            let pex = create(&decl, &template, &constants)?;
            write(out, script, &pex)?;
            report.push(format!("{script}: created ({} functions, {} properties)", decl.functions.len(), pex.objects[0].properties.len()));
            continue;
        }
        // Vanilla scripts, and added scripts already present (F4SE's own or an earlier
        // run's): add only what is missing.
        let (from, bytes) = source(data, script, archive.as_ref())?;
        let mut pex = Pex::parse(&bytes).map_err(|e| format!("{from}: {e}"))?;
        let added = patch(&mut pex, &decl, &constants)?;
        if added.is_empty() {
            report.push(format!("{script}: already declares everything ({from})"));
            continue;
        }
        write(out, script, &pex)?;
        report.push(format!("{script}: added {} ({from})", added.len()));
    }
    Ok(report)
}
