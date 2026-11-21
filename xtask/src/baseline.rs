//! Spike S5: record what a host does to an F4SE plugin, using the probe.
//!
//! `cargo xtask baseline [--host f4se|f4x] [--profile NAME] [--headless] [--no-build]`
//!
//! Real F4SE runs from an isolated profile (`f4se-baseline`), started through its own
//! `f4se_loader.exe`. The probe writes `probe.jsonl`; this task copies it to
//! `baselines/1.10.163/probe-<host>.jsonl`.

use std::os::unix::fs::symlink;
use std::time::Duration;

use crate::env::GameEnv;
use crate::profile::{Host, Profile};
use crate::spike_launch::{GameGuard, Launch, build_windows, new_session, spawn};
use crate::workspace_root;

pub fn run(args: &[String]) -> Result<(), String> {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let host = Host::parse(&value("--host").unwrap_or_else(|| "f4se".into()))?;
    if host != Host::F4se {
        return Err("only --host f4se is implemented until the f4x plugin host exists (Phase 3)".into());
    }
    let profile_name = value("--profile").unwrap_or_else(|| "f4se-baseline".into());
    let root = workspace_root();
    let base_env = GameEnv::from_env();
    base_env.check()?;
    let profile = match Profile::load(&profile_name) {
        Ok(p) if p.host == host => p,
        _ => Profile::create(&profile_name, host, &base_env)?,
    };
    if !flag("--no-build") {
        build_windows(&root, &["f4x-probe"])?;
    }
    let probe = root.join("target/x86_64-pc-windows-msvc/release/f4x_probe.dll");
    let link = profile.plugins_dir().join("f4x_probe.dll");
    let _ = std::fs::remove_file(&link);
    symlink(&probe, &link).map_err(|e| e.to_string())?;

    let env = GameEnv { prefix: profile.prefix(), ..GameEnv::from_env() };
    env.ensure_prefix()?;
    let session = new_session(&root, &format!("baseline-{}", host.name()));
    let child = spawn(&Launch {
        env: &env,
        program: profile.game_dir().join("f4se_loader.exe"),
        args: Vec::new(),
        game_dir: profile.game_dir(),
        session: session.clone(),
        headless: flag("--headless"),
        extra_env: vec![("F4X_PROBE_EXIT_AFTER_S".into(), "8".into())],
        workspace: root.clone(),
    })?;
    let mut guard = GameGuard::new(child, &session);
    let exited = guard.wait_exit(Duration::from_secs(150));
    guard.shutdown();

    let recorded = session.join("probe.jsonl");
    let text = std::fs::read_to_string(&recorded).map_err(|_| {
        format!("no probe.jsonl in {} (did the host load the probe?)", session.display())
    })?;
    let events: Vec<serde_json::Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let messages: Vec<String> = events
        .iter()
        .filter(|e| e["event"] == "message")
        .map(|e| e["name"].as_str().unwrap_or("?").to_owned())
        .collect();
    println!("{} events; messages: {}", events.len(), messages.join(" → "));
    if !exited {
        println!("(game did not exit on its own; killed)");
    }
    let out = root.join("baselines/1.10.163");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let target = out.join(format!("probe-{}.jsonl", host.name()));
    std::fs::write(&target, text).map_err(|e| e.to_string())?;
    println!("wrote {}", target.display());
    Ok(())
}
