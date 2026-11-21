//! Host-side symbolization of crash frames.
//!
//! Frames in f4x's own DLLs and in test plugins are resolved with `llvm-symbolizer`
//! against the PDBs cargo writes next to them (`debug = "line-tables-only"` in release).
//! Frames in the game and system DLLs are left as `module+rva`.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// Directories searched for DLLs (and their PDBs): the workspace's Windows build output.
pub fn default_search_dirs(workspace: &Path) -> Vec<PathBuf> {
    ["release", "debug"].iter().map(|p| workspace.join("target/x86_64-pc-windows-msvc").join(p)).filter(|d| d.is_dir()).collect()
}

fn find_module(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                let path = entry.path();
                if path.with_extension("pdb").is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn image_base(dll: &Path) -> Option<u64> {
    let bytes = std::fs::read(dll).ok()?;
    let pe = u32::from_le_bytes(bytes.get(0x3C..0x40)?.try_into().ok()?) as usize;
    Some(u64::from_le_bytes(bytes.get(pe + 24 + 24..pe + 24 + 32)?.try_into().ok()?))
}

/// `function (file:line)` for each RVA, via one `llvm-symbolizer` run.
fn symbolize_module(dll: &Path, rvas: &[u64], workspace: &Path) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let Some(base) = image_base(dll) else { return out };
    let Ok(mut child) = Command::new("llvm-symbolizer")
        .arg(format!("--obj={}", dll.display()))
        .args(["--output-style=LLVM", "--no-inlines"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return out;
    };
    if let Some(mut stdin) = child.stdin.take() {
        for rva in rvas {
            let _ = writeln!(stdin, "{:#x}", base + rva);
        }
    }
    let Ok(output) = child.wait_with_output() else { return out };
    let text = String::from_utf8_lossy(&output.stdout);
    // Blocks of "function\nfile:line:col\n" separated by blank lines, one per input.
    let blocks: Vec<&str> = text.split("\n\n").filter(|b| !b.trim().is_empty()).collect();
    let prefix = workspace.to_string_lossy().replace('\\', "/");
    for (rva, block) in rvas.iter().zip(blocks) {
        let mut lines = block.lines();
        let (Some(function), location) = (lines.next(), lines.next().unwrap_or("")) else { continue };
        if function == "??" {
            continue;
        }
        let location = location.replace('\\', "/");
        let location = location.strip_prefix(&format!("{prefix}/")).unwrap_or(&location);
        let location = location.rsplit_once(':').map_or(location, |(file_line, _col)| file_line);
        out.insert(*rva, format!("{function} ({location})"));
    }
    out
}

/// Fill `symbol` on frames (in `address` and `frames`) that can be resolved.
pub fn symbolize_report(report: &mut Value, dirs: &[PathBuf], workspace: &Path) {
    let mut wanted: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut collect = |frame: &Value| {
        if frame["symbol"].is_string() {
            return;
        }
        if let (Some(module), Some(rva)) = (frame["module"].as_str(), frame["rva"].as_u64()) {
            wanted.entry(module.to_owned()).or_default().push(rva);
        }
    };
    collect(&report["address"]);
    for frame in report["frames"].as_array().into_iter().flatten() {
        collect(frame);
    }
    let mut resolved: HashMap<(String, u64), String> = HashMap::new();
    for (module, mut rvas) in wanted {
        let Some(dll) = find_module(&module, dirs) else { continue };
        rvas.sort_unstable();
        rvas.dedup();
        for (rva, symbol) in symbolize_module(&dll, &rvas, workspace) {
            resolved.insert((module.to_ascii_lowercase(), rva), format!("{module}!{symbol}"));
        }
    }
    let apply = |frame: &mut Value| {
        if let (Some(module), Some(rva)) = (frame["module"].as_str(), frame["rva"].as_u64())
            && let Some(symbol) = resolved.get(&(module.to_ascii_lowercase(), rva))
        {
            frame["symbol"] = Value::String(symbol.clone());
        }
    };
    apply(&mut report["address"]);
    for frame in report["frames"].as_array_mut().into_iter().flatten() {
        apply(frame);
    }
}
