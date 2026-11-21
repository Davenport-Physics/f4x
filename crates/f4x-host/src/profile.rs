//! Test profiles: an isolated game directory and Proton prefix per host.
//!
//! `~/.local/share/f4x/profiles/<name>/`
//! - `game/`: a symlink farm over the real install with only vanilla data
//!   (`*.esm`, `*.ba2`, `*.cdx`, `*.csg`, `Video/`) plus the profile's own
//!   `Data/F4SE/Plugins` and `Data/Scripts`. The user's mods and loose files never leak in.
//! - `prefix/`: its own Proton prefix (own wineserver, own My Games: inis, saves, logs).
//! - `profile.json`: `{ "host": "f4se" | "f4x" }`.
//! - `plugin-links.json`: the plugin symlinks f4x put into `Data/F4SE/Plugins`
//!   (see [`Profile::set_plugins`]), so each launch can undo the previous one's.
//!
//! The real install is only read. Everything a profile adds lives under the profile.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::env::GameEnv;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Host {
    /// Real F4SE 0.6.23 (black-box baseline), with the f4x agent as an F4SE plugin.
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

impl std::fmt::Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug)]
pub struct Profile {
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

/// The manifest of plugin symlinks f4x created in a profile's `Data/F4SE/Plugins`.
const PLUGIN_LINKS: &str = "plugin-links.json";

/// `$F4X_HOME`, default `~/.local/share/f4x`: profiles, prefixes, caches, the launch lock.
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

    /// Load `name`, creating it if it does not exist. Fails if it exists for another host.
    pub fn load_or_create(name: &str, host: Host, env: &GameEnv) -> Result<Profile, String> {
        match Profile::load(name) {
            Ok(profile) if profile.host == host => Ok(profile),
            Ok(profile) => Err(format!("profile {name:?} is for host {}, not {host}", profile.host)),
            Err(_) => Profile::create(name, host, env),
        }
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

        let profile_env = env.with_prefix(root.join("prefix"));
        profile_env.ensure_prefix()?;
        fs::write(root.join("profile.json"), json!({"host": host.name()}).to_string()).map_err(|e| e.to_string())?;
        eprintln!("profile {name} ({}) at {}", host.name(), root.display());
        Ok(Profile { name: name.to_owned(), root, host })
    }

    /// Make `dlls` the only f4x-managed plugins in `Data/F4SE/Plugins`: remove every
    /// symlink an earlier call created (recorded in `plugin-links.json`) and any other
    /// `f4x_*` symlink, then link each DLL in under its own file name. Real files in the
    /// folder are never touched. Returns the links created.
    pub fn set_plugins(&self, dlls: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
        let dir = self.plugins_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let manifest = self.root.join(PLUGIN_LINKS);

        let mut stale: Vec<PathBuf> = fs::read_to_string(&manifest)
            .ok()
            .and_then(|text| serde_json::from_str::<PluginLinks>(&text).ok())
            .map(|links| links.links.into_iter().map(|name| dir.join(name)).collect())
            .unwrap_or_default();
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with("f4x_") {
                stale.push(entry.path());
            }
        }
        for link in stale {
            if fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
                fs::remove_file(&link).map_err(|e| format!("removing {}: {e}", link.display()))?;
            }
        }

        let mut names: Vec<String> = Vec::new();
        let mut created = Vec::new();
        for dll in dlls {
            let name = dll.file_name().ok_or_else(|| format!("{} has no file name", dll.display()))?;
            let name = name.to_string_lossy().into_owned();
            if names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                return Err(format!("two plugins are named {name}"));
            }
            let link = dir.join(&name);
            if fs::symlink_metadata(&link).is_ok() {
                return Err(format!("{} already exists and is not an f4x link; not replacing it", link.display()));
            }
            symlink(dll, &link).map_err(|e| format!("linking {}: {e}", link.display()))?;
            names.push(name);
            created.push(link);
        }
        let text = serde_json::to_string_pretty(&PluginLinks { links: names }).map_err(|e| e.to_string())?;
        fs::write(&manifest, text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        Ok(created)
    }
}

#[derive(Serialize, Deserialize)]
struct PluginLinks {
    /// File names inside `Data/F4SE/Plugins`.
    links: Vec<String>,
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

/// Run a helper tool. Its stdout goes to our stderr: stdout may be a protocol stream
/// (the MCP server).
fn run(command: &mut std::process::Command) -> Result<(), String> {
    let status = command.stdout(std::io::stderr()).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_plugins_replaces_only_its_own_links() {
        let root = crate::test_dir("profile");
        let profile = Profile { name: "t".into(), root: root.clone(), host: Host::F4se };
        let plugins = profile.plugins_dir();
        fs::create_dir_all(&plugins).unwrap();
        let build = root.join("build");
        fs::create_dir_all(&build).unwrap();
        for name in ["f4x_agent.dll", "my_test.dll", "other_test.dll", "f4x_probe.dll"] {
            fs::write(build.join(name), name).unwrap();
        }
        // A user's real plugin and a leftover probe link from an older tool.
        fs::write(plugins.join("Buffout4.dll"), "real").unwrap();
        symlink(build.join("f4x_probe.dll"), plugins.join("f4x_probe.dll")).unwrap();

        let made = profile.set_plugins(&[build.join("f4x_agent.dll"), build.join("my_test.dll")]).unwrap();
        assert_eq!(made.len(), 2);
        assert!(!plugins.join("f4x_probe.dll").exists(), "stale f4x_* link removed");
        assert_eq!(fs::read_to_string(plugins.join("my_test.dll")).unwrap(), "my_test.dll");

        profile.set_plugins(&[build.join("other_test.dll")]).unwrap();
        assert!(fs::symlink_metadata(plugins.join("my_test.dll")).is_err(), "manifest link removed");
        assert!(fs::symlink_metadata(plugins.join("f4x_agent.dll")).is_err());
        assert!(plugins.join("other_test.dll").exists());
        assert_eq!(fs::read_to_string(plugins.join("Buffout4.dll")).unwrap(), "real");

        // Never replace a real file.
        fs::write(build.join("Buffout4.dll"), "ours").unwrap();
        assert!(profile.set_plugins(&[build.join("Buffout4.dll")]).is_err());
        assert_eq!(fs::read_to_string(plugins.join("Buffout4.dll")).unwrap(), "real");
    }
}
