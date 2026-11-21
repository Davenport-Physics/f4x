//! The session directory shared with the host (`F4X_SESSION_DIR`).

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub struct Session {
    pub dir: PathBuf,
}

static SESSION: OnceLock<Option<Session>> = OnceLock::new();
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The session, if the launcher provided a directory.
pub fn get() -> Option<&'static Session> {
    SESSION
        .get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os("F4X_SESSION_DIR")?);
            fs::create_dir_all(&dir).ok()?;
            Some(Session { dir })
        })
        .as_ref()
}

/// Use `dir` as the session (tests, and hosts that pick the directory themselves).
pub fn init_with(dir: PathBuf) -> Option<&'static Session> {
    let _ = fs::create_dir_all(&dir);
    SESSION.get_or_init(|| Some(Session { dir })).as_ref()
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
