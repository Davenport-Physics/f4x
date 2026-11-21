//! Test profiles: an isolated game directory and Proton prefix per host.
//!
//! `~/.local/share/f4x/profiles/<name>/`
//! - `game/`: a symlink farm over the real install with only vanilla data
//!   (`*.esm`, `*.ba2`, `*.cdx`, `*.csg`, `Video/`) plus the profile's own
//!   `Data/F4SE/Plugins` and `Data/Scripts`. The user's mods and loose files never leak in.
//! - `prefix/`: its own Proton prefix (own wineserver, own My Games: inis, saves, logs).
//! - `profile.json`: `{ "host": "f4se" | "f4x" }`.
//!
//! The real install is only read. Everything a profile adds lives under the profile.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::env::GameEnv;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// Real F4SE 0.6.23 (black-box baseline).
    F4se,
    /// The f4x runtime.
    F4x,
}

impl Host {
    pub fn parse(s: &str) -> Result<Host, String> {
        match s {
            "f4se" => Ok(Host::F4se),
            "f4x" => Ok(Host::F4x),
            _ => Err(format!("unknown host {s:?} (f4se|f4x)")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Host::F4se => "f4se",
            Host::F4x => "f4x",
        }
    }
}

pub struct Profile {
    #[allow(dead_code)]
    pub name: String,
    pub root: PathBuf,
    pub host: Host,
}

/// F4SE 0.6.23 binaries + scripts, extracted (without the bundled source) from the
/// sha-pinned archive by `cache_f4se`.
pub const F4SE_ARCHIVE: &str = "f4se_0_06_23.7z";
pub const F4SE_URL: &str = "https://f4se.silverlock.org/beta/f4se_0_06_23.7z";
pub const F4SE_SHA256: &str = "39575bbb688fc40598496e28afafb5270a6e44c00656a160a30f467efbade21c";
const F4SE_FILES: &[&str] = &["f4se_loader.exe", "f4se_1_10_163.dll", "f4se_steam_loader.dll"];

pub fn data_home() -> PathBuf {
    std::env::var_os("F4X_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/f4x")
    })
}

impl Profile {
    pub fn path(name: &str) -> PathBuf {
        data_home().join("profiles").join(name)
    }

    pub fn load(name: &str) -> Result<Profile, String> {
        let root = Profile::path(name);
        let text = fs::read_to_string(root.join("profile.json"))
            .map_err(|_| format!("profile {name:?} does not exist; run `cargo xtask profile create {name} --host <f4se|f4x>`"))?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let host = Host::parse(value["host"].as_str().unwrap_or(""))?;
        Ok(Profile { name: name.to_owned(), root, host })
    }

    pub fn game_dir(&self) -> PathBuf {
        self.root.join("game")
    }

    pub fn prefix(&self) -> PathBuf {
        self.root.join("prefix")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.game_dir().join("Data/F4SE/Plugins")
    }

    /// Create (or refresh) a profile.
    pub fn create(name: &str, host: Host, env: &GameEnv) -> Result<Profile, String> {
        let root = Profile::path(name);
        let game = root.join("game");
        let data = game.join("Data");
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;

        let skip_root = |n: &str| {
            let lower = n.to_ascii_lowercase();
            lower == "data" || lower.starts_with("f4se_") || lower.starts_with("f4x_")
        };
        link_entries(&env.game_dir, &game, |n| !skip_root(n))?;
        let vanilla = |n: &str| {
            let lower = n.to_ascii_lowercase();
            [".esm", ".ba2", ".cdx", ".csg"].iter().any(|ext| lower.ends_with(ext)) || lower == "video"
        };
        link_entries(&env.game_dir.join("Data"), &data, vanilla)?;
        fs::create_dir_all(data.join("F4SE/Plugins")).map_err(|e| e.to_string())?;
        fs::create_dir_all(data.join("Scripts")).map_err(|e| e.to_string())?;

        if host == Host::F4se {
            let cache = cache_f4se()?;
            for file in F4SE_FILES {
                copy_if_changed(&cache.join(file), &game.join(file))?;
            }
            let scripts = cache.join("Data/Scripts");
            for entry in fs::read_dir(&scripts).map_err(|e| e.to_string())?.flatten() {
                copy_if_changed(&entry.path(), &data.join("Scripts").join(entry.file_name()))?;
            }
        }

        let profile_env = GameEnv { prefix: root.join("prefix"), ..GameEnv::from_env() };
        profile_env.ensure_prefix()?;
        fs::write(root.join("profile.json"), json!({"host": host.name()}).to_string()).map_err(|e| e.to_string())?;
        eprintln!("profile {name} ({}) at {}", host.name(), root.display());
        Ok(Profile { name: name.to_owned(), root, host })
    }
}

/// Symlink every entry of `from` accepted by `keep` into `to` (replacing stale links).
fn link_entries(from: &Path, to: &Path, keep: impl Fn(&str) -> bool) -> Result<(), String> {
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !keep(&name_str) {
            continue;
        }
        let link = to.join(&name);
        if let Ok(meta) = fs::symlink_metadata(&link) {
            if meta.file_type().is_symlink() {
                fs::remove_file(&link).map_err(|e| e.to_string())?;
            } else {
                continue; // a real file the profile owns
            }
        }
        symlink(entry.path(), &link).map_err(|e| format!("linking {}: {e}", link.display()))?;
    }
    Ok(())
}

fn copy_if_changed(from: &Path, to: &Path) -> Result<(), String> {
    let new = fs::read(from).map_err(|e| format!("{}: {e}", from.display()))?;
    if fs::read(to).ok().as_deref() != Some(new.as_slice()) {
        let _ = fs::remove_file(to);
        fs::write(to, new).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// Download (sha-pinned) and extract F4SE's runtime files, never its bundled source.
pub fn cache_f4se() -> Result<PathBuf, String> {
    let cache = data_home().join("cache");
    let out = cache.join("f4se-0.6.23");
    let extracted = out.join("f4se_0_06_23");
    if extracted.join("f4se_1_10_163.dll").is_file() {
        return Ok(extracted);
    }
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let archive = cache.join(F4SE_ARCHIVE);
    if !archive.is_file() {
        run(std::process::Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&archive).arg(F4SE_URL))?;
    }
    let sum = std::process::Command::new("sha256sum").arg(&archive).output().map_err(|e| e.to_string())?;
    if !String::from_utf8_lossy(&sum.stdout).starts_with(F4SE_SHA256) {
        return Err(format!("{} does not match the pinned SHA-256", archive.display()));
    }
    let mut command = std::process::Command::new("7z");
    command.args(["x", "-y", "-bso0", "-bsp0"]).arg(&archive).arg(format!("-o{}", out.display()));
    for file in F4SE_FILES {
        command.arg(format!("f4se_0_06_23/{file}"));
    }
    command.arg("f4se_0_06_23/Data/Scripts/*.pex");
    run(&mut command)?;
    Ok(extracted)
}

fn run(command: &mut std::process::Command) -> Result<(), String> {
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed")) }
}

pub fn cli(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            let name = args.get(1).ok_or("profile create <name> --host <f4se|f4x>")?;
            let host = args.iter().position(|a| a == "--host").and_then(|i| args.get(i + 1)).ok_or("--host <f4se|f4x>")?;
            Profile::create(name, Host::parse(host)?, &GameEnv::from_env()).map(|_| ())
        }
        Some("path") => {
            let name = args.get(1).ok_or("profile path <name>")?;
            println!("{}", Profile::load(name)?.root.display());
            Ok(())
        }
        _ => Err("usage: cargo xtask profile <create NAME --host f4se|f4x | path NAME>".into()),
    }
}
