//! `provenance-lint`: keeps the clean-room rules from CLAUDE.md enforceable.
//!
//! 1. No tracked text file may cite F4SE source identifiers or paths.
//! 2. Every line of `provenance/*.jsonl` must be a JSON object with an `id` and a
//!    non-empty `sources` array.

use std::fs;
use std::path::{Path, PathBuf};

use crate::workspace_root;

/// Strings that only appear when someone has been reading F4SE's source tree.
const BANNED: &[&str] = &[
    "PluginAPI.h",
    "f4se_version.h",
    "RelocAddr",
    "BranchTrampoline",
    "f4se-quarantine",
    "ianpatt/f4se",
    "f4se_common/",
];

/// Files allowed to name the banned strings because they define the rule.
const EXEMPT: &[&str] = &["CLAUDE.md", "docs/PLAN.md", "xtask/src/lint.rs", ".claude/settings.json"];

const TEXT_EXTENSIONS: &[&str] =
    &["rs", "toml", "md", "json", "jsonl", "txt", "py", "sh", "yml", "yaml", "c", "cpp", "h", "hpp", "psc"];

pub fn provenance_lint() -> Result<(), String> {
    let root = workspace_root();
    let mut problems = Vec::new();

    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else { continue };
        for (number, line) in text.lines().enumerate() {
            for banned in BANNED {
                if line.contains(banned) {
                    problems.push(format!("{rel}:{}: cites `{banned}`", number + 1));
                }
            }
        }
        if rel.starts_with("provenance/") && rel.ends_with(".jsonl") {
            check_provenance(&rel, &text, &mut problems);
        }
    }

    if problems.is_empty() {
        println!("provenance-lint: {} files clean", files.len());
        Ok(())
    } else {
        for problem in &problems {
            eprintln!("{problem}");
        }
        Err(format!("{} provenance problem(s)", problems.len()))
    }
}

/// Minimal structural check without pulling in a JSON parser: each non-empty line
/// must be an object with an `"id"` and a `"sources":[` that is not empty.
fn check_provenance(rel: &str, text: &str, problems: &mut Vec<String>) {
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let at = number + 1;
        if !(line.starts_with('{') && line.ends_with('}')) {
            problems.push(format!("{rel}:{at}: not a JSON object"));
            continue;
        }
        if !line.contains("\"id\"") {
            problems.push(format!("{rel}:{at}: missing \"id\""));
        }
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if !compact.contains("\"sources\":[") || compact.contains("\"sources\":[]") {
            problems.push(format!("{rel}:{at}: missing or empty \"sources\""));
        }
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "scratch") {
                continue;
            }
            collect(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| TEXT_EXTENSIONS.contains(&e)) {
            out.push(path);
        }
    }
}
