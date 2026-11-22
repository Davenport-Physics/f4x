//! The session directory shared with the host (`F4X_SESSION_DIR`).
//!
//! Without a host (normal play, launched by `f4x_loader.exe` from the game folder) the
//! agent still keeps its logs and crash reports, in
//! `Documents\My Games\Fallout4\f4x\latest` (the run before is kept in `previous`).

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub struct Session {
    pub dir: PathBuf,
    /// Started by the f4x host (tests, agents) rather than for normal play.
    pub hosted: bool,
}

static SESSION: OnceLock<Option<Session>> = OnceLock::new();
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The session, if the launcher provided a directory.
pub fn get() -> Option<&'static Session> {
    SESSION
        .get_or_init(|| {
            if let Some(dir) = std::env::var_os("F4X_SESSION_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).ok()?;
                return Some(Session { dir, hosted: true });
            }
            play_dir().map(|dir| Session { dir, hosted: false })
        })
        .as_ref()
}

/// Use `dir` as the session (tests, and hosts that pick the directory themselves).
pub fn init_with(dir: PathBuf) -> Option<&'static Session> {
    let _ = fs::create_dir_all(&dir);
    SESSION.get_or_init(|| Some(Session { dir, hosted: true })).as_ref()
}

/// `Documents\My Games\Fallout4\f4x\latest`, rotating the last run to `previous`.
#[cfg(windows)]
fn play_dir() -> Option<PathBuf> {
    // Only inside the game process (not, e.g., unit tests).
    let exe = std::env::current_exe().ok()?;
    if !exe.file_name()?.to_string_lossy().eq_ignore_ascii_case("Fallout4.exe") {
        return None;
    }
    let root = documents()?.join("My Games").join("Fallout4").join("f4x");
    let (latest, previous) = (root.join("latest"), root.join("previous"));
    let _ = fs::remove_dir_all(&previous);
    let _ = fs::rename(&latest, &previous);
    fs::create_dir_all(&latest).ok()?;
    Some(latest)
}

#[cfg(not(windows))]
fn play_dir() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn documents() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: standard shell call; the buffer is freed below.
    unsafe {
        if SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut path) < 0 || path.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path.cast());
        Some(PathBuf::from(text))
    }
}

impl Session {
    /// Write `name` atomically (temp file + rename) so the host never reads half a file.
    pub fn write_atomic(&self, name: &str, contents: &[u8]) -> std::io::Result<()> {
        let target = self.dir.join(name);
        let temp = self.dir.join(format!(".{name}.tmp"));
        fs::write(&temp, contents)?;
        fs::rename(&temp, &target)
    }

    /// A fresh `prefix<n>.ext` file name.
    pub fn numbered(&self, prefix: &str, ext: &str) -> String {
        loop {
            let n = COUNTER.fetch_add(1, Relaxed);
            let name = format!("{prefix}{n}.{ext}");
            if !self.dir.join(&name).exists() {
                return name;
            }
        }
    }
}
