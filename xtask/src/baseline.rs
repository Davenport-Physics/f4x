//! Spike S5: record what a host does to an F4SE plugin, using the probe.
//!
//! `cargo xtask baseline [--host f4se|f4x] [--profile NAME] [--headless] [--no-build] [--out FILE]`
//!
//! Real F4SE runs from an isolated profile (`f4se-baseline`), started through its own
//! `f4se_loader.exe`. The probe writes `probe.jsonl`; this task copies it to `--out`,
//! default `baselines/1.10.163/probe-<host>.jsonl` for the default profile and
//! `baselines/1.10.163/probe-<profile>.jsonl` for any other, so a run in another
//! profile never overwrites the reference baseline. F4SE's own logs are copied into
//! the session's `f4se-logs/`.

use std::time::Duration;

use f4x_host::{GameEnv, GameGuard, Host, Launch, Profile, build_windows, launch_lock, new_session, spawn};

use crate::workspace_root;

pub fn run(args: &[String]) -> Result<(), String> {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let host = Host::parse(&value("--host").unwrap_or_else(|| "f4se".into()))?;
    if host != Host::F4se {
        return Err("only --host f4se is implemented until the f4x plugin host exists (Phase 3)".into());
    }
    let default_profile = f4x_host::contract::DEFAULT_F4SE_PROFILE;
    let profile_name = value("--profile").unwrap_or_else(|| default_profile.into());
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
    // The probe is the only f4x plugin in the profile: links from earlier `f4x launch
    // --host f4se` runs (the agent, test plugins) are removed through the manifest.
    let probe = f4x_host::windows_out(&root).join("f4x_probe.dll");
    profile.set_plugins(&[probe])?;

    let env = base_env.with_prefix(profile.prefix());
    env.ensure_prefix()?;
    let session = new_session(&root, &format!("baseline-{}", host.name()));
    let lock = launch_lock()?;
    let spawned_at = std::time::SystemTime::now();
    let child = spawn(&Launch {
        env: &env,
        program: profile.game_dir().join("f4se_loader.exe"),
        args: Vec::new(),
        game_dir: profile.game_dir(),
        session: session.clone(),
        headless: flag("--headless"),
        extra_env: vec![("F4X_PROBE_EXIT_AFTER_S".into(), "8".into())],
        workspace: root.clone(),
        share: Vec::new(),
    })?;
    let mut guard = GameGuard::new(child, &session, Some(lock));
    let exited = guard.wait_exit(Duration::from_secs(150));
    guard.shutdown();
    for log in env.collect_f4se_logs(&session, spawned_at) {
        println!("saved {}", log.display());
    }

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
    let target = match value("--out") {
        Some(path) => std::path::PathBuf::from(path),
        None if profile_name == default_profile => root.join(format!("baselines/1.10.163/probe-{}.jsonl", host.name())),
        None => root.join(format!("baselines/1.10.163/probe-{profile_name}.jsonl")),
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&target, text).map_err(|e| e.to_string())?;
    println!("wrote {}", target.display());
    Ok(())
}
