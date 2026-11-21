//! Where the game, the Proton prefix and the launch tools live.
//!
//! Defaults match this machine's Heroic install; every path can be overridden with an
//! environment variable so the same tasks work elsewhere.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GameEnv {
    pub game_dir: PathBuf,
    pub prefix: PathBuf,
    /// The user's own prefix, used read-only to seed new test prefixes with their inis.
    pub source_prefix: PathBuf,
    pub proton: PathBuf,
    pub umu: PathBuf,
    /// umu's game id; picks protonfixes for Fallout 4.
    pub umu_game_id: String,
    pub store: String,
}

fn var_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(expand_home(default)))
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_owned(),
    }
}

impl GameEnv {
    pub fn from_env() -> Self {
        GameEnv {
            game_dir: var_path("F4X_GAME_DIR", "/path/to/games/HeroicGames/Fallout 4 GOTY"),
            // A dedicated prefix: its own wineserver (so Vortex/Heroic sessions in the
            // user's prefix can't interfere, and headless gamescope works), its own
            // My Games folder (inis, saves) and nothing of the user's gets touched.
            prefix: var_path("F4X_PREFIX", "~/.local/share/f4x/prefixes/test"),
            source_prefix: var_path(
                "F4X_SOURCE_PREFIX",
                "~/Games/Heroic/Prefixes/default/Fallout 4 Game of the Year Edition",
            ),
            proton: var_path("F4X_PROTON", "~/.config/heroic/tools/proton/GE-Proton-latest"),
            umu: var_path("F4X_UMU", "~/.config/heroic/tools/runtimes/umu/umu_run.py"),
            umu_game_id: std::env::var("F4X_UMU_GAMEID").unwrap_or_else(|_| "umu-377160".into()),
            store: std::env::var("F4X_STORE").unwrap_or_else(|_| "gog".into()),
        }
    }

    /// The same environment with another Proton prefix (a profile's own).
    pub fn with_prefix(&self, prefix: PathBuf) -> Self {
        GameEnv { prefix, ..self.clone() }
    }

    pub fn check(&self) -> Result<(), String> {
        for (what, path) in [
            ("game dir (F4X_GAME_DIR)", self.game_dir.join("Fallout4.exe")),
            ("Proton (F4X_PROTON)", self.proton.join("proton")),
            ("umu (F4X_UMU)", self.umu.clone()),
        ] {
            if !path.exists() {
                return Err(format!("{what} not found: {}", path.display()));
            }
        }
        Ok(())
    }
}

const MY_GAMES: &str = "drive_c/users/steamuser/Documents/My Games/Fallout4";
const SEED_INIS: &[&str] = &["Fallout4.ini", "Fallout4Prefs.ini", "Fallout4Custom.ini"];

impl GameEnv {
    /// The prefix's `My Games/Fallout4` (inis, saves, F4SE's logs).
    pub fn my_games(&self) -> PathBuf {
        self.prefix.join("pfx").join(MY_GAMES)
    }

    /// Copy F4SE's logs (`My Games/Fallout4/F4SE/*.log`) modified since `since` into
    /// `<session>/f4se-logs/`, because the next launch overwrites them. Returns the copies.
    pub fn collect_f4se_logs(&self, session: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.my_games().join("F4SE")) else { return Vec::new() };
        let out = session.join("f4se-logs");
        let mut copied = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let fresh = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since);
            if !fresh || path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("log")) {
                continue;
            }
            let target = out.join(entry.file_name());
            if std::fs::create_dir_all(&out).is_ok() && std::fs::copy(&path, &target).is_ok() {
                copied.push(target);
            }
        }
        copied
    }

    /// Create the test prefix if needed. Proton populates it on first launch; we only
    /// seed the game's ini files (the game needs `Fallout4Prefs.ini` to start).
    /// Every call re-applies the test settings, so the game is always muted.
    pub fn ensure_prefix(&self) -> Result<(), String> {
        let my_games = self.my_games();
        if !my_games.join("Fallout4Prefs.ini").is_file() {
            self.seed_prefix(&my_games)?;
        }
        apply_test_settings(&my_games)
    }

    fn seed_prefix(&self, my_games: &Path) -> Result<(), String> {
        std::fs::create_dir_all(my_games).map_err(|e| format!("creating {}: {e}", my_games.display()))?;
        let source = self.source_prefix.join(MY_GAMES);
        for ini in SEED_INIS {
            let from = source.join(ini);
            if from.is_file() {
                std::fs::copy(&from, my_games.join(ini)).map_err(|e| format!("copying {}: {e}", from.display()))?;
            }
        }
        if !my_games.join("Fallout4Prefs.ini").is_file() {
            return Err(format!(
                "no Fallout4Prefs.ini to seed {} from {} (start the game once normally, or set F4X_SOURCE_PREFIX)",
                self.prefix.display(),
                source.display()
            ));
        }
        eprintln!("created test prefix {}", self.prefix.display());
        Ok(())
    }
}

/// Settings every f4x test prefix gets (these are f4x's own copies of the inis, never
/// the user's): the game is muted.
fn apply_test_settings(my_games: &Path) -> Result<(), String> {
    let prefs = my_games.join("Fallout4Prefs.ini");
    let text = std::fs::read_to_string(&prefs).map_err(|e| format!("{}: {e}", prefs.display()))?;
    let updated = set_ini(&text, "AudioMenu", "fAudioMasterVolume", "0.0000");
    if updated != text {
        std::fs::write(&prefs, updated).map_err(|e| format!("{}: {e}", prefs.display()))?;
    }
    Ok(())
}

/// Set `key=value` in `[section]`, adding the key or section if missing. Keeps the
/// file's line endings.
pub fn set_ini(text: &str, section: &str, key: &str, value: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = Vec::new();
    let mut in_section = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix('[') {
            if in_section && !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            in_section = header.trim_end_matches(']').eq_ignore_ascii_case(section);
        } else if in_section
            && !done
            && trimmed.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        {
            out.push(format!("{key}={value}"));
            done = true;
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        if !in_section {
            out.push(format!("[{section}]"));
        }
        out.push(format!("{key}={value}"));
    }
    let mut joined = out.join(newline);
    joined.push_str(newline);
    joined
}

/// Wine maps `Z:` to the host root, so any host path is reachable as `Z:\...`.
pub fn wine_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// The top-level mount point of `path` (e.g. `/mnt/<uuid>`), which pressure-vessel
/// must be told to share so the game can see build output and session files.
pub fn mount_root(path: &Path) -> PathBuf {
    let mut components = path.components();
    let mut root = PathBuf::new();
    for _ in 0..3 {
        if let Some(c) = components.next() {
            root.push(c);
        }
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_existing_missing_and_new_section_keys() {
        let ini = "[General]\r\nsFoo=1\r\n[AudioMenu]\r\nfAudioMasterVolume=1.0000\r\nfVal7=1.0\r\n";
        let out = set_ini(ini, "AudioMenu", "fAudioMasterVolume", "0.0000");
        assert_eq!(out, "[General]\r\nsFoo=1\r\n[AudioMenu]\r\nfAudioMasterVolume=0.0000\r\nfVal7=1.0\r\n");
        let out = set_ini("[General]\nsFoo=1\n", "General", "bAlwaysActive", "1");
        assert_eq!(out, "[General]\nsFoo=1\nbAlwaysActive=1\n");
        let out = set_ini("[General]\nsFoo=1\n", "AudioMenu", "fAudioMasterVolume", "0");
        assert_eq!(out, "[General]\nsFoo=1\n[AudioMenu]\nfAudioMasterVolume=0\n");
    }

    #[test]
    fn ensure_prefix_mutes_an_existing_prefix() {
        let dir = crate::test_dir("mute");
        let my_games = dir.join("pfx").join(MY_GAMES);
        std::fs::create_dir_all(&my_games).unwrap();
        std::fs::write(my_games.join("Fallout4Prefs.ini"), "[AudioMenu]\r\nfAudioMasterVolume=1.0000\r\n").unwrap();
        let env = GameEnv { prefix: dir.clone(), source_prefix: dir.join("none"), ..GameEnv::from_env() };
        env.ensure_prefix().unwrap();
        let text = std::fs::read_to_string(my_games.join("Fallout4Prefs.ini")).unwrap();
        assert_eq!(text, "[AudioMenu]\r\nfAudioMasterVolume=0.0000\r\n");
    }

    #[test]
    fn collects_only_fresh_f4se_logs() {
        let dir = crate::test_dir("f4se-logs");
        let env = GameEnv { prefix: dir.join("prefix"), ..GameEnv::from_env() };
        let logs = env.my_games().join("F4SE");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("old.log"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let since = std::time::SystemTime::now();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(logs.join("f4se.log"), "new").unwrap();
        std::fs::write(logs.join("notes.txt"), "x").unwrap();
        let session = dir.join("session");
        let copied = env.collect_f4se_logs(&session, since);
        assert_eq!(copied, vec![session.join("f4se-logs/f4se.log")]);
        assert_eq!(std::fs::read_to_string(&copied[0]).unwrap(), "new");
    }

    #[test]
    fn wine_paths_and_mount_roots() {
        assert_eq!(wine_path(Path::new("/mnt/x/a b/c")), "Z:\\mnt\\x\\a b\\c");
        assert_eq!(mount_root(Path::new("/mnt/uuid/Software/f4x")), PathBuf::from("/mnt/uuid"));
        assert_eq!(mount_root(Path::new("/home/me/x")), PathBuf::from("/home/me"));
    }
}
