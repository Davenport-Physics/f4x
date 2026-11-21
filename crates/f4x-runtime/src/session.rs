//! The per-launch session directory shared with the host (`F4X_SESSION_DIR`).
//!
//! Files: `runtime.log` (text log), `session.json` (pid, control port, token; written
//! atomically once the control server listens) and `heartbeat.json` (once a second).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub struct Session {
    pub dir: PathBuf,
    pub started: Instant,
    log: Mutex<File>,
}

static SESSION: OnceLock<Option<Session>> = OnceLock::new();

pub fn get() -> Option<&'static Session> {
    SESSION
        .get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os("F4X_SESSION_DIR")?);
            fs::create_dir_all(&dir).ok()?;
            let log = OpenOptions::new().create(true).append(true).open(dir.join("runtime.log")).ok()?;
            Some(Session { dir, started: Instant::now(), log: Mutex::new(log) })
        })
        .as_ref()
}

impl Session {
    pub fn log(&self, message: &str) {
        let elapsed = self.started.elapsed();
        if let Ok(mut file) = self.log.lock() {
            let _ = writeln!(file, "[{:>9.3}] {message}", elapsed.as_secs_f64());
        }
    }

    pub fn uptime_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    /// Write `name` atomically (temp file + rename) so the host never reads half a file.
    pub fn write_atomic(&self, name: &str, contents: &str) {
        let target = self.dir.join(name);
        let temp = self.dir.join(format!("{name}.tmp"));
        if fs::write(&temp, contents).is_ok() {
            let _ = replace(&temp, &target);
        }
    }
}

fn replace(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

/// Log to the session if there is one.
pub fn log(message: &str) {
    if let Some(session) = get() {
        session.log(message);
    }
}
