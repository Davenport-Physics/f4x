//! Spike S1: launch the game under Proton with the f4x runtime injected, then check
//! injection, the control channel and the main-loop hook.
//!
//! `cargo xtask spike-launch [--headless] [--runs N] [--no-build] [--hold N]`
//!
//! The launch machinery (environment, lock, guard, client) lives in `f4x-host`.

use std::path::Path;
use std::time::{Duration, Instant};

use f4x_host::launch::game_running;
use f4x_host::{Client, GameEnv, GameGuard, Launch, build_windows, launch_lock, new_session, spawn, wait_for_session, wine_path};
use serde_json::{Value, json};

use crate::workspace_root;

const STARTUP_DEADLINE: Duration = Duration::from_secs(90);

pub fn run(args: &[String]) -> Result<(), String> {
    let headless = args.iter().any(|a| a == "--headless");
    let no_build = args.iter().any(|a| a == "--no-build");
    // `--hold N`: keep the game running N more seconds after the checks, pinging once a
    // second, and report how long it stayed alive.
    let hold = args
        .iter()
        .position(|a| a == "--hold")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
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
    let out = f4x_host::windows_out(&root);

    let mut failures = 0;
    for run in 1..=runs {
        let result = launch_once(&env, &root, &out, headless, run, hold);
        println!("{result}");
        let ok = ["injected", "tcp", "frame_hook"].iter().all(|k| result[k] == true);
        if !ok {
            failures += 1;
        }
    }
    if failures > 0 { Err(format!("{failures} of {runs} run(s) failed")) } else { Ok(()) }
}

fn launch_once(env: &GameEnv, root: &Path, out: &Path, headless: bool, run: u32, hold: u64) -> Value {
    let session = new_session(root, &format!("spike-{run}"));
    let started = Instant::now();

    let mut result = json!({
        "run": run, "headless": headless, "injected": false, "tcp": false, "frame_hook": false,
        "session": session.display().to_string(),
    });

    let lock = launch_lock().ok();
    let dll = out.join("f4x_runtime.dll");
    let child = match spawn(&Launch {
        env,
        program: out.join("f4x_loader.exe"),
        args: vec!["--game-dir".into(), wine_path(&env.game_dir), "--dll".into(), wine_path(&dll)],
        game_dir: env.game_dir.clone(),
        session: session.clone(),
        headless,
        extra_env: vec![("F4X_CONTROL".into(), "1".into())],
        workspace: root.to_owned(),
        share: Vec::new(),
    }) {
        Ok(child) => child,
        Err(error) => {
            result["error"] = error.into();
            return result;
        }
    };
    let mut guard = GameGuard::new(child, &session, lock);

    if let Err(error) = drive(&session, &mut guard, &mut result, hold) {
        result["error"] = error.into();
    }
    result["injected"] = session_log_contains(&session, "attached").into();
    guard.shutdown();
    result["seconds"] = ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0).into();
    result
}

fn drive(session: &Path, guard: &mut GameGuard, result: &mut Value, hold: u64) -> Result<(), String> {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let endpoint = wait_for_session(session, deadline, guard)?;
    let mut client = Client::connect_endpoint(&endpoint).map_err(|e| format!("connect: {e}"))?;
    client.set_timeout(Duration::from_secs(10));
    result["tcp"] = true.into();
    let mut call = |method: &str| client.call(method, json!({})).map_err(|e| format!("{method}: {e}"));

    // The session file appears at injection time, before the game has even run its
    // entry point; wait until the main loop is pumping messages and keeps doing so.
    let injected_at = Instant::now();
    let mut first_pump = None;
    let mut last = 0;
    while Instant::now() < deadline {
        let pumps = pump_count(&call("ping")?);
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
    if hold > 0 {
        let mut alive = 0;
        for _ in 0..hold {
            std::thread::sleep(Duration::from_secs(1));
            match call("ping") {
                Ok(ping) => {
                    alive += 1;
                    result["last_pump_calls"] = pump_count(&ping).into();
                    result["last_uptime_ms"] = ping["uptime_ms"].clone();
                }
                Err(_) => break,
            }
        }
        result["held_s"] = alive.into();
        result["survived_hold"] = (alive == hold).into();
    }
    // `catalog.natives` in the Phase 1 agent, `catalog.write` in the S1 spike runtime.
    if let Ok(catalog) = call("catalog.natives").or_else(|_| call("catalog.write")) {
        result["natives"] = catalog["natives"].clone();
    }
    let _ = call("quit");
    Ok(())
}

/// Main-loop iterations: `pumps` (f4x-proto) or the S1 spike runtime's `pump_calls`.
fn pump_count(ping: &Value) -> u64 {
    ping["pumps"].as_u64().or_else(|| ping["pump_calls"].as_u64()).unwrap_or(0)
}

/// The runtime's log mentions `needle`: `trace.jsonl` (Phase 1 agent) or `runtime.log`
/// (S1 spike runtime).
fn session_log_contains(session: &Path, needle: &str) -> bool {
    ["trace.jsonl", "runtime.log"]
        .iter()
        .any(|name| std::fs::read_to_string(session.join(name)).is_ok_and(|log| log.contains(needle)))
}
