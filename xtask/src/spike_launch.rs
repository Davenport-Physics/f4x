//! Spike S1: launch the game under Proton with the f4x runtime injected, then check
//! injection, the control channel and the main-loop hook.
//!
//! `cargo xtask spike-launch [--headless] [--runs N] [--no-build]`

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::env::{GameEnv, mount_root, wine_path};
use crate::workspace_root;

const STARTUP_DEADLINE: Duration = Duration::from_secs(90);
/// `--hold N`: keep the game running N more seconds after the checks, pinging once a
/// second, and report how long it stayed alive.
static HOLD_SECONDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn run(args: &[String]) -> Result<(), String> {
    let headless = args.iter().any(|a| a == "--headless");
    let no_build = args.iter().any(|a| a == "--no-build");
    let hold = args
        .iter()
        .position(|a| a == "--hold")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    HOLD_SECONDS.store(hold, std::sync::atomic::Ordering::Relaxed);
    let runs = args
        .iter()
        .position(|a| a == "--runs")
        .and_then(|i| args.get(i + 1))
        .map(|n| n.parse::<u32>().map_err(|e| format!("--runs: {e}")))
        .transpose()?
        .unwrap_or(1);

    let env = GameEnv::from_env();
    env.check()?;
    env.ensure_prefix()?;
    if game_running() {
        eprintln!("note: a Fallout4.exe is already running; f4x sessions queue on the launch lock");
    }
    let root = workspace_root();
    if !no_build {
        build_windows(&root, &["f4x-loader", "f4x-runtime"])?;
    }
    let out = root.join("target/x86_64-pc-windows-msvc/release");

    let mut failures = 0;
    for run in 1..=runs {
        let result = launch_once(&env, &root, &out, headless, run);
        println!("{result}");
        let ok = ["injected", "tcp", "frame_hook"].iter().all(|k| result[k] == true);
        if !ok {
            failures += 1;
        }
    }
    if failures > 0 { Err(format!("{failures} of {runs} run(s) failed")) } else { Ok(()) }
}

pub fn build_windows(root: &Path, packages: &[&str]) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command.current_dir(root).args(["xwin", "build", "--release", "--target", "x86_64-pc-windows-msvc"]);
    for package in packages {
        command.args(["-p", package]);
    }
    let status = command.status().map_err(|e| format!("cargo xwin: {e}"))?;
    if status.success() { Ok(()) } else { Err("windows build failed".into()) }
}

fn game_running() -> bool {
    Command::new("pgrep").args(["-f", "Fallout4\\.exe"]).stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn launch_once(env: &GameEnv, root: &Path, out: &Path, headless: bool, run: u32) -> Value {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let session = root.join(format!("target/f4x-sessions/{stamp}-spike-{run}"));
    let _ = std::fs::create_dir_all(&session);
    let started = Instant::now();

    let mut result = json!({
        "run": run, "headless": headless, "injected": false, "tcp": false, "frame_hook": false,
        "session": session.display().to_string(),
    });

    let lock = launch_lock().ok();
    let child = match spawn_game(env, root, out, &session, headless) {
        Ok(child) => child,
        Err(error) => {
            result["error"] = error.into();
            return result;
        }
    };
    let mut guard = GameGuard { child: Some(child), _lock: lock, session_marker: wine_path(&session) };

    match drive(&session, &mut guard, &mut result) {
        Ok(()) => {}
        Err(error) => result["error"] = error.into(),
    }
    result["injected"] = session_log_contains(&session, "attached").into();
    guard.shutdown();
    result["seconds"] = ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0).into();
    result
}

fn spawn_game(env: &GameEnv, root: &Path, out: &Path, session: &Path, headless: bool) -> Result<Child, String> {
    let dll = out.join("f4x_runtime.dll");
    spawn(&Launch {
        env,
        program: out.join("f4x_loader.exe"),
        args: vec!["--game-dir".into(), wine_path(&env.game_dir), "--dll".into(), wine_path(&dll)],
        game_dir: env.game_dir.clone(),
        session: session.to_owned(),
        headless,
        extra_env: vec![("F4X_CONTROL".into(), "1".into())],
        workspace: root.to_owned(),
    })
}

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
}

/// Serialises game launches across processes (agents may run tasks in parallel): the
/// returned file holds an exclusive lock until dropped.
pub fn launch_lock() -> Result<std::fs::File, String> {
    let path = crate::profile::data_home().join("launch.lock");
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| e.to_string())?;
    if file.try_lock().is_err() {
        eprintln!("waiting for another f4x game session to finish ({})...", path.display());
        file.lock().map_err(|e| e.to_string())?;
    }
    Ok(file)
}

pub fn spawn(launch: &Launch<'_>) -> Result<Child, String> {
    let env = launch.env;
    let log = std::fs::File::create(launch.session.join("launch.log")).map_err(|e| e.to_string())?;
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
    let shared = format!("{}:{}", mount_root(&launch.workspace).display(), mount_root(&env.game_dir).display());
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

/// A fresh session directory under `target/f4x-sessions`.
pub fn new_session(root: &Path, label: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let session = root.join(format!("target/f4x-sessions/{stamp}-{label}"));
    let _ = std::fs::create_dir_all(&session);
    session
}

fn drive(session: &Path, guard: &mut GameGuard, result: &mut Value) -> Result<(), String> {
    let session_file = session.join("session.json");
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let info: Value = loop {
        if let Ok(text) = std::fs::read_to_string(&session_file)
            && let Ok(value) = serde_json::from_str(&text)
        {
            break value;
        }
        if guard.exited() {
            return Err("launcher exited before the runtime published a session".into());
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for session.json".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let port = info["port"].as_u64().ok_or("session.json has no port")? as u16;
    let token = info["token"].as_str().ok_or("session.json has no token")?.to_owned();

    let mut client = Client::connect(port)?;
    client.call("auth", json!({"token": token}))?;
    result["tcp"] = true.into();

    // The session file appears at injection time, before the game has even run its
    // entry point; wait until the main loop is pumping messages and keeps doing so.
    let injected_at = Instant::now();
    let mut first_pump = None;
    let mut last = 0;
    while Instant::now() < deadline {
        let ping = client.call("ping", json!({}))?;
        let pumps = ping["pump_calls"].as_u64().unwrap_or(0);
        if pumps > 0 && first_pump.is_none() {
            first_pump = Some((injected_at.elapsed(), pumps));
        } else if let Some((_, first)) = first_pump
            && pumps > first
        {
            last = pumps;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some((after, first)) = first_pump {
        result["first_pump_after_s"] = ((after.as_secs_f64() * 10.0).round() / 10.0).into();
        result["pump_calls"] = json!([first, last]);
        result["frame_hook"] = (last > first).into();
    }
    let hold = HOLD_SECONDS.load(std::sync::atomic::Ordering::Relaxed);
    if hold > 0 {
        let mut alive = 0;
        for _ in 0..hold {
            std::thread::sleep(Duration::from_secs(1));
            match client.call("ping", json!({})) {
                Ok(ping) => {
                    alive += 1;
                    result["last_pump_calls"] = ping["pump_calls"].clone();
                    result["last_uptime_ms"] = ping["uptime_ms"].clone();
                }
                Err(_) => break,
            }
        }
        result["held_s"] = alive.into();
        result["survived_hold"] = (alive == hold).into();
    }
    if let Ok(catalog) = client.call("catalog.write", json!({})) {
        result["natives"] = catalog["natives"].clone();
    }
    let _ = client.call("quit", json!({}));
    Ok(())
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

impl Client {
    fn connect(port: u16) -> Result<Self, String> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connect: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(Client { reader: BufReader::new(stream), writer, next_id: 1 })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.writer, "{request}").map_err(|e| format!("{method}: {e}"))?;
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(|e| format!("{method}: {e}"))?;
        let response: Value = serde_json::from_str(&line).map_err(|e| format!("{method}: bad reply {line:?}: {e}"))?;
        match response.get("error") {
            Some(error) => Err(format!("{method}: {error}")),
            None => Ok(response["result"].clone()),
        }
    }
}

fn session_log_contains(session: &Path, needle: &str) -> bool {
    std::fs::read_to_string(session.join("runtime.log")).is_ok_and(|log| log.contains(needle))
}

/// Owns the launched process group and makes sure nothing survives the run.
pub struct GameGuard {
    child: Option<Child>,
    /// Held for the whole session; see [`launch_lock`].
    _lock: Option<std::fs::File>,
    /// Our `F4X_SESSION_DIR` value: processes carrying it in their environment are ours.
    session_marker: String,
}

impl GameGuard {
    pub fn new(child: Child, session: &Path, lock: std::fs::File) -> Self {
        GameGuard { child: Some(child), _lock: Some(lock), session_marker: wine_path(session) }
    }

    /// Wait up to `timeout` for the launched process tree to exit on its own.
    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.exited() && session_processes(&self.session_marker).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        false
    }

    fn exited(&mut self) -> bool {
        self.child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some())
    }

    pub fn shutdown(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() && session_processes(&self.session_marker).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let group = format!("-{}", child.id());
        let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
        std::thread::sleep(Duration::from_secs(5));
        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
        // Only kill processes from this session; never the user's own game or Vortex.
        for pid in session_processes(&self.session_marker) {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        }
        let _ = child.wait();
    }
}

/// Pids whose environment has `F4X_SESSION_DIR=<marker>`.
fn session_processes(marker: &str) -> Vec<u32> {
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

impl Drop for GameGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

