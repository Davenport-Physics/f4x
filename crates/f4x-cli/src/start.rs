//! Starting a supervised session: shared by `f4x launch`, `f4x test` and the MCP server.

use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use f4x_host::contract::{self, LaunchOptions};
use f4x_host::session::{SessionDir, SessionEndpoint, wait_main_loop};
use f4x_host::supervisor::{self, ReadyTimeouts};
use serde_json::Value;

pub struct Started {
    pub dir: SessionDir,
    pub endpoint: SessionEndpoint,
    /// The detached supervisor (reap it with `try_wait`).
    pub supervisor: Child,
}

#[derive(Debug)]
pub struct StartError {
    /// The session directory, once one was created (its logs explain the failure).
    pub session: Option<PathBuf>,
    pub message: String,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.session {
            Some(session) => write!(f, "{} (session {})", self.message, session.display()),
            None => f.write_str(&self.message),
        }
    }
}

/// Prepare the launch, start its supervisor and wait until the agent publishes
/// `session.json`. `lock_timeout`: how long to queue behind another session.
pub fn start(options: &LaunchOptions, lock_timeout: Option<Duration>) -> Result<Started, StartError> {
    let spec = contract::prepare(options).map_err(|message| StartError { session: None, message })?;
    let session = spec.session.clone();
    let fail = |message: String| StartError { session: Some(session.clone()), message };
    let exe = std::env::current_exe().map_err(|e| fail(format!("locating the f4x binary: {e}")))?;
    let mut child = supervisor::spawn_detached(&exe, &session).map_err(fail)?;
    eprintln!("f4x: session {} ({} host{})", session.display(), spec.host, if spec.headless { ", headless" } else { "" });
    let dir = SessionDir::new(&session);
    let timeouts = ReadyTimeouts { lock: lock_timeout, startup: Duration::from_secs(spec.startup_timeout_s) };
    let endpoint = supervisor::wait_ready(&dir, Some(&mut child), timeouts, || {
        eprintln!("f4x: waiting for another f4x game session to finish (launch lock)...");
    })
    .map_err(|message| {
        let _ = child.try_wait();
        fail(message)
    })?;
    Ok(Started { dir, endpoint, supervisor: child })
}

/// Connect to a started session and wait for the game's main loop.
pub fn wait_for_main_loop(started: &Started, timeout: Duration) -> Result<Value, String> {
    let mut client = f4x_host::Client::connect_endpoint(&started.endpoint).map_err(|e| format!("connect: {e}"))?;
    wait_main_loop(&mut client, Instant::now() + timeout).map_err(|e| e.to_string())
}
