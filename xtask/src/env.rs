//! Where the game, the Proton prefix and the launch tools live.
//!
//! Defaults match this machine's Heroic install; every path can be overridden with an
//! environment variable so the same tasks work elsewhere.

use std::path::{Path, PathBuf};

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
    /// Create the test prefix if needed. Proton populates it on first launch; we only
    /// seed the game's ini files (the game needs `Fallout4Prefs.ini` to start).
    pub fn ensure_prefix(&self) -> Result<(), String> {
        let my_games = self.prefix.join("pfx").join(MY_GAMES);
        if my_games.join("Fallout4Prefs.ini").is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(&my_games).map_err(|e| format!("creating {}: {e}", my_games.display()))?;
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
