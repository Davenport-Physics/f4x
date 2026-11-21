//! `f4x`: launch, drive and test Fallout 4 with the f4x in-game agent, from the shell or
//! (`f4x mcp`) from an agent.
//!
//! Every command prints human-readable text by default and one JSON object with
//! `--json` (`logs --follow` and `rpc --listen` print one JSON object per line).
//! Session commands default to the newest running session under `target/f4x-sessions`.

mod mcp;
mod start;
mod testing;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use f4x_host::contract::{DEFAULT_STARTUP_TIMEOUT_S, LaunchOptions};
use f4x_host::profile::Host;
use f4x_host::session::{self, LogFollower, SessionDir, format_log_line, level_rank, passes_level, tail_lines};
use f4x_host::{signals, supervisor, workspace_root};
use f4x_proto::{Screenshot, ScreenshotParams, method};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "f4x", version, about = "Launch, drive and test Fallout 4 with the f4x in-game agent")]
struct Cli {
    /// Machine-readable output: one JSON object (JSON lines for streams).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a game session and print its directory. The session keeps running (owned
    /// by a detached supervisor) until `f4x quit`, a crash or --lifetime.
    Launch(LaunchArgs),
    /// Call a method of the in-game agent.
    Rpc(RpcArgs),
    /// Show a session's state.
    Status(SessionArg),
    /// Print a session's log.
    Logs(LogsArgs),
    /// Save a screenshot of the next frame.
    Screenshot(ScreenshotArgs),
    /// Quit a session and wait until it released the launch lock.
    Quit(QuitArgs),
    /// Launch, run the in-game tests, quit. Exit code 0 only if every test passed.
    Test(TestArgs),
    /// Serve the Model Context Protocol on stdin/stdout.
    Mcp,
    #[command(name = "__supervise", hide = true)]
    Supervise { session: PathBuf },
}

#[derive(Args, Clone)]
struct HostArgs {
    /// f4x: our loader + runtime. f4se: real F4SE with the agent as a plugin.
    #[arg(long, default_value = "f4x", value_parser = ["f4x", "f4se"])]
    host: String,
    /// Profile under ~/.local/share/f4x/profiles (default: the real install with the
    /// test prefix for f4x; f4se-baseline for f4se).
    #[arg(long)]
    profile: Option<String>,
    /// Run under gamescope's headless backend.
    #[arg(long)]
    headless: bool,
    /// A test plugin DLL to load (repeatable).
    #[arg(long = "plugin", value_name = "DLL")]
    plugins: Vec<PathBuf>,
    /// Build the Windows crates first (cargo xwin).
    #[arg(long)]
    build: bool,
    /// Seconds the agent gets to publish session.json.
    #[arg(long, value_name = "S", default_value_t = DEFAULT_STARTUP_TIMEOUT_S)]
    startup_timeout: u64,
    /// Seconds to queue behind another session (default: forever).
    #[arg(long, value_name = "S")]
    lock_timeout: Option<u64>,
}

impl HostArgs {
    fn options(&self, lifetime: u64) -> Result<LaunchOptions, String> {
        let mut options = LaunchOptions::new(Host::parse(&self.host)?);
        options.profile = self.profile.clone();
        options.headless = self.headless;
        options.plugins = self.plugins.clone();
        options.build = self.build;
        options.startup_timeout_s = self.startup_timeout;
        options.lifetime_s = Some(lifetime);
        Ok(options)
    }

    fn lock_timeout(&self) -> Option<Duration> {
        self.lock_timeout.map(Duration::from_secs)
    }
}

#[derive(Args)]
struct LaunchArgs {
    #[command(flatten)]
    host: HostArgs,
    /// Set F4X_TEST_MODE=1 (test-only agent methods such as debug.crash).
    #[arg(long)]
    test_mode: bool,
    /// Also wait until the game's main loop runs.
    #[arg(long)]
    main_loop: bool,
    /// Stay in the foreground until the game exits (Ctrl-C stops it).
    #[arg(long)]
    wait_exit: bool,
    /// Kill the session after this many seconds (0: never).
    #[arg(long, value_name = "S", default_value_t = 3600)]
    lifetime: u64,
}

#[derive(Args)]
struct SessionArg {
    /// Session directory (path, or name under target/f4x-sessions). Default: the
    /// newest running session.
    #[arg(long, value_name = "DIR")]
    session: Option<String>,
}

#[derive(Args)]
struct RpcArgs {
    method: String,
    /// JSON params (default {}).
    params: Option<String>,
    #[command(flatten)]
    session: SessionArg,
    /// Seconds to wait for the reply.
    #[arg(long, value_name = "S", default_value_t = 30.0)]
    timeout: f64,
    /// Then print notifications for this many seconds (0: until the game exits).
    #[arg(long, value_name = "S")]
    listen: Option<f64>,
}

#[derive(Args)]
struct LogsArgs {
    #[command(flatten)]
    session: SessionArg,
    /// Keep printing new lines until the session ends.
    #[arg(long, short)]
    follow: bool,
    /// Minimum level (trace, debug, info, warn, error); structured trace only.
    #[arg(long, value_name = "L")]
    level: Option<String>,
    /// Lines of history to print first.
    #[arg(long, short = 'n', default_value_t = 200)]
    lines: usize,
    /// trace, runtime, loader, launch or supervisor (default: trace if present, else runtime).
    #[arg(long)]
    source: Option<String>,
}

#[derive(Args)]
struct ScreenshotArgs {
    #[command(flatten)]
    session: SessionArg,
    /// Copy the PNG here.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct QuitArgs {
    #[command(flatten)]
    session: SessionArg,
    /// Kill the game instead of asking it to quit.
    #[arg(long)]
    force: bool,
    /// Seconds to wait for a clean exit before killing.
    #[arg(long, value_name = "S", default_value_t = 45)]
    wait: u64,
}

#[derive(Args)]
struct TestArgs {
    /// Only tests whose name contains this (same as --filter).
    #[arg(value_name = "FILTER")]
    positional_filter: Option<String>,
    #[command(flatten)]
    host: HostArgs,
    /// Only tests whose name contains this.
    #[arg(long, value_name = "S")]
    filter: Option<String>,
    /// Leave out tests whose name contains this (repeatable).
    #[arg(long, value_name = "S")]
    skip: Vec<String>,
    /// Per-test timeout in seconds.
    #[arg(long, value_name = "S", default_value_t = testing::DEFAULT_TEST_TIMEOUT_S)]
    timeout: u64,
    /// Seconds to wait for the main loop.
    #[arg(long, value_name = "S", default_value_t = 120)]
    main_loop_timeout: u64,
    /// Write JUnit XML here.
    #[arg(long, value_name = "FILE")]
    junit: Option<PathBuf>,
    /// Launch-test-quit this many times.
    #[arg(long, value_name = "N", default_value_t = 1)]
    runs: u32,
    /// Kill a run's session after this many seconds.
    #[arg(long, value_name = "S", default_value_t = 1800)]
    lifetime: u64,
}

/// A command's result: what to print, and whether it succeeded.
struct Report {
    ok: bool,
    value: Value,
    human: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match cli.command {
        Command::Supervise { session } => return ExitCode::from(supervisor::main(&session) as u8),
        Command::Mcp => {
            return match mcp::serve(workspace_root()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("f4x mcp: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Launch(args) => launch(args, json),
        Command::Rpc(args) => rpc(args, json),
        Command::Status(args) => status(args),
        Command::Logs(args) => logs(args, json),
        Command::Screenshot(args) => screenshot(args),
        Command::Quit(args) => quit(args),
        Command::Test(args) => test(args, json),
    };
    match result {
        Ok(report) => {
            if json && !report.value.is_null() {
                let mut value = report.value;
                if let Some(object) = value.as_object_mut() {
                    object.entry("ok").or_insert(report.ok.into());
                }
                println!("{value}");
            } else if !report.human.is_empty() {
                println!("{}", report.human);
            }
            if report.ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(message) => {
            if json {
                println!("{}", json!({ "ok": false, "error": message }));
            } else {
                eprintln!("error: {message}");
            }
            ExitCode::FAILURE
        }
    }
}

fn find_session(arg: &SessionArg, live: bool) -> Result<SessionDir, String> {
    let root = workspace_root();
    match &arg.session {
        Some(given) => mcp::resolve(&root, given),
        None if live => session::latest_live(&root).ok_or_else(|| "no running f4x session (start one with `f4x launch`)".into()),
        None => session::latest(&root).ok_or_else(|| "no f4x sessions yet".into()),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn launch(args: LaunchArgs, json: bool) -> Result<Report, String> {
    let mut options = args.host.options(args.lifetime)?;
    options.test_mode = args.test_mode;
    signals::install();
    let mut started = start::start(&options, args.host.lock_timeout()).map_err(|e| e.to_string())?;
    let mut value = json!({
        "session": started.dir.path.display().to_string(),
        "endpoint": started.endpoint.public(),
        "supervisor_pid": started.supervisor.id(),
        "host": options.host.name(),
        "headless": options.headless,
    });
    if args.main_loop {
        match start::wait_for_main_loop(&started, Duration::from_secs(120)) {
            Ok(state) => value["state"] = state,
            Err(e) => {
                value["error"] = format!("the main loop did not start: {e}").into();
                return Ok(Report { ok: false, human: pretty(&value), value });
            }
        }
    }
    if !args.wait_exit {
        return Ok(Report { ok: true, human: started.dir.path.display().to_string(), value });
    }
    // Foreground: show the session, then wait for the supervisor to finish.
    if json {
        println!("{value}");
    } else {
        println!("{}", started.dir.path.display());
    }
    let mut stop_sent = false;
    let code = loop {
        if let Ok(Some(status)) = started.supervisor.try_wait() {
            break status.code();
        }
        if signals::interrupted() && !stop_sent {
            eprintln!("f4x: stopping the session");
            started.dir.request_stop();
            stop_sent = true;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = started.dir.supervisor();
    let exit = state.as_ref().and_then(|s| s.exit.clone());
    let clean = code == Some(0);
    let value = json!({ "session": started.dir.path.display().to_string(), "exit": exit, "crashes": started.dir.crash_summaries() });
    Ok(Report { ok: clean, human: format!("session ended: {}", exit.map(|e| json!(e).to_string()).unwrap_or_default()), value })
}

fn rpc(args: RpcArgs, json: bool) -> Result<Report, String> {
    let params: Value = match &args.params {
        Some(text) => serde_json::from_str(text).map_err(|e| format!("params are not JSON: {e}"))?,
        None => json!({}),
    };
    let dir = find_session(&args.session, args.session.session.is_none())?;
    signals::install();
    let mut client = dir.connect().map_err(|e| format!("{}: {e}", dir.path.display()))?;
    let reply = client.call_timeout(&args.method, params, Duration::from_secs_f64(args.timeout.max(0.1)));
    let result = match reply {
        Ok(result) => result,
        Err(error) => {
            let value = json!({ "error": error, "session": dir.path.display().to_string() });
            return Ok(Report { ok: false, human: format!("error: {error}"), value });
        }
    };
    if let Some(seconds) = args.listen {
        if json {
            println!("{}", json!({ "ok": true, "result": result }));
        } else {
            println!("{}", pretty(&result));
        }
        let deadline = (seconds > 0.0).then(|| Instant::now() + Duration::from_secs_f64(seconds));
        while deadline.is_none_or(|d| Instant::now() < d) {
            match client.next_notification(Duration::from_millis(500)) {
                Ok(Some(note)) if json => println!("{}", json!(note)),
                Ok(Some(note)) => println!("{} {}", note.method, note.params),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        return Ok(Report { ok: true, value: json!({ "done": true }), human: String::new() });
    }
    Ok(Report { ok: true, human: pretty(&result), value: json!({ "result": result }) })
}

fn status(args: SessionArg) -> Result<Report, String> {
    let dir = find_session(&args, false)?;
    let status = dir.status(true);
    let mut human = format!("session  {}\nlive     {}", dir.path.display(), status["live"]);
    if let Some(state) = dir.supervisor() {
        human += &format!("\nphase    {:?} (supervisor pid {})", state.phase, state.pid);
        if let Some(exit) = &state.exit {
            human += &format!("\nexit     {}", json!(exit));
        }
    }
    if !status["endpoint"].is_null() {
        human += &format!("\nagent    {}", status["endpoint"]);
    }
    for key in ["state", "ping", "heartbeat", "rpc_error"] {
        if !status[key].is_null() {
            human += &format!("\n{key:<8} {}", status[key]);
        }
    }
    for crash in status["crashes"].as_array().into_iter().flatten() {
        human += &format!("\ncrash    {}\n         {}", crash["file"].as_str().unwrap_or(""), crash["summary"].as_str().unwrap_or(""));
    }
    Ok(Report { ok: true, value: status, human })
}

fn logs(args: LogsArgs, json: bool) -> Result<Report, String> {
    let dir = find_session(&args.session, false)?;
    let path = dir.log_path(args.source.as_deref())?;
    let min = match &args.level {
        Some(level) => Some(level_rank(level).ok_or_else(|| format!("unknown level {level:?}"))?),
        None => None,
    };
    let print = |line: &str| {
        if !passes_level(line, min) {
            return;
        }
        if !json {
            println!("{}", format_log_line(line));
        } else if serde_json::from_str::<Value>(line).is_ok_and(|v| v.is_object()) {
            println!("{line}");
        } else {
            println!("{}", json!({ "line": line }));
        }
    };
    let (history, end) = if path.exists() {
        let read = if min.is_some() { args.lines * 20 } else { args.lines };
        tail_lines(&path, read)?
    } else if args.follow {
        (Vec::new(), 0)
    } else {
        return Err(format!("{} does not exist", path.display()));
    };
    let history: Vec<&String> = history.iter().filter(|l| passes_level(l, min)).collect();
    for line in &history[history.len().saturating_sub(args.lines)..] {
        print(line);
    }
    if args.follow {
        signals::install();
        let mut follower = LogFollower::at(path.clone(), end);
        let mut last_check = Instant::now();
        loop {
            for line in follower.poll() {
                print(&line);
            }
            if signals::interrupted() {
                break;
            }
            if last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                if !dir.is_live() {
                    for line in follower.poll() {
                        print(&line);
                    }
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    // The lines are the output; no trailing report.
    Ok(Report { ok: true, value: Value::Null, human: String::new() })
}

fn screenshot(args: ScreenshotArgs) -> Result<Report, String> {
    let dir = find_session(&args.session, args.session.session.is_none())?;
    let mut client = dir.connect().map_err(|e| e.to_string())?;
    let reply = client
        .call_timeout(method::SCREENSHOT, json!(ScreenshotParams::default()), Duration::from_secs(20))
        .map_err(|e| format!("screenshot: {e}"))?;
    let shot: Screenshot = serde_json::from_value(reply.clone()).map_err(|e| format!("unexpected reply {reply}: {e}"))?;
    let mut file = dir.file(&shot.file);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !file.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(output) = &args.output {
        std::fs::copy(&file, output).map_err(|e| format!("copying {} to {}: {e}", file.display(), output.display()))?;
        file = output.clone();
    }
    let value = json!({ "file": file.display().to_string(), "width": shot.width, "height": shot.height, "frame": shot.frame });
    Ok(Report { ok: true, human: file.display().to_string(), value })
}

fn quit(args: QuitArgs) -> Result<Report, String> {
    let dir = find_session(&args.session, true)?;
    let report = dir.quit(args.force, Duration::from_secs(args.wait));
    let ok = report["exited"] == true;
    let human = format!(
        "{} {} in {} s{}",
        dir.path.display(),
        if ok { "exited" } else { "is STILL RUNNING" },
        report["seconds"],
        if report["forced"] == true { " (killed)" } else { "" }
    );
    Ok(Report { ok, human, value: report })
}

fn test(args: TestArgs, json: bool) -> Result<Report, String> {
    let options = args.host.options(args.lifetime)?;
    let host = options.host.name().to_owned();
    signals::install();
    let plan = testing::Plan {
        launch: options,
        lock_timeout: args.host.lock_timeout(),
        selection: testing::Selection { filter: args.filter.clone().or(args.positional_filter.clone()), skip: args.skip.clone() },
        timeout_s: args.timeout,
        main_loop_timeout: Duration::from_secs(args.main_loop_timeout),
        runs: args.runs.max(1),
    };
    let runs = testing::run_plan(&plan, |line| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    });
    if let Some(path) = &args.junit {
        let xml = f4x_host::junit::render(&testing::junit_suites(&runs, &host));
        std::fs::write(path, xml).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let ok = runs.iter().all(testing::Run::ok);
    let failed_runs = runs.iter().filter(|r| !r.ok()).count();
    let human = format!("{} run(s): {}", runs.len(), if ok { "all passed".to_owned() } else { format!("{failed_runs} FAILED") });
    let value = json!({ "host": host, "runs": runs.iter().map(testing::Run::to_json).collect::<Vec<_>>(), "junit": args.junit });
    Ok(Report { ok, human, value })
}
