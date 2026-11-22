//! `install-game` / `uninstall-game`: use f4x instead of F4SE in the real game install.
//!
//! What install does (all recorded in `~/.local/share/f4x/install/manifest.json`):
//! - copies `f4x_loader.exe` and `f4x_runtime.dll` (release build) into the game folder,
//!   backing up any existing file of the same name first;
//! - points Heroic's launch target for the game (`targetExe` in
//!   `~/.config/heroic/GamesConfig/<app>.json`) at `f4x_loader.exe`. The whole config
//!   file is backed up and only that one value is edited in place.
//!
//! - replaces `f4se_loader.exe` with a copy of `f4x_loader.exe` (the original is backed
//!   up), so anything that still launches F4SE's loader runs f4x: a launcher's cached
//!   settings, a mod manager's "F4SE" tool, shortcuts.
//!
//! F4SE's runtime DLLs stay in place, unused. Plugins in `Data/F4SE/Plugins` are loaded by
//! f4x. Uninstall removes our files
//! (only if unchanged), restores backups, and puts the previous `targetExe` back (only if
//! the value is still ours).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// (installed name, built file).
const FILES: [(&str, &str); 3] =
    [("f4x_loader.exe", "f4x_loader.exe"), ("f4x_runtime.dll", "f4x_runtime.dll"), ("f4se_loader.exe", "f4x_loader.exe")];
/// Heroic's id for GOG's Fallout 4 GOTY (override with `F4X_HEROIC_APP`).
const HEROIC_APP: &str = "1998527297";

fn heroic_app() -> String {
    f4x_host::env::setting("F4X_HEROIC_APP").unwrap_or_else(|| HEROIC_APP.to_owned())
}

fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/f4x/install")
}

fn heroic_config(app: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(format!(".config/heroic/GamesConfig/{app}.json"))
}

fn sha256(path: &Path) -> Result<String, String> {
    let out = std::process::Command::new("sha256sum").arg(path).output().map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(str::to_owned).ok_or_else(|| "sha256sum failed".into())
}

/// Whether the user's game is running: a process whose program (argv[0], a Windows path
/// under Wine) is `Fallout4.exe`, other than f4x's own test sessions.
fn running_game() -> bool {
    let Ok(dir) = fs::read_dir("/proc") else { return false };
    dir.flatten().any(|entry| {
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else { return false };
        // f4x's own test sessions carry their session marker; they use profiles, not
        // the real install.
        if fs::read(entry.path().join("environ")).is_ok_and(|env| env.split(|&b| b == 0).any(|v| v.starts_with(b"F4X_SESSION_DIR="))) {
            return false;
        }
        let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
        let argv0 = String::from_utf8_lossy(argv0);
        argv0.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case("Fallout4.exe"))
    })
}

/// Replace the string value of `"key": "..."` (first occurrence) without reformatting
/// the file. Returns the old value.
fn replace_json_string(text: &str, key: &str, new: &str) -> Result<(String, String), String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle).ok_or_else(|| format!("no {key} in the Heroic config"))?;
    let rest = &text[at + needle.len()..];
    let colon = rest.find(':').ok_or("malformed")?;
    let after = &rest[colon + 1..];
    let open = after.find('"').ok_or("targetExe is not a string")?;
    // Find the closing quote, honouring escapes.
    let bytes = after.as_bytes();
    let mut i = open + 1;
    while i < bytes.len() && bytes[i] != b'"' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    let old_json = &after[open..=i];
    let old: String = serde_json::from_str(old_json).map_err(|e| e.to_string())?;
    let start = at + needle.len() + colon + 1 + open;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&serde_json::to_string(new).map_err(|e| e.to_string())?);
    out.push_str(&text[start + old_json.len()..]);
    Ok((out, old))
}

pub fn install(args: &[String]) -> Result<(), String> {
    let env = f4x_host::env::GameEnv::from_env();
    let game = env.game_dir.clone();
    if !game.join("Fallout4.exe").is_file() {
        return Err(format!("no Fallout4.exe in {}", game.display()));
    }
    if running_game() {
        return Err("Fallout 4 is running; quit it first".into());
    }
    let manifest_path = install_dir().join("manifest.json");
    if manifest_path.exists() && !args.iter().any(|a| a == "--reinstall") {
        return Err(format!("already installed ({}); use --reinstall to update the files", manifest_path.display()));
    }
    let previous: Option<Value> = fs::read_to_string(&manifest_path).ok().and_then(|t| serde_json::from_str(&t).ok());
    let build = crate::workspace_root().join("target/x86_64-pc-windows-msvc/release");
    fs::create_dir_all(install_dir().join("backup")).map_err(|e| e.to_string())?;

    let mut files = Vec::new();
    for (name, built) in FILES {
        let source = build.join(built);
        if !source.is_file() {
            return Err(format!("{} is missing; build with `cargo xwin build --release --target x86_64-pc-windows-msvc -p f4x-loader -p f4x-runtime`", source.display()));
        }
        let target = game.join(name);
        // Back up a foreign file once (not one we installed before).
        let ours_before = previous.as_ref().and_then(|m| m["files"].as_array()).is_some_and(|f| f.iter().any(|e| e["name"] == name));
        let mut backup = previous
            .as_ref()
            .and_then(|m| m["files"].as_array())
            .and_then(|f| f.iter().find(|e| e["name"] == name))
            .and_then(|e| e["backup"].as_str().map(str::to_owned));
        if target.exists() && !ours_before {
            let b = install_dir().join("backup").join(name);
            fs::copy(&target, &b).map_err(|e| format!("backing up {}: {e}", target.display()))?;
            backup = Some(b.display().to_string());
        }
        fs::copy(&source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        files.push(json!({"name": name, "path": target.display().to_string(), "sha256": sha256(&target)?, "backup": backup}));
        println!("installed {}", target.display());
    }

    // Heroic (if installed): point the game's launch target at our loader. Other launchers
    // that start f4se_loader.exe already run f4x (it was replaced above).
    let app = heroic_app();
    let config = heroic_config(&app);
    let loader = game.join("f4x_loader.exe").display().to_string();
    let heroic = if config.is_file() {
        let text = fs::read_to_string(&config).map_err(|e| format!("{}: {e}", config.display()))?;
        let (edited, old_target) = replace_json_string(&text, "targetExe", &loader)?;
        let original_target = previous
            .as_ref()
            .and_then(|m| m["heroic"]["previous_target_exe"].as_str().map(str::to_owned))
            .unwrap_or(old_target.clone());
        if previous.is_none() {
            fs::copy(&config, install_dir().join(format!("heroic-{app}.json.bak"))).map_err(|e| e.to_string())?;
        }
        serde_json::from_str::<Value>(&edited).map_err(|e| format!("edited Heroic config would not parse: {e}"))?;
        fs::write(&config, edited).map_err(|e| format!("{}: {e}", config.display()))?;
        println!("Heroic launch target: {old_target} -> {loader} (restart Heroic to pick it up)");
        json!({"config": config.display().to_string(), "app": app, "previous_target_exe": original_target, "target_exe": loader})
    } else {
        println!("no Heroic config for app {app}; launch the game through f4se_loader.exe or f4x_loader.exe");
        Value::Null
    };

    let manifest = json!({
        "game_dir": game.display().to_string(),
        "files": files,
        "heroic": heroic,
        "installed_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    });
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).map_err(|e| e.to_string())?;
    println!("manifest: {}", manifest_path.display());
    println!("f4se_loader.exe now starts f4x (F4SE's is backed up in {})", install_dir().join("backup").display());
    println!("logs and crash reports while playing: <Documents>/My Games/Fallout4/f4x/latest");
    Ok(())
}

pub fn uninstall(_args: &[String]) -> Result<(), String> {
    let manifest_path = install_dir().join("manifest.json");
    let manifest: Value = serde_json::from_str(&fs::read_to_string(&manifest_path).map_err(|_| "f4x is not installed (no manifest)")?)
        .map_err(|e| e.to_string())?;
    if running_game() {
        return Err("Fallout 4 is running; quit it first".into());
    }
    for f in manifest["files"].as_array().into_iter().flatten() {
        let path = PathBuf::from(f["path"].as_str().unwrap_or_default());
        match sha256(&path) {
            Ok(hash) if Some(hash.as_str()) == f["sha256"].as_str() => {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
                println!("removed {}", path.display());
            }
            Ok(_) => println!("kept {} (changed since install)", path.display()),
            Err(_) => {}
        }
        if let Some(backup) = f["backup"].as_str() {
            fs::copy(backup, &path).map_err(|e| e.to_string())?;
            println!("restored {}", path.display());
        }
    }
    let heroic = &manifest["heroic"];
    if let (Some(config), Some(previous), Some(ours)) = (heroic["config"].as_str(), heroic["previous_target_exe"].as_str(), heroic["target_exe"].as_str()) {
        let text = fs::read_to_string(config).map_err(|e| e.to_string())?;
        let (edited, current) = replace_json_string(&text, "targetExe", previous)?;
        if current == ours {
            fs::write(config, edited).map_err(|e| e.to_string())?;
            println!("Heroic launch target restored: {previous}");
        } else {
            println!("Heroic launch target is {current:?} (changed since install); left as is");
        }
    }
    fs::rename(&manifest_path, install_dir().join("manifest.uninstalled.json")).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::replace_json_string;

    #[test]
    fn replaces_only_the_value() {
        let text = "{\n  \"x\": {\n    \"targetExe\": \"/a b/f4se_loader.exe\",\n    \"other\": 1\n  }\n}";
        let (out, old) = replace_json_string(text, "targetExe", "/a b/f4x_loader.exe").unwrap();
        assert_eq!(old, "/a b/f4se_loader.exe");
        assert_eq!(out, text.replace("f4se_loader", "f4x_loader"));
    }
}
