//! Launching the game through umu + GE-Proton, the cross-process launch lock, and the
//! guard that makes sure nothing of a session survives it.
//!
//! The facts behind these choices are in `docs/spikes/S1-launch.md`: `PROTON_VERB=run`,
//! dedicated prefixes, pressure-vessel filesystem sharing.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::env::{GameEnv, mount_root, wine_path};

/// One game launch through umu + Proton.
pub struct Launch<'a> {
    pub env: &'a GameEnv,
    /// The Windows program to run (host path), e.g. our loader or `f4se_loader.exe`.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory: the game directory (real install or a profile's farm).
    pub game_dir: PathBuf,
    pub session: PathBuf,
    pub headless: bool,
    pub extra_env: Vec<(String, String)>,
    pub workspace: PathBuf,
    /// More host paths the game must see (e.g. plugin DLLs outside the workspace); their
    /// mount roots are shared into the container next to the workspace and the install.
    pub share: Vec<PathBuf>,
}

/// The launch lock file, `$F4X_HOME/launch.lock`.
pub fn launch_lock_path() -> PathBuf {
    crate::profile::data_home().join("launch.lock")
}

fn open_lock() -> Result<File, String> {
    let path = launch_lock_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Serialises game launches across processes (agents may run tasks in parallel): the
/// returned file holds an exclusive lock until dropped.
pub fn launch_lock() -> Result<File, String> {
    let file = open_lock()?;
    if file.try_lock().is_err() {
        eprintln!("waiting for another f4x game session to finish ({})...", launch_lock_path().display());
        file.lock().map_err(|e| e.to_string())?;
    }
    Ok(file)
}

/// Like [`launch_lock`], but polls, calling `on_wait` once when the lock is busy and
/// giving up (returning `None`) as soon as `cancel` returns true.
pub fn launch_lock_until(mut on_wait: impl FnMut(), mut cancel: impl FnMut() -> bool) -> Result<Option<File>, String> {
    let file = open_lock()?;
    let mut waiting = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(e.to_string()),
        }
        if !waiting {
            waiting = true;
            on_wait();
        }
        if cancel() {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Build Windows-side crates with cargo-xwin into `<root>/target` (release, MSVC).
/// Cargo's output goes to stderr.
pub fn build_windows(root: &Path, packages: &[&str]) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["xwin", "build", "--release", "--target", "x86_64-pc-windows-msvc", "--target-dir"])
        .arg(root.join("target"));
    for package in packages {
        command.args(["-p", package]);
    }
    let status = command.stdout(std::io::stderr()).status().map_err(|e| format!("cargo xwin: {e}"))?;
    if status.success() { Ok(()) } else { Err("windows build failed".into()) }
}

/// Is any `Fallout4.exe` running (ours or the user's)?
pub fn game_running() -> bool {
    Command::new("pgrep").args(["-f", "Fallout4\\.exe"]).stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn spawn(launch: &Launch<'_>) -> Result<Child, String> {
    let env = launch.env;
    let log = File::create(launch.session.join("launch.log")).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;

    let mut command = if launch.headless {
        let mut c = Command::new("gamescope");
        let extra = std::env::var("F4X_GAMESCOPE_ARGS").unwrap_or_else(|_| "-W 1280 -H 720 -r 60".into());
        c.args(["--backend", "headless"]).args(extra.split_whitespace()).arg("--");
        c.arg(&env.umu);
        c
    } else {
        Command::new(&env.umu)
    };
    // The container must see the workspace (build output, session files) and the real
    // install (which a profile's symlink farm points into).
    let mut roots = vec![mount_root(&launch.workspace), mount_root(&env.game_dir)];
    for path in &launch.share {
        let root = mount_root(path);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let shared = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(":");
    command
        .arg(&launch.program)
        .args(&launch.args)
        .current_dir(&launch.game_dir)
        .env("GAMEID", &env.umu_game_id)
        .env("STORE", &env.store)
        .env("WINEPREFIX", &env.prefix)
        .env("STEAM_COMPAT_DATA_PATH", &env.prefix)
        .env("STEAM_COMPAT_INSTALL_PATH", &env.game_dir)
        .env("PROTONPATH", &env.proton)
        .env("STEAM_COMPAT_APP_ID", "0")
        .env("SteamAppId", "0")
        .env("WINEDEBUG", std::env::var("F4X_WINEDEBUG").unwrap_or_else(|_| "-all".into()))
        .env("PRESSURE_VESSEL_FILESYSTEMS_RW", shared)
        .env("F4X_SESSION_DIR", wine_path(&launch.session))
        // `waitforexitandrun` (umu's default) blocks until every wineserver in the
        // prefix exits, so it hangs whenever Vortex/MO2 or another game session is open.
        .env("PROTON_VERB", "run")
        .envs(launch.extra_env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .process_group(0);
    command.spawn().map_err(|e| format!("spawning {}: {e}", env.umu.display()))
}

/// `<root>/target/f4x-sessions`.
pub fn sessions_dir(root: &Path) -> PathBuf {
    root.join("target/f4x-sessions")
}

/// A fresh session directory under `target/f4x-sessions`: `<unix seconds>-<label>`,
/// with a numeric suffix if that name is taken.
pub fn new_session(root: &Path, label: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let base = sessions_dir(root);
    let _ = std::fs::create_dir_all(&base);
    let mut session = base.join(format!("{stamp}-{label}"));
    let mut n = 2;
    while std::fs::create_dir(&session).is_err() {
        session = base.join(format!("{stamp}-{label}-{n}"));
        n += 1;
        if n > 1000 {
            break;
        }
    }
    session
}

/// Owns the launched process group and makes sure nothing survives the run.
pub struct GameGuard {
    child: Option<Child>,
    status: Option<ExitStatus>,
    /// Held for the whole session; see [`launch_lock`].
    _lock: Option<File>,
    /// Our `F4X_SESSION_DIR` value: processes carrying it in their environment are ours.
    session_marker: String,
}

impl GameGuard {
    pub fn new(child: Child, session: &Path, lock: Option<File>) -> Self {
        GameGuard { child: Some(child), status: None, _lock: lock, session_marker: wine_path(session) }
    }

    /// The launched process (gamescope or umu); also the process group id.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// The launched process has exited (other processes of the session may linger).
    pub fn exited(&mut self) -> bool {
        if self.status.is_some() {
            return true;
        }
        match self.child.as_mut() {
            None => true,
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    self.status = Some(status);
                    true
                }
                Ok(None) => false,
                Err(_) => true,
            },
        }
    }

    /// The launched process's exit status, once it has exited.
    pub fn status(&mut self) -> Option<ExitStatus> {
        self.exited();
        self.status
    }

    /// Any process of this session still running (launcher, game, wineserver).
    pub fn alive(&mut self) -> bool {
        !self.exited() || !session_processes(&self.session_marker).is_empty()
    }

    /// Wait up to `timeout` for the launched process tree to exit on its own.
    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        false
    }

    /// Give the session 30 s to exit on its own, then kill it.
    pub fn shutdown(&mut self) {
        if self.child.is_none() {
            return;
        }
        if !self.wait_exit(Duration::from_secs(30)) {
            self.kill();
        }
        self.reap();
    }

    /// Kill the session now: SIGTERM to the process group, SIGKILL 5 s later, then
    /// SIGKILL to every other process that carries this session's marker.
    pub fn kill(&mut self) {
        if let Some(child) = &self.child {
            let group = format!("-{}", child.id());
            let _ = Command::new("kill").args(["-TERM", "--", &group]).stderr(Stdio::null()).status();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && self.alive() {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = Command::new("kill").args(["-KILL", "--", &group]).stderr(Stdio::null()).status();
        }
        // Only kill processes from this session; never the user's own game or Vortex.
        kill_session_processes(&self.session_marker);
        self.reap();
    }

    fn reap(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Ok(status) = child.wait()
        {
            self.status.get_or_insert(status);
        }
    }
}

impl Drop for GameGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Pids whose environment has `F4X_SESSION_DIR=<marker>`.
pub fn session_processes(marker: &str) -> Vec<u32> {
    let needle = format!("F4X_SESSION_DIR={marker}");
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|var| var == needle.as_bytes()))
        })
        .collect()
}

/// SIGKILL every process of a session (by its marker).
pub fn kill_session_processes(marker: &str) {
    for pid in session_processes(marker) {
        let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).stderr(Stdio::null()).status();
    }
}

/// Is `pid` a running (not zombie) process?
pub fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesised command name, which may contain spaces.
        Ok(stat) => stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next()) != Some("Z"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sessions_never_collide() {
        let root = crate::test_dir("sessions");
        let a = new_session(&root, "x");
        let b = new_session(&root, "x");
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
    }

    #[test]
    fn pid_liveness() {
        assert!(pid_alive(std::process::id()));
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!pid_alive(pid), "an exited, unreaped child is a zombie");
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }
}
