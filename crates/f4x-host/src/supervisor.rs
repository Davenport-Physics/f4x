//! The session supervisor: a small process that owns one game session from launch to
//! exit, so host commands can return while the game keeps running.
//!
//! `f4x launch` writes the plan to `<session>/launch.json` and starts
//! `<f4x exe> __supervise <session>` in its own Unix session (no controlling terminal,
//! immune to the shell's SIGHUP). The supervisor:
//! 1. takes the launch lock (polling, so a `stop-request` cancels the wait);
//! 2. re-applies the prefix test settings (mute) and spawns the game ([`spawn`]);
//! 3. watches it every 250 ms and records its phase in `supervisor.json`;
//! 4. kills it on a `stop-request` file, SIGTERM/SIGINT/SIGHUP, the startup timeout
//!    (no `session.json`), the lifetime limit, or a fatal crash report whose process
//!    has not died 15 s later;
//! 5. when the game exits (after `quit`, a crash or on its own), waits for the rest of
//!    the session's processes ([`GameGuard::shutdown`]), copies F4SE's logs into the
//!    session (the next launch overwrites them), releases the lock and records why the
//!    session ended.
//!
//! If the supervisor itself is SIGKILLed the kernel still releases the lock, but the
//! game keeps running; `f4x quit` then kills the session's processes by their marker.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::contract::LaunchSpec;
use crate::launch::{GameGuard, launch_lock_until, pid_alive, spawn};
use crate::session::{SessionDir, SessionEndpoint};

pub const LAUNCH_FILE: &str = "launch.json";
pub const STATE_FILE: &str = "supervisor.json";
pub const LOG_FILE: &str = "supervisor.log";
/// Touch to have the supervisor kill the game now.
pub const STOP_REQUEST: &str = "stop-request";
/// Written by `quit` before it asks the agent to exit, so the exit is recorded as a quit.
pub const QUIT_REQUEST: &str = "quit-request";
/// The hidden subcommand a host binary must route to [`main`].
pub const SUPERVISE_ARG: &str = "__supervise";
/// A fatal crash report whose process is still alive this long later gets killed.
const CRASH_GRACE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Another session holds the launch lock.
    WaitingLock,
    /// The game is starting; the agent has not published `session.json` yet.
    Starting,
    /// `session.json` is there.
    Running,
    /// The game exited or is being killed; cleaning up.
    Stopping,
    /// Everything of the session is gone and the lock is released.
    Exited,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExitReason {
    /// The game exited after `quit`.
    Quit,
    /// The game exited by itself, without a crash report.
    GameExited,
    /// The game exited with a fatal crash report, or exited without `quit` and with any
    /// crash report; or it hung after a fatal report and was killed.
    Crash,
    /// A `stop-request` (e.g. `f4x quit --force`).
    StopRequest,
    /// No `session.json` within the startup timeout.
    StartupTimeout,
    /// The lifetime limit expired.
    Lifetime,
    /// The supervisor got SIGTERM/SIGINT/SIGHUP.
    Signal,
    /// The wait for the launch lock was cancelled.
    LockCancelled,
    /// The game could not be started.
    SpawnFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExitInfo {
    pub reason: ExitReason,
    /// The launcher's exit code (`gamescope`/umu, which pass on the game's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ExitInfo {
    /// A clean end: quit, or a zero exit without a crash.
    pub fn clean(&self) -> bool {
        match self.reason {
            ExitReason::Quit => true,
            ExitReason::GameExited => self.code == Some(0),
            _ => false,
        }
    }
}

/// `supervisor.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SupervisorState {
    pub pid: u32,
    pub phase: Phase,
    pub host: String,
    pub headless: bool,
    /// Unix milliseconds.
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// The launched process (gamescope or umu), also its process group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitInfo>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn write_state(session: &Path, state: &SupervisorState) {
    let target = session.join(STATE_FILE);
    let temp = session.join(format!("{STATE_FILE}.tmp"));
    if let Ok(text) = serde_json::to_string_pretty(state)
        && std::fs::write(&temp, text).is_ok()
    {
        let _ = std::fs::rename(&temp, &target);
    }
}

/// Run one session to its end in this process (see the module docs) and return the
/// final state. Progress goes to stderr.
pub fn run(spec: &LaunchSpec) -> SupervisorState {
    let session = spec.session.as_path();
    let dir = SessionDir::new(session);
    let mut state = SupervisorState {
        pid: std::process::id(),
        phase: Phase::Starting,
        host: spec.host.name().into(),
        headless: spec.headless,
        created_ms: now_ms(),
        launched_ms: None,
        ready_ms: None,
        ended_ms: None,
        launcher_pid: None,
        exit: None,
    };
    write_state(session, &state);

    let finish = |mut state: SupervisorState, reason, detail: Option<String>| {
        eprintln!("supervisor: session ended: {reason:?} {}", detail.as_deref().unwrap_or(""));
        state.phase = Phase::Exited;
        state.ended_ms = Some(now_ms());
        state.exit = Some(ExitInfo { reason, code: None, signal: None, detail });
        write_state(session, &state);
        state
    };

    let lock = {
        let waiting = std::cell::Cell::new(false);
        let result = launch_lock_until(
            || {
                waiting.set(true);
                eprintln!("supervisor: waiting for another f4x game session to finish (launch lock)");
            },
            || {
                if waiting.get() {
                    waiting.set(false);
                    let mut s = state.clone();
                    s.phase = Phase::WaitingLock;
                    write_state(session, &s);
                }
                dir.stop_requested() || crate::signals::interrupted()
            },
        );
        match result {
            Ok(Some(lock)) => lock,
            Ok(None) => return finish(state, ExitReason::LockCancelled, None),
            Err(e) => return finish(state, ExitReason::SpawnFailed, Some(format!("launch lock: {e}"))),
        }
    };
    state.phase = Phase::Starting;
    write_state(session, &state);

    // Mute (and the other test settings) right before every launch.
    if let Err(e) = spec.env.ensure_prefix() {
        return finish(state, ExitReason::SpawnFailed, Some(e));
    }
    let spawned_at = SystemTime::now();
    let child = match spawn(&spec.launch()) {
        Ok(child) => child,
        Err(e) => return finish(state, ExitReason::SpawnFailed, Some(e)),
    };
    let mut guard = GameGuard::new(child, session, Some(lock));
    state.launcher_pid = guard.pid();
    state.launched_ms = Some(now_ms());
    write_state(session, &state);
    eprintln!("supervisor: launched {} (pid {:?})", spec.program.display(), state.launcher_pid);

    let launched = Instant::now();
    let startup = Duration::from_secs(spec.startup_timeout_s);
    let lifetime = spec.lifetime_s.map(Duration::from_secs);
    let mut fatal_since: Option<Instant> = None;
    let (reason, kill, detail) = loop {
        std::thread::sleep(Duration::from_millis(250));
        if crate::signals::interrupted() {
            break (ExitReason::Signal, true, None);
        }
        if dir.stop_requested() {
            break (ExitReason::StopRequest, true, None);
        }
        if guard.exited() {
            break (ExitReason::GameExited, false, None);
        }
        if state.phase == Phase::Starting {
            if SessionEndpoint::read(session).is_ok() {
                state.phase = Phase::Running;
                state.ready_ms = Some(now_ms());
                write_state(session, &state);
                eprintln!("supervisor: session.json published after {:.1} s", launched.elapsed().as_secs_f64());
            } else if launched.elapsed() > startup {
                let detail = format!("session.json did not appear within {} s", startup.as_secs());
                break (ExitReason::StartupTimeout, true, Some(detail));
            }
        }
        if lifetime.is_some_and(|l| launched.elapsed() > l) {
            break (ExitReason::Lifetime, true, Some(format!("lifetime of {} s expired", lifetime.unwrap().as_secs())));
        }
        if dir.fatal_crash() {
            let since = *fatal_since.get_or_insert_with(Instant::now);
            if since.elapsed() > CRASH_GRACE {
                break (ExitReason::Crash, true, Some("fatal crash report; the process did not exit".into()));
            }
        }
    };

    state.phase = Phase::Stopping;
    write_state(session, &state);
    eprintln!("supervisor: stopping ({reason:?})");
    if kill {
        guard.kill();
    } else {
        guard.shutdown();
    }
    let status = guard.status();
    spec.env.collect_f4se_logs(session, spawned_at);
    drop(guard); // releases the launch lock

    // First-chance reports (`fatal: false`) may be exceptions the game handled; they
    // only mean a crash when the game went away without being asked to.
    let reason = match reason {
        ExitReason::GameExited if dir.fatal_crash() => ExitReason::Crash,
        ExitReason::GameExited if dir.file(QUIT_REQUEST).exists() => ExitReason::Quit,
        ExitReason::GameExited if !dir.crashes().is_empty() => ExitReason::Crash,
        other => other,
    };
    state.phase = Phase::Exited;
    state.ended_ms = Some(now_ms());
    state.exit = Some(ExitInfo {
        reason,
        code: status.and_then(|s| s.code()),
        signal: status.and_then(|s| s.signal()),
        detail,
    });
    write_state(session, &state);
    eprintln!("supervisor: session ended: {:?}", state.exit);
    state
}

/// Entry point of `<exe> __supervise <session>`: returns the process exit code.
pub fn main(session: &Path) -> i32 {
    crate::signals::install();
    let spec = match LaunchSpec::read(session) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("supervisor: {e}");
            return 2;
        }
    };
    let state = run(&spec);
    if state.exit.as_ref().is_some_and(ExitInfo::clean) { 0 } else { 1 }
}

/// Start `<exe> __supervise <session>` detached: its own Unix session, stdin closed,
/// output to `<session>/supervisor.log`.
pub fn spawn_detached(exe: &Path, session: &Path) -> Result<Child, String> {
    let log = std::fs::File::create(session.join(LOG_FILE)).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .arg(SUPERVISE_ARG)
        .arg(session)
        // Only the game's processes may carry a session marker.
        .env_remove("F4X_SESSION_DIR")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map_err(|e| format!("starting the supervisor ({}): {e}", exe.display()))
}

/// How long [`wait_ready`] waits for each stage.
#[derive(Clone, Copy, Debug)]
pub struct ReadyTimeouts {
    /// For the launch lock (another session running). `None`: forever.
    pub lock: Option<Duration>,
    /// For `session.json` after the game starts.
    pub startup: Duration,
}

/// Wait until a supervised session publishes `session.json`. `child` is the
/// supervisor if this process started it (it gets reaped); otherwise liveness comes
/// from `supervisor.json`'s pid. On a lock timeout or an interrupt the session is told
/// to stop. `on_waiting_lock` is called once if the session waits for the lock.
pub fn wait_ready(
    dir: &SessionDir,
    mut child: Option<&mut Child>,
    timeouts: ReadyTimeouts,
    mut on_waiting_lock: impl FnMut(),
) -> Result<SessionEndpoint, String> {
    let started = Instant::now();
    let mut lock_wait_reported = false;
    let mut launched_at: Option<Instant> = None;
    loop {
        if let Ok(endpoint) = dir.endpoint() {
            return Ok(endpoint);
        }
        let state = dir.supervisor();
        let alive = match child.as_deref_mut() {
            Some(child) => child.try_wait().ok().flatten().is_none(),
            None => state.as_ref().is_some_and(|s| pid_alive(s.pid)),
        };
        if !alive || state.as_ref().is_some_and(|s| s.phase == Phase::Exited) {
            // One last look: it may have published just before exiting.
            if let Ok(endpoint) = dir.endpoint() {
                return Ok(endpoint);
            }
            let why = match state.and_then(|s| s.exit) {
                Some(exit) => {
                    format!("{:?}{}", exit.reason, exit.detail.map(|d| format!(": {d}")).unwrap_or_default())
                }
                None => "the supervisor exited (see supervisor.log)".into(),
            };
            return Err(format!("the session ended before the agent published session.json ({why})"));
        }
        if crate::signals::interrupted() {
            dir.request_stop();
            return Err("interrupted".into());
        }
        match state.as_ref().map(|s| s.phase) {
            Some(Phase::WaitingLock) => {
                if !lock_wait_reported {
                    lock_wait_reported = true;
                    on_waiting_lock();
                }
                if timeouts.lock.is_some_and(|t| started.elapsed() > t) {
                    dir.request_stop();
                    return Err("timed out waiting for the launch lock (another f4x session is running)".into());
                }
            }
            Some(Phase::Starting | Phase::Running) if state.as_ref().is_some_and(|s| s.launched_ms.is_some()) => {
                let at = *launched_at.get_or_insert_with(Instant::now);
                // The supervisor enforces the startup timeout itself; this is a backstop.
                if at.elapsed() > timeouts.startup + Duration::from_secs(15) {
                    dir.request_stop();
                    return Err("timed out waiting for session.json".into());
                }
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
